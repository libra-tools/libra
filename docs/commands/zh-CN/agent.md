# `libra agent`

`libra agent push --mega2 [--remote <name>] [--new-stream]` 把 `refs/libra/traces` 推到 Mega2 TraceRepo。来源是仓库的 `libra.repoid`。第一次 epoch 在任何网络尝试之前写入，失败重试会复用它。只有 `--new-stream` 会更换 epoch。`--mega2 --force-rewrite` 在打开连接之前被拒绝；非快进不会变成 force、新 epoch，也不会改推普通代码 remote。SSH 使用派生的 `traces@` 登录名，不改已保存的 remote URL。remote 没有 URL 时命令在本地停止，并打印已保存的 epoch。推送已经成功但读不回 scope UUID 时，成功结论保持不变，收据记为 `scope_resolution_pending`。

Grok 的 `support_wave` 是 `grok`，不是 `first_batch`，可安装生命周期 hook，不是 `review` 或 `investigate` 启动器。**自动 Thinking 采集当前暂停。** 生命周期由共享管线记录；hook 解析不读取会话日志，也不写 `GROK_HOME/libra-capture`。已有 `updates.jsonl` 与 `chat_history.jsonl` 读取器尚未接入授权 snapshot/checkpoint writer；离线 observer JSON 是临时状态，不是 Libra checkpoint。后续目标是 transcript 保留 Thinking 正文和未证明的 `encrypted_content`，不解密；当前合成读取器测试不证明真实 checkpoint 已含这些数据。默认 `checkpoint show` 不打印 Thinking 明文；显式 transcript 路径只能返回实际已保存的内容。

Hermes 已支持本地采集，并且可以安装 hook。`support_wave=hermes`，不是 `first_batch`，也不能启动 `review` 或 `investigate`。迁移 `2026103001` 接受 `agent_kind=hermes`，并保留此前已接受的全部 kind，包括 `grok` 与 `opencode`；它不增加 `pi`（见下一段）。内部 reader 接受官方 JSONL：`active` 与 `compacted` 都为假的行不进入，非空 `reasoning` 或 `reasoning_content` 字符串会保留。`reasoning_details`、`codex_reasoning_items`、`codex_message_items` 保持原始字符串。该导出不可用时，Libra 只读打开 SQLite，不迁移 provider 数据库。`libra agent list` 会列出 `hermes`。安装只改写带有 `libra capture` 标记且 sha256 与记录一致的 `HERMES_HOME/plugins/libra-hermes-capture`。无标记或已漂移的文件保持原样。PATH 上没有 Hermes v0.22.5 `bf0203a` 时，不带名字的 `enable` 会跳过它，显式 `enable hermes` 不写文件。review 与 investigate 启动保持关闭。`cloud sync` 仍在任何远程写入之前拒绝 `hermes`（`LBR-CONFLICT-002`，`--force` 亦然）：存在这类仅本地采集会话时会持续拒绝，直到后续版本为该种类提供云支持，期间提示改用外部工具备份整个仓库（见 [`cloud`](cloud.md)）。`libra agent import --path <session-id>.jsonl --agent hermes --yes` 把一份官方导出写入本地仓库，不扫描 Hermes home。消息按 id 排序。thinking 字符串作为未证明数据附带保留，不写入 assistant 回答。缺少的 usage 保持未知，不写成 0。非 default profile、非 cli 来源和无证 parent 会被拒绝。重复导入不会再写一个 checkpoint。有界 snapshot 把这份 thinking 标成未证明且 partial，保留文本超过 8 MiB 时拒绝该 frame，不把它当成 assistant 回答。合成导出不会通过真实会话获取门。live 桥只在持有 export-job lease 时运行 `hermes sessions export --format jsonl --session-id <id> <output-file>`，总期限 2 秒。`libra agent rpc trust hermes` 只固定该可执行文件，不接受 OpenCode trust。超时是 partial，可以再试。`libra agent doctor` 把这条桥标成 source-qualified，不声称 native origin。`libra agent hooks hermes observe` 只接受已证明的 observer。`on_session_end` 记录一轮，不关闭会话。`on_session_finalize` 才是拆除，并使用共用的 capture finalizer。control hook 和 subagent 事件不注册。plugin 只转发身份字段，不读 Hermes 数据库。hook 安装保持关闭。

Pi 注册为仓库 identity，只有显式 import 会写入其会话内容。迁移 `2026103101` 在专用迁移连接上为仓库 `agent_session.agent_kind` CHECK 增加 `pi`，并保留此前已接受的全部 kind，包括 `grok`、`opencode` 与 `hermes`。`pi` 为 `supported=false`：`agent list` 不列出它，`add`/`enable` 返回 unsupported 错误，`disable`/`remove` 因没有可移除的 hook 而拒绝。发现、实时采集、hook 安装以及 review 或 investigate 启动均保持关闭。Libra 自有的 Pi extension（`.pi/extensions/libra-hooks.ts`）只在 Pi 报告项目受信时把观察事件转发给 `libra agent hooks pi observe`；receiver 只为会话开始与 tool 事件保存生命周期元数据，不含提示、消息、tool 输入输出或 thinking，不保存任何 Pi 内容。`agent enable` 暂不安装该 extension。写入 `pi` 会话内容的唯一方式是显式、仅本地的 import：`agent import --agent pi --path <timestamp>_<session-id>.jsonl --yes`。它只在取得同意后读取你指定的那一个文件一次，不扫描 Pi 的会话目录，也不上传任何内容。路径不得含 `..`，文件名须带有会话 id，头部（第一行）须有相同的 `id`，且 `cwd` 位于本仓库之内，否则不写入任何内容。字节先脱敏，再经 Pi worker frame 加界。被接受的会话通过与其它 provider 相同的 import 协调器与 claim 存储，按每个 turn 一个 checkpoint 写入。其 snapshot 标为 partial 且 unproven。turn 保存提示与回答；每个 thinking 块作为未证明的保留数据存于其旁，文本、签名与 redacted 标记均与脱敏后的会话所写完全一致，且不并入回答。Pi 不写会话结束记录，因此会话保持 active，最后一个 turn 为 incomplete。再次导入同一会话时，已写入的 turn 不会产生新的 checkpoint。条目超过 200000 个、deadline 已到期或被 Pi reader 拒绝的会话不写入任何内容。`pi` 现在是内置 slug，外部 `libra-agent-pi` 可执行文件不会被信任或代替它调用（`LBR-AGENT-006`）。`cloud sync` 与 `hermes` 一样，在任何远程写入之前拒绝本地 `pi` 会话。存在 `pi` 会话时拒绝回滚该迁移，并保留这些行。早于该迁移的 Libra binary 拒绝打开带有该迁移的仓库，不修改任何文件，并提示安装更新的 binary。

> **coverage-v2 源码版本：** 下文 coverage-v2 说明对应 Libra 0.30.50 源码线及后续兼容版本，不宣告稳定版已发布；可用性以[官方发布](https://github.com/libra-tools/libra/releases)及 `libra --version` 为准。其它章节保留各自的版本要求；coverage-v2 不证明 native 来源，也不准入原生 reasoning artifact。

管理 Claude Code、Codex 与 OpenCode 的外部代理捕获。


## Codex 父子会话用量快照（CX-26 开发中）

未发布源码可为 UUID Codex 会话的 checkpoint 增加 `usage_attribution`。
`scope=session_snapshot`、`as_of=live_capture` 或 `as_of=import_pass`
表示本次观察到的整会话快照，不是计费增量，不能跨 checkpoint 相加；
同一次 import 的父 checkpoint 使用相同观察值。
`parent_token_usage`、`child_token_usage` 保留已知计数，缺失值为 null；
缓存、reasoning、total 等可选计数仅在全部参与项均有值时求和。

归属读取会复验当前 scope、claim、revision、digest、link 与 capture
incarnation，最多列出 16 个 source/revision/checkpoint 引用。引用中的
`parent` 在当前线程下为 null，嵌套时为父 source commitment；原生身份继续
保留在独立普通 child 来源中。partial
状态下的来源计数是有界观察，不代表已穷尽目录。未知、busy、过期、损坏或
缺失来源不能当成实测零；用量未知不妨碍独立导出 child thinking。
历史非 UUID 标签和其它 provider 不增加该字段。

只有可用且穷尽的后代扫描、当前耐久 claim 及完整父子用量均成立时，
才给出 `children_coverage=complete` 与经溢出检查的总量。其它情况保留
`partial=true`、`aggregate_token_usage=null`、
`aggregate_includes_child_usage=false` 和已知小计。import 先保存 child，
再生成父 metadata；一侧失败仍保留另一侧已写入的数据及进度。
重放不刷新旧父快照，也不单因 child 成长新增父 checkpoint。

`excluded_legacy_count` 仅统计通过 commit/tree/metadata 角色链及来源
commitment 证明的旧采集实例；无法证明的旧记录仍为 unknown。
差集最多检查 16 个 claim 和一个饱和见证，每个角色对象最多 64 KiB、
合计 1 MiB。历史记录过多、父 spawn 证据不可用（含其 1 MiB 上限）或
发现窗口耗尽时，coverage 仍可为 partial。
不回写旧 checkpoint 或已封存的恢复 artifact。含该可选 metadata 的新
artifact 需要支持它的版本进行恢复；使用旧版本时应保留原 artifact。

凭据脱敏可能吞掉记录或嵌套用量字段，即使保留的 JSON 仍有效，用量也会
标记 partial 或 unavailable。可读 thinking 与 opaque 值仍按既有普通
数据合同保留和导出。新增 Codex 功能测试仅在本机显式启用 feature 和
ignored 用例运行，不加入 CI。

## Codex ordinary thinking retention（CX-15 开发中）

本开发源码中，经同意执行 `agent import --path <rollout.jsonl> --agent codex --yes`
会保留 Codex `response_item` reasoning 的实际可用值，包括 `summary_text`、
`reasoning_text`、`encrypted_content` 及未知关联 JSON 字段。JSON 字符串先解码
转义，再按既有凭据模式脱敏；其它值保留，包括大整数和精确小数。缺失、null、
空值与非空值分别记录。来源状态为 **Unproven**，opaque/不支持的数据或无法确定
turn 归属时为 **Partial**。模型提供的摘要不等于完整内部推理。

默认 `agent checkpoint export <id>` 可取回这些普通值，无需 `--raw --allow-raw`。
导出的 JSONL 可能在既有 per-turn 语义投影之后带有保留的 source 记录，消费者应
逐行读取。已覆盖的 turn 仅 opaque/关联值变化时，导入会新增补充 checkpoint；
相同保留数据重放不重复写入，旧 checkpoint 和 V1 语义 claim 不改写。独立 V2
claim 迁移属于 CX-05，不因保存普通值获得 native artifact 或解密权限。

侧车只含计数、显式 turn 的 token 快照和 partial 状态，不保存 thinking 正文或
密文。大工具输出及损坏尾部不再使前面有效 thinking 整份丢失；无法解码的字符串
只在局部脱敏，并保留结构上的 partial。既有 consent、仓库/session 关联、来源
字节上限及 deadline 继续有效。这些值属于 checkpoint，会随导出、agent push 或
cloud 备份一起处理；当前没有单独排除 thinking 的开关。

CX-15 使用合成文件验证真实 import/store/export，并非真实模型验收；系统 Codex
hook 安装、rollout 定位和 TUI 贪吃蛇 smoke 仍有各自的后续计划门。

侧车保存 checkpoint 写入时的 provider 显式 turn 快照（`turn_ordinal_namespace=codex_explicit_turn`），不将该序号当作 V1 checkpoint 的归属。仅 metadata 变化不新增 checkpoint。完整 turn 表位于共享 `redaction_report.json` 对象，各 checkpoint metadata 仅带有界汇总；统计或保留准备失败明确标 partial / `retention_unavailable`，保留既有 V1 投影。

每次导入最多先追加 16 个 retention supplement，再处理新 turn；未处理完会返回包含已保存进度的 partial，释放本轮所有权，重跑可续传。新副本 ID 通过仓库密钥绑定语义 digest、完整性与保留值，同内容在 repack 后重放也不重复写入。完整 traces repack 后新增内容仍受已有 packed-parent writer 限制；请保留源文件和仓库以便修复。本机 packed transcript 用例仅验证合法混合存储，不代表整个 traces 的 packed-parent 已支持。

## Codex 轮次归属（CX-05 开发中）

首条非空持久记录固定整份 source snapshot 的 typed preview codec：`session_meta` 中 `cli_version: 0.162.1` 选择当前结构化 codec，其他或缺失 header 保留 legacy codec。恢复旧会话后追加新标记不能重新解释旧前缀。必须提供完整 source snapshot，孤立 tail 不是等价 coverage 输入。选择依据的 header 保留在 ordinary source；后续自动 writer 激活须将 codec 选择持久绑定 session，不能在重试时重新猜测。

当前 codec 按 `turn_context`、生命周期标记或已接受输入元数据中的有效 `turn_id` 分组。同轮 steering 共用一组；带作用域回答和迟到输入镜像不回退活动流上下文。无作用域或匹配当前 turn 的终态标记关闭它。终态 scope 为畸形值时不关闭流，并降低投影完整性。可选 turn_context 缺 ID 时降低投影完整性：只保留已有生命周期证明的 scope，否则清除上下文；明确 null 或无效 ID 仍清除。首次 raw 输入若明确属于旧 turn，保留在旧组并标时序 partial，后续无 scope 输出仍用活动 turn；活动 scope 为空或相同时才由新 raw 输入绑定。缺失或无效 turn ID 使用 incomplete 的 ordinal 兼容键，依据分组创建次序，与紧凑的投影 ordinal 分开；前方空组后续变为可见时不改变这些键。

人类输入依据 producer `content_item_kinds` 和 accepted-input 事件判定。developer/system、继承历史和外来 thread 不创建人类轮次；形似上下文包装的真实文本保留。同一 surface 上的重复输入仍重复；每轮出现序号配对 event/raw/completed 镜像。raw/client/completed item 的 ID 命名空间独立，缺 ID 或 ID 不等使配对保持 partial，因此实际 current 会话通常为 partial。非文本内容、冲突和未配对事件也保持 partial。只有带注释 raw 或有 scope 的 completed input 能建立用户记录；无自身 scope 的 legacy accepted-input 事件只佐证匹配输入，不能独立建立人类轮次；不匹配的镜像不能在更直接来源旁虚构用户记录。Legacy role-only 投影可能含注入上下文，不证明人类来源。既有 ordinary retention 在凭据扫描后继续保留 source 值。

此纯数据路径复用既有 response codec 和 V1/V2 selector：typed 可读 reasoning 选 V2，普通记录仍选 V1。历史 V1 normalization、ordinary retention 写入及旧 claim 保持原 key/digest。Codex 自动 writer 激活仍待后续接线。Current codec 标为 V1 的普通 preview 记录不能直接写入 legacy V1 claim 域：激活时须显式采用现有 ordinary V2 域，或把已有 V1 claim 的 session 固定 legacy codec，并持久绑定选择；显式 prepared ordinary V2 import 已使用共享重放门。reasoning sidecar 保留独立的 `codex_explicit_turn` ordinal 命名空间，不能按位置关联 coverage ordinal。

Snapshot 既有总上限为 16 MiB。当前 codec 的分组字节也限 16 MiB，每条 JSONL 限 1 MiB，turn、输入出现项和 identity 集合各限 4096。大 patch/tool output 或内联 image data URL 可能使此纯 preview 不可用。精确版本 pin 的兼容范围和大记录可用性都须在自动 writer 激活前复审。超限或 deadline 到期使 preview 失败，空的或无法归属的 partial 组保留 partial 信号；可归属损坏只降低对应组，未归属损坏降低整份 snapshot。普通 retention 沿用原路径。源码及合成验证不证明真实模型采集或 native 来源。

## Codex rollout 元数据与能力（CX-04/CX-14 开发中）

当前源码以 Codex 0.162.1 schema 解析 envelope rollout；其它版本的未知记录保持 partial。`token_usage_record` 按 response 去重，compaction 的重复快照不再计数；旧累计 token 跨 turn 差分，无解释的倒退标为 partial，显式上下文边界的重置也保留限定。与精确记录一致的旧观察不重复累加。cached tokens 聚合缓存读写；解析器中的缺失或无效 usage 为 unknown，不是实测零值。

envelope 的 prompt 依事件侧已接受输入及生产者 `content_item_kinds` 判定，真实用户写出的上下文标签原样保留。未标注来源或标注数量不符的 user-role response 正文仍保存在普通源数据中，但不冒充用户 prompt。优先按共享 item identity 配对镜像并选 completed 输入；否则只在相同 turn、相同投影正文下按出现次数跨来源配对。同一来源的重复输入保留；无共享 identity 的配对只是 partial 下界。首个 `session_meta` 绑定 owner thread；其它 header 导致 partial，foreign 记录不计入其 usage，foreign header 后所有无 scope 的记录均不强行归属，直到 owner header 再次出现；显式属于 owner thread 的记录仍可投影。model 取最新显式 `turn_context` model。只有带非空 call/item identity 的结构化 patch completed 成功记录才报告修改路径；提议文本、失败或部分执行不算已修改。路径仅作词法投影，相对记录的工作目录，该目录可能是仓库子目录。

源读取上限 16 MiB，单条 JSONL 的完整解析上限 1 MiB，保留的元数据与 identity 状态均有界。对超长已知记录，能力投影可仅读取有界的 type/role/scope 骨架并忽略正文，使大图片或工具输出不抑制无关的精确 usage、已完成文件变更；直接受影响或无法分类的投影仍报单条记录超限，跳过正文时底层 parser 标记 partial。纯空白行不构成记录；hook helper 继续使用更小的既有限额。未知变体、重复 JSON key、损坏末行、无成功状态证明的旧 patch 及预算耗尽返回 partial 和不含正文的警告。旧平铺导出保留有界元数据的兼容 reader，超长、坏行或无法分类的行跳过并限定，不抹掉其它可用行；旧平铺 role-only 用户投影可能包含 host 注入上下文，不是人类请求的证据；混合格式只保证部分结果，envelope 文件内的平铺行不作投影。这不授予 raw 保存权限、不改写 coverage claim，也不代表真实 thinking 验收。Codex adapter 已暴露 transcript analyzer，`libra agent list --json` 的能力行反映已接线的结构化 completed 文件变更接口。usage 不可用或 usage 自身一致性检查已证明有缺口时，token calculator 返回可操作错误，不伪造零值，也不返回已知不完整总量；带限定的显式 reset 仍可能给出下界。对最早 usage 为旧累计观察的源，非零 offset 后续窗口一直使用累计 counter 差分，即使稍后出现精确记录；缺少新的累计观察时仍为 unknown，防止迟到的精确镜像被重复计算。prompt、skill、文件与 usage 的 offset 读取保留此前上下文和累计基线，只投影新完成的记录；已见 item/response 镜像及单独补上的换行不重放旧数据。应传入完整、只追加的字节快照与上次字节长度，不能只传切下来的尾部；源被截短或替换后须从 offset 零重新开始。精确记录仅在 offset 之前时，后来的累计观察不能证明新增精确 usage。这些仍是有界的尽力投影；analyzer position 是所传快照长度，不代表每个字节都产出了完整元数据。prefix prompt 正文在记录限额内做 hash，旧正文不占新增输出的字符串或保留正文预算；当前 scope 和输出上下文仍有界。skill 投影用受支持的 prompt 正文 hash 配对，只保留识别到的斜杠命令及其上下文，prompt 投影不保留 skill event。skill event ID 依来源 identity 生成，无 identity 时使用带限定的 occurrence 后备；旧平铺格式的读取也跨 offset 保留物理行序号。各能力只投影自身元数据：prompt 预算失败不抑制其它有效 token 或已完成的文件变更，response/patch identity 限额也不抑制 prompt。所有投影仍共享输入和单条记录限额。预算错误会指出对应能力与限额，并说明该能力无法投影此会话；等待或从相同超限快照重新开始不能修复。底层 parser 仍可提供 partial 元数据。缺失 usage 通常提示等待，只有 usage 自身的 counter、response identity 或 reset 观察无证时才给出另一诊断；无关 prompt 的 partial 不使 usage 变成坏数据。offset 零的总量应替换此前总量，不能作为新增 delta 再相加。采集侧把 calculator 错误标为 partial 元数据，继续共享 checkpoint 流程。其它 adapter 的契约保持原状。


## 原生 Windows 采集

此支持目前仅在 **main 源码构建**中提供，**v0.30.50 发布二进制不包含该功能**。
在后续正式版本包含此功能之前，请按[源码构建说明](../../../README.md#install)安装。

Windows 可在本地 NTFS 磁盘上采集 Session，无需 WSL。安装 **OpenCode 2.x**；
GitHub 上的 1.19.0 CLI 使用不兼容的插件 API。配置文件位于
`%USERPROFILE%\.config\opencode\opencode.json`，原生 CLI 下载地址为
<https://opencode.ai/update/api/latest/cli/opencode/opencode-windows-x64.zip>。
将 `libra.exe` 与 `opencode.exe` 加入 PATH 后，在 PowerShell 中运行：

```powershell
libra init
libra agent add opencode
libra agent rpc trust --dir C:\path\to\verified-opencode2
libra agent rpc trust opencode
opencode run --standalone "Reply with a short greeting."
libra agent session list
libra agent checkpoint list
libra agent checkpoint export <checkpoint-id>
```

Windows 通过 no-follow `CreateFileW` 句柄校验本地路径，拒绝 reparse point
（含 junction）、alternate stream、设备名与 UNC 采集存储。仓库私有密钥使用
当前用户/SYSTEM ACL、`LockFileEx` 排他锁及原子 no-replace 发布；认证或恢复时
不会静默替换缺失、损坏的密钥。

Windows OpenCode exporter **没有操作系统级文件系统/网络沙箱**。只运行显式受信的
`opencode.exe`，复验其字节与文件身份并持有防替换句柄；Windows Job Object
约束进程树，deadline 为 30 秒，接受的 stdout 上限为 16 MiB。输出暂存在 ACL
保护的临时文件中，读取后删除；stderr 和环境中的凭据变量不会转发。进程运行时及
退出后均检查大小，但 Windows 没有 Unix `RLIMIT_FSIZE` 的写入时硬限制，故两次
检查之间临时输出可能超过接受上限。macOS Seatbelt 与 Linux bubblewrap 要求不变；
Windows capability 可用性要求受信 exporter，不表示存在 OS 沙箱。

Windows 支持 checkpoint 列表和普通 transcript 导出。仅 owner 可读的原始
reasoning artifact 导出，以及 `libra agent doctor` 的 object/findings 扫描仍不可用；
doctor 会明确报告不可用，且不修复这些扫描项。

## macOS 上的 OpenCode 普通采集（Libra 0.30.49）

普通自动采集使用官方 OpenCode **2.0.29**。下文冻结的 2.0.26 词汇和私有 native
producer 章节属于独立的历史/显式 native 路线，不是普通采集的前置。

Libra 0.30.50 新增 Session 目录过滤、从受支持的包管理器启动器中
选择原生可执行文件，以及 metadata-only checkpoint 导出为空时的明确警告。
下文这三项新增行为需要 Libra 0.30.50 或后续版本。

在要采集的仓库中核对版本，然后准备本仓库的 exporter trust 和 observer：

```bash
libra --version
opencode --version
libra agent rpc trust --dir /absolute/directory/containing/opencode
libra agent rpc trust opencode
libra agent add opencode
libra --json agent list --schema-version 2
libra agent doctor
```

把示例目录替换为已核验可执行文件所在的真实目录。当前 CLI 分开登记目录和信任 binary；
不能把 binary 文件名当作 `opencode` slug，也不能把 `--dir` 与该 slug 合并。
安装或刷新 observer 后，从这个仓库启动 OpenCode；`--pure` / `OPENCODE_PURE=1`
会禁用采集插件。采集一轮后检查并导出已存 checkpoint：

```bash
libra agent session list
libra agent checkpoint list
libra agent checkpoint show <checkpoint-id>
libra agent checkpoint export <checkpoint-id> -o capture.json
```

已授权源包含不支持的 reasoning state 时，可以将安全普通副本存为 `Unproven` / `Partial`，
保留 prompt、model、token 投影和可读 reasoning text，丢弃不支持的 reasoning state 及 time。
这不授予 native artifact、Complete coverage claim 或 native ACK；原始任意 `originProof`
或 sealed candidate 仍不能走此 fallback。含这类 state 的历史 import 仍报
`LBR-AGENT-020`；普通采集不启用该 import 或 durable partial replay。真实 native
`signature`、`redactedData`、`reasoningEncryptedContent` 的来源与归档是独立门，
Source A 仍为 UNPROVEN。OpenCode/账号/模型检查只在 macOS 本机执行，不进 CI。

## 概要

```bash
libra agent status
libra agent list [--schema-version <1|2>] [--json]
libra agent import (--session <id> | --path <path> | --since <rfc3339> | --all) [--agent <name>] [--limit <n>] [--cursor <n>] --yes
libra --json agent graph <session> [--repo <path>]
libra agent enable [--agent <name>]... [--hook-timeout <seconds>]
libra agent add [<name>...]
libra agent disable [--agent <name>]...
libra agent remove [<name>...]
libra agent session <subcommand>
libra agent checkpoint <subcommand>
libra agent skill <subcommand>
libra agent clean [--all]
libra agent doctor [--repair]
libra agent workspace list [--limit <n>] [--cursor <token>] [--state <state>]...
libra agent workspace show <workspace-id>
libra agent push [--remote <name>] [--force-rewrite]
libra agent rpc <subcommand>
libra agent bridge --stdio
libra agent mcp --stdio
```

## 说明

`libra agent` 管理 Libra 的外部代理捕获表面。它安装和移除提供商 hook，报告已捕获的 session/checkpoint 状态，暴露只读诊断，并可将 `refs/libra/traces` 推送到远程。

`libra agent workspace list|show` 是 workspace 注册表（`workspace_record`）的
只读机器接口：agent runtime 关联过的每个 linked worktree、task worktree 或
remote workspace，含生命周期状态（`provisioning`/`active`/`releasing`/
`released`/`orphaned`）、owner、lease fence/到期与 canonical 路径。`list`
走 keyset 分页（按 `workspace_id` 升序；默认 `--limit 50`，上限 500；
`next_cursor` 原样回传），`--state` 可重复过滤；`show <workspace-id>` 返回
冻结的 schema v1 单条记录。lease 变更不在此暴露——它属于 agent runtime 的
内部服务。

支持的 roster 为 `claude-code`、`codex`、`opencode`（首批），三者均可安装 hook：`claude-code` 写 `.claude/settings.json`；`codex` 写用户级 `$CODEX_HOME/hooks.json` 并在 `$CODEX_HOME/config.toml` 写入 Libra 托管的 trust 条目（未受信的 Codex hook 会被静默跳过，trust 条目是安装的一部分）；`opencode` 写 Libra 托管插件 `.opencode/plugin/libra-hooks.js`（注意：`opencode --pure` 会禁用包括捕获在内的全部外部插件）。`gemini` 已从支持 roster 降级为仅卸载通道：`libra agent remove gemini` 可移除历史安装的 Libra 托管 hook（幂等），已捕获会话保持可读；对它或其它非 roster 代理执行 `add`/`enable` 会返回可操作的 unsupported 错误。

对 Codex，`enable` 与 `disable` 会在协作式 Libra 进程锁下更新耦合的
`$CODEX_HOME/hooks.json` 与 `config.toml`。它们会先准备两个文件，并拒绝在替换前可观察到的
编辑；但任意编辑器无法被可移植的原子“比较并替换”覆盖。命令运行期间请不要编辑这两个文件。
若报告非明文完整性恢复日志路径，说明继续自动覆盖已不安全；请保留该日志，先手动解决编辑冲突，
移除所报告的日志后再重试。日志只记录有限的事务元数据（schema 版本、操作、阶段和恢复围栏）
以及域隔离的文件指纹，不含原始 hook 或 config 内容；未知指纹状态会刻意保持零写入，而非自动修复。
在 Unix 上，Codex 以规范化的 `hooks.json` 路径作为信任键。`enable` 在写入 trust 条目前解析已存在的
`$CODEX_HOME` 目录（包括 macOS 的 `/tmp` 与 `/private/tmp` 别名），并清理 Libra 以前按
调用方别名写入的托管条目。升级后若 `agent doctor` 显示 Codex hook 未受信，可先检查
主动禁用的条目，再重跑 `libra agent enable --agent codex`。

### OpenCode 事件输入契约

OpenCode parser 使用已审计的 **2.0.26** 事件表：`session.created`、用户
`session.inbox.delivered`、`session.execution.succeeded`、
`session.execution.failed`、`session.execution.interrupted`、`session.deleted`
和 `session.compaction.ended`；`tool.execute.after` 是独立的工具观察 hook。
delivery 必须带 `role="user"` 和字符串 prompt，允许空字符串。只有 `user`、
`superseded`、`inactivity` interruption 会结束 turn；`shutdown` 保留重启连续性。
enqueue 由插件内部合并，不作为独立 lifecycle 输入。

兼容输入是 `session.status(idle)`、`message.updated`、`session.compacted` 和带
跟踪 session ID 的 `server.instance.disposed`；busy/retry status 拒绝写入。旧
`message.updated` 的 prompt 必须是字符串，允许空字符串；role 可以省略，显式
role 只能为 `user`。`message` 和 `user_prompt` 不会填补缺失 prompt。独立弃用
alias `session.idle` 仍接受，只记录不含内容的 `legacy_event_alias` tracing 警告。

managed plugin 使用 OpenCode 2.0.26 的 `setup(ctx)` API、事件订阅和只观察的工具
hook。一个 OpenCode 服务器（例如 `opencode serve --service`）会把所有项目的事件发给
每个项目的插件。Libra 0.30.50 及后续版本不转发已知属于其他
`location.directory` 的 Session，并在此关闭移出目录的 Session；没有位置的事件仍会
转发，除非已知该 Session 属于其他目录。尚未归属的 Session 的工具调用
通过宿主的 `session.get` 判定（宿主无法回答时照旧转发，并只报告一次
`session_location_unavailable`）。执行 `libra agent enable --agent opencode`
刷新；旧模板、修改过的模板或双目录 managed 重复副本均不报告 installed=true。
Node/Bun 共用 `node:child_process`，固定绝对二进制文件名、`shell:false`；
session/turn 边界同步转发，tool/compaction 异步观察，不修改宿主参数或返回控制字段。

user inbox enqueue/delivery 在有界 live 窗口内按 session/inbox ID 配对一次，并转发稳定 `message_id`
供 Libra 既有的有界 receipt ledger 消费；这不是无限历史重放去重保证；旧 user message
取首个 text part，没有文本时在下一无关事件或 cleanup 前显式发送空字符串。
assistant step 跟踪 model。上限为进程级 64 个跟踪会话、128 个 pending prompt、
128 个先到 text part、256 个已分类 ID、16 个异步子进程；prompt 最多 16,384
个 UTF-16 单元且不拆 surrogate。容量耗尽或文本缺失只输出固定、不含内容的
诊断，同一 reason 每插件实例最多一次；截断带 `prompt_truncated=true`。ID 历史为滚动 256 条窗口，session 结束后释放；窗口满后仍采集新的 prompt。会话窗口满时，
以 `server.instance.disposed` 收尾最久未观察的会话，再接纳新会话；后续事件
可重新打开被移出的跟踪。这仅结束 Libra 跟踪，不删除或停止 OpenCode 会话。
early text 同样使用滚动窗口。

cleanup 与 process exit 遍历所有跟踪会话，各使用一个共享总 deadline。
转发失败不阻断 OpenCode 宿主；非零退出/超时只输出固定的
`forward_failed` / `forward_timeout`，每个原因在每个插件实例中最多一次，不回显错误内容。每个子进程的上限为 export deadline + 15 秒
（当前 45 秒），超时用 `SIGKILL`。installer 预检两个目录，读取前拒绝 symlink
和特殊文件，保留用户文件；managed 重复副本删除失败时回滚本次 canonical
更新。当前检查以目录命名空间稳定为前提，检查后目录移动及末次归属检查后的
并发叶替换仍延后。`opencode --pure` / `OPENCODE_PURE=1` 关闭外部插件和
hook 采集。Node/Bun 测试使用 synthetic 事件与受控 exporter；真实 OpenCode
内容采集和 native reasoning 来源仍由各自验收门证明。

OpenCode 2.0.26 export 元数据读取 `user.text`、assistant `model.id`、
`tokens` 与成功的文件工具。assistant token 总量为
`input + output + reasoning + cache.read + cache.write`，cached 为
`cache.read + cache.write`，不再叠加 session 总量。兼容旧 `info/parts`：
assistant 聚合用量优先，没有聚合时才累计去重的 step-finish 明细；user 模型字段不被采用。
文件清单合并成功的 edit/write/patch 与 snapshot，patch 移动需验证源和目标。
输出为项目相对路径并使用正斜杠；工具路径依 session 已证明的 location，snapshot 已相对项目根。
历史位置缺失、项目外或非法路径、不支持的形状、无效 token 数值及容量上限均标记 partial，
仅输出不含内容的诊断。idle、model/agent 切换、synthetic/system/skill、shell 与 compaction
是已识别的元数据，不进入用户 prompt。内部 ID、路径和别名超限也标记 partial，
ID 容量耗尽后不再消费后续消息。本提取不读取文件、不归档 reasoning 正文；合成 fixture
不能证明真实 macOS 内容采集或 native reasoning 来源。

旧 ordinary OpenCode coverage normalizer 接受 2.0.26 原生 messages 与旧 `info/parts`。
每条 human message 开始独立且稳定的 logical turn；有效 native `idle` 结束本区间
所有待定分轮，包括 failed/interrupted。Complete 表示采集表示完整，不等于任务成功。
最后有效 idle 后的语义消息保持 Incomplete；未知、错型内容与 reasoning 保持
Incomplete，idle 不会洗白。旧格式 step-finish、patch、step-start、snapshot、retry、
compaction、agent、subtask、file 九类元数据只识别、不投影。
native 用户附件、skill text 与工具文件结果尚不支持：标记 Incomplete，
不向语义记录复制附件字节或 URI。良构 agent/skill 元数据、model/usage 和 provider state
不改变 digest。原生工具使用 name/id/state.input 与文本 state.content；error 无 content 时
只投影结构化 error.message，不投影 response.body。本卡不凭 reasoning 字节或 state 证明
可信 native 来源。

OpenCode 用户文本在native/classic coverage与native/classic/flat-JSONL元数据提取中
共用reminder过滤。精确字面 `<system-reminder>...</system-reminder>` 块（含嵌套）
在任何位置被移除；块外文本保持原字节。纯注入消息不产用户prompt或User语义记录，
过滤先于skill匹配，独立token元数据仍保留。未闭合块隐藏尾部并标记partial/Incomplete；
孤立结束标签保留并标记partial/Incomplete，idle不能洗白。只过滤用户文本，
其它provider语义保持。此为字面标记过滤，不证明producer来源。

OpenCode 提取对空输入、非法 JSON 和非法 export 形状显式标记
`extraction.partial=true`，记录不含payload的固定警告。缺失或不支持的role仍可保留
良构的独立flat model/token字段。合法空会话export与缺失transcript字节
（`extraction.present=false`）不同。非关键提取错误不阻断原本允许的checkpoint写入；
坏export不替换已有checkpoint。coverage、授权与native来源门仍然必须满足。

## 子命令

| 子命令 | 说明 |
|------------|-------------|
| `status` | 报告已捕获的外部代理会话状态 |
| `list` | 列出受支持代理的能力矩阵（roster、hook、安装状态） |
| `import` | 经明确同意后，发现并导入历史 Claude/Codex transcript 文件，或导入一次受信、沙箱化的 OpenCode export |
| `graph <session>` | 只读检查 session → turn → revision → subagent 捕获图；需要全局 `--json`/`--machine`（交互式 TUI 入口已在 W5 breaking 发布中删除） |
| `enable` | 启用一个或多个外部代理并安装 hook |
| `add` | `enable` 的别名：`add <name>` ≡ `enable --agent <name>` |
| `disable` | 禁用一个或多个外部代理并卸载 hook |
| `remove` | `disable` 的别名：`remove <name>` ≡ `disable --agent <name>` |
| `session list` | 列出已捕获会话 |
| `session show <id>` | 显示一个已捕获会话 |
| `session stop <id>` | 将已捕获会话标记为 stopped |
| `session resume <id>` | 将已停止的已捕获会话重新标记为 active |
| `session derive-tool-calls <id>` | 从已捕获会话推导工具调用记录 |
| `checkpoint list` | 列出已捕获 checkpoint |
| `checkpoint show <id>` | 显示安全的 checkpoint 结构摘要（隐藏 metadata 和内部对象 ID） |
| `checkpoint rewind <id>` | 检查或应用某个 checkpoint 的工作树回退 |
| `checkpoint export <id>` | 导出 checkpoint transcript：默认脱敏（无需授权）；raw（未脱敏）导出须 `--allow-raw --raw` 并写入 append-only `agent_audit_log`（缺失授权时拒绝并返回 `LBR-AGENT-013`）。未采集到 transcript 的 checkpoint（metadata-only 采集——对 OpenCode 而言即写入时 export bridge 不可用或执行失败）导出 0 字节并发出 warning |
| `skill search` | 按 `--skill`、`--provider`、`--session`、RFC3339 `--since`/`--until` 搜索捕获的 skill events（`--limit`/`--cursor` keyset 分页、`--json`）。基于 checkpoint metadata 的读时投影，无独立表 |
| `skill list` | `skill search` 的别名（同过滤项） |
| `skill registry` | 展示各 agent 的 curated 可发现 skill 注册表（`--provider <slug>` 限定；公开 SkillDiscoverer 面） |
| `clean` | 清理已停止会话的临时 checkpoint（prune 遇到进行中的 checkpoint 写入、traces 引用可达但无 catalog 行的提交、或仍有耐久 object-index repair 待处理时 fail-closed 拒绝；同时删除因此不可达的 `object_index` 行） |
| `doctor` | 诊断 hook 安装和捕获状态；检测（`--repair` 时修复）checkpoint 存储不一致。对象检查刻意只读取本仓库的本地对象目录：不可用的 alternate/remote 对象会保守地报告为不可用，而不会在产生诊断时解析外部可控的 alternate 路径。只读的 `legacy_code_residue` 字段（人读为「Frozen Code residue」行）报告冷冻的 Code 时代路径 `.libra/sessions/code/`、`.libra/code/` 与 `libra/intent` ref 是否仍存在；它不会删除或改写该状态，清理另由 plan-20260920 ADR-RC-04 / DEFER-RC-02 承接 |
| `push` | 将 `refs/libra/traces` 推送到远程（`clean` prune 重写后的非快进推送用 `--force-rewrite`，采用 force-with-lease 语义） |
| `rpc list` | 列出 `PATH` 上发现的 `libra-agent-*` 二进制（含 trusted/quarantined 状态）；需先开启 external-agents 开关 |
| `rpc trust <slug>` | 信任一个已发现的二进制——记录 path + sha256 + device/inode/mtime 来源（所在目录 world-writable、或二进制不在受信目录下时拒绝——`LBR-AGENT-005`）。provider-exporter slug `opencode` 则改为固定 provider 自身的 CLI 二进制——只从已注册受信目录解析、绝不扫描 `$PATH`——供沙箱化 export bridge 使用；该形式无需 external-agents opt-in。来源 sha256 以单次流式读取计算、不设固定大小上限，因此大型单文件 provider CLI（OpenCode Bun 构建约 171 MiB）仍可受信；计算期间读出字节超过其报告大小的文件会被拒绝（`LBR-AGENT-005`）。mise（aube）或 pnpm 的 `.bin` 启动器 shim 会被跟随到其声明的原生可执行文件，该文件本身也必须位于受信目录下（`--json` 中以 `launcher` 字段报告）；JavaScript 启动器会被拒绝（`LBR-AGENT-005`） |
| `rpc trust --dir <path>` | 注册一个受信目录（`agent.external_agents.trusted_dirs`，默认 `~/.libra/agents`）：外部二进制的 canonical path 必须位于其中之一才可被信任。路径会被 canonicalize，且必须是存在且非 world-writable 的目录 |
| `rpc untrust <slug>` | 撤销 binary trust；OpenCode 同时原子吊销 native issuer，详见下方轮换与撤销章节（始终可用，不受开关限制） |
| `rpc invoke` | 在**已信任**的 `libra-agent-*` 二进制上调用一个 JSON-RPC 方法 |
| `bridge --stdio` | 在 stdin/stdout 上运行仓库级 DeepSeek Harness 桥接（JSON-RPC 2.0 NDJSON）。这是 Harness 的**唯一**标准入站写入传输；它不是 `libra code --control`。stdout 每条只承载一个协议帧；诊断只写 stderr。协议 v1：20 个 method 的 allowlist、256 KiB 帧上限、64 个 in-flight 请求、默认 30 秒 deadline。20 个 method 全部已实现：`initialize` 握手；session/event ingress `session.open`、`event.append`（batch ack / 幂等 / digest conflict / 服务端脱敏）、`session.flush`、`session.close`、`evidence.append`、`provenance.append`；只读方法 `context.get`、`status.get`、`history.search`、`checkpoint.list`、`checkpoint.show` 与 `diff.get`；mutation `checkpoint.create`、`commit.create`、`checkpoint.restore` 与 `review.run`（均带 `operation_id` 幂等、actor binding 与 approval 门禁）；以及 workspace lease `workspace.claim` / `workspace.renew` / `workspace.release`（owner 从已认证的 bridge session 派生）。`diff.get` 只接受封闭的 `mode`（`worktree` / `staged` / `checkpoint`）加上经校验的仓库相对 `paths`——绝不接受自由形式的 revision 或 pathspec——并强制 `--no-ext-diff` / `--no-textconv`，使仓库配置无法转化为进程执行。`commit.create` 只提交当前 index（无 `-a`、无 pathspec、无 amend、无 author 覆盖），关联图写入 `agent_bridge_link` 而不是拼进 commit message。`checkpoint.restore` 必须携带显式 `expected_head` fence 且要求 index/worktree 干净，并且绝不移动 HEAD。`review.run` 启动只读 review 并返回 `run_id`；用同一个 `operation_id` 重放会返回该 run 的当前状态，而不会启动第二个 run。HEAD 漂移或 worktree 脏在任何写入之前以 `LBR-AGENT-038` 拒绝。ack 只表示脱敏 projection 已 durable，不代表 Harness 原始 transcript 已迁移到 Libra。不是 Git 命令。 |
| `mcp --stdio` | 在 stdin/stdout 上提供仓库 MCP 服务（JSON-RPC 2.0，每行一条）。legacy 协议版本 `2025-11-25` 与 `2025-06-18` 支持 `initialize`、`notifications/initialized`、`ping`、`tools/list` 与 `tools/call`。协议失败是 JSON-RPC 错误；参数不符合工具输入 schema 时返回工具错误 `LBR-AGENT-027`（`isError: true`）。stdout 只写 JSON-RPC 消息。这不是 `libra agent bridge`，也不是已删除的 `libra code` MCP。 |

## 已删除的 dry-run prompt 输出

`--show-redacted-prompt` 不是旗标。promote 子命令保持删除，因此 dry-run 不会打印 prompt 正文。人读输出和 `--json` 都不包含 prompt。clap 拒绝该调用，而不是格式化 `<len=N>` 行。

## MCP server

`libra agent mcp --stdio` 是给外部客户端用的仓库 MCP 服务。客户端配置形状为 `command` = `libra`，`args` = `["agent", "mcp", "--stdio"]`，`"cwd"` 为仓库目录：

```json
{"command":"libra","args":["agent","mcp","--stdio"],"cwd":"/path/to/repo"}
```

服务端使用 legacy MCP `2025-11-25` 与 `2025-06-18`。无法识别的 `protocolVersion` 按 `2025-11-25` 应答。`capabilities` 只有 `tools`（`listChanged: false`）。JSON-RPC batch 会被拒绝。不符合工具 schema 的输入返回工具错误 `LBR-AGENT-027`。

工具结果在写出前会 redact：服务端对 `structuredContent` 脱敏，清洗除换行和制表符以外的控制字符，再据此重建文本副本。已知密钥模式被去掉。若 redact 后的结果无法再次解析，服务端返回工具错误 `LBR-AGENT-034`，不会回退到原始结果。完整 JSON-RPC 响应超过 256 KiB 时返回工具错误 `LBR-AGENT-028`。读工具超过 30 秒返回工具错误 `LBR-AGENT-036`，且 `retryable` 为 true。读工具最多在 4 个并发读子进程中执行，每个有 512 MiB 常驻内存预算；检测到超限时会终止子进程，并返回 `LBR-AGENT-028`，原因是 `worker_memory_limit`。在 Windows 上，由操作系统内存限制终止的子进程也可能返回 `LBR-AGENT-037`。超时后服务端先回复再强制终止该子进程，回收完成后才把名额交给下一次读取。

## 常用选项

| 标志 | 子命令 | 说明 |
|------|------------|-------------|
| `--agent <name>` | `enable`, `disable` | 选择代理名称；省略时针对支持 roster（`add`/`remove` 以位置参数接收名称） |
| `--hook-timeout <seconds>` | `enable` | 写入已安装 Claude Code 与 Codex hook 的宿主 handler timeout（默认 30）。隐藏的 `--capture-budget-ms` 由它减去最多 1000ms 清理余量得出（`30 → 29000`、`60 → 59000`）。Claude `SessionEnd` 仍不设置 timeout、预算保持 `9000`；Codex `SessionEnd` 与 `Interrupt` 仍限制为 `3s → 2000`。所有选中的代理都必须接受宿主 timeout：与 OpenCode、Grok 或 Hermes 同时使用时，会在写入任何 hook 前以 `LBR-CLI-002` 失败。`0` 或非数字会被拒绝。不带该参数重跑 `enable` 会恢复默认值 |
| `--schema-version <1\|2>` | `list` | 选择 machine schema；版本 1 保持冻结旧行结构，版本 2 增加 `transcript_discoverable`、`importable`、`export_bridge` 的 `methods[]` 可用性 |
| `--session <id>` / `--path <path>` / `--since <rfc3339>` / `--all` | `import` | 四选一的历史导入范围；`--path` 还必须指定 `--agent`，OpenCode 通过 export bridge 支持显式 `--session` |
| `--yes` | `import` | JSON/非 TTY 导入必需；确认 Libra 可以读取私有 provider session 内容、脱敏并把类型化投影写入当前仓库 |
| `--restore-erased` | `import` | 显式移除本地防复活 tombstone 后重试；要求 `--yes`，并追加审计记录 |
| `--limit <n>` / `--cursor <n>` | `import` | 有界发现分页（默认 20、硬上限 100）以及上一页返回的下一个零基游标；单次调用还有 64 MiB 累计原始输入上限。每源有效 cap 为 `min(agent.max_transcript_read_bytes, 16 MiB adapter hard cap)`；显式配置更大值时会诊断实际有效 cap |
| `--repo <path>` | `graph` | 从另一个 Libra 仓库读取捕获元数据，而不是从当前目录发现仓库 |
| `--limit <n>` | `session list`, `checkpoint list` | 每页最大行数（默认 50，硬上限 500——超过时钳制并在 stderr 提示；`0` 按 `1` 处理） |
| `--cursor <cursor>` | `session list`, `checkpoint list` | 上一页 `next_cursor` 返回的不透明 keyset 游标；不要手工构造 |
| `--extract-transcript <path>` | `session show` | 从当前已验证的 Claude Code transcript 来源重新派生并最多复制 16 MiB 到一个新的本地文件；绝不回退使用已捕获的 metadata 路径，也绝不覆盖已有路径，超限时不发布输出，代理不受支持或当前来源不可用时会失败。使用 `--json` 时，结果为安全 envelope：`{ "session": <session row>, "extracted_transcript": { "output_path": <path>, "bytes": <count> } }`；提取块只包含目标路径和字节数。JSON 和人类输出都不暴露 provider 来源路径。 |
| `--allow-raw` / `--raw` | `checkpoint export` | 授权并请求 raw（未脱敏）导出；缺少 `--allow-raw` 时 `--raw` 请求会被拒绝（`LBR-AGENT-013`）并记入审计 |
| `--justification <text>` / `-o <path>` | `checkpoint export` | raw 导出的审计理由与输出文件 |
| `--all` | `clean` | 清理所有已停止会话的 checkpoint，而不只是最近一个 |
| `--gc` / `--retention-days <n>` / `--dry-run` | `clean` | 三窗口保留期 GC：(1) 删除已停止会话中早于 `agent.retention.transcript_days`（默认 90；用 `--retention-days` 覆盖）的 checkpoint；(2) 清理早于 `agent.retention.stderr_days`（默认 30）的**终态** run 的 reviewer stderr 诊断日志，保留聚合记录；(3) **A0-09** 删除早于 `agent.retention.findings_days`（默认 90）的**终态** review/investigate run 整个目录（`findings.md`/`manifest.json`/`state.json`/reviewer 日志）。对象化的 findings blob 是内容寻址对象，由仓库级 `libra maintenance run --task gc` 可达性回收（PD-04）：孤儿 findings blob 连同其 `object_index` 行一起回收，live-run manifest 引用的 OID 保持存活（per-run retention 绝不删除可能被共享的对象）。non-terminal/时间戳不可解析的 run 一律 fail-safe 跳过；永不触碰 `agent_audit_log`。`--dry-run` 仅预览各窗口及配套清理的 would-be 删除（包括 JSON `findings_runs_pruned`/`import_identities_pruned`），不实际删除 |
| `--repair` | `doctor` | 修复检测到的 checkpoint 存储不一致（重建 catalog 行、补插 `object_index` 行、安全 drain 有效的过期普通 writer marker，并忽略普通 TTL 立即 drain `cleanup_pending` marker）；损坏 marker 保持 `manual_required`。同时修复当前授权、可达的 agent 提交关联；省略时仅检测，完整帧损坏的 journal 会保留供检查 |
| `--commit-link-page <1..100>` | `doctor` | 每次检查最多 100 个从当前 worktree HEAD 可达的提交；完整性始终标为 unknown |
| `--codex-legacy-after <checkpoint-id>` | `doctor` | 从上一页报告的 checkpoint 游标之后继续扫描 Codex legacy reasoning revision；需要修复该页时同时使用 `--repair` |
| `--reset-commit-link-journal` | `doctor --repair` | 检查后显式丢弃损坏或无法回收的满日志；仍可读取且有容量的日志会拒绝重置。其中残留的无 trailer 恢复证据会丢失，提交、checkpoint 和已有 catalog 关联不变 |
| `--acknowledge-commit-link-loss` | `doctor --repair --reset-commit-link-journal` | 检查其他页与 worktree 后，允许重置仍含其恢复证据的可读取满日志；不能绕过失败的修复、lease 核验或未完成的 64 关联批次 |
| `--commit-page <1..100>` | `session show` | 每页显示最多 64 个当前作用域 Codex 会话的本地提交关联；第 100 页仍有更多时报告上限 |
| `--remote <name>` | `push` | 选择用于推送代理 trace 引用的远程 |
| `--force-rewrite` | `push` | 允许本地 `clean` prune 之后的非快进推送（traces 引用由 Libra 托管，prune 即整链重写）；采用针对本仓库最近一次推送记录的 force-with-lease 语义——绝非无条件 force——远程被别处重写时仍 fail-closed 拒绝 |
| `--dry-run` | `checkpoint rewind` | 显示影响而不修改文件；这是默认值 |
| `--apply` | `checkpoint rewind` | 恢复所选 checkpoint 的工作树 |

## Review 与 investigate 工作区

新的 review 和 investigate 工作区位于仓库共享存储下的 `.libra/worktree/tasks/<run-id>/<attempt-id>/workspace/`；checkpoint 范围运行使用同级 `checkpoint-input/`。继续 investigate 会分配新 attempt，不覆盖之前的工作区。从 linked worktree 发起运行也使用同一个共享根目录。

运行记录、findings 和日志仍在 `.libra/sessions/agent-runs/<run-id>/`，工作文件目录变化不移动 session 元数据或对象库。新 state 和 manifest 使用 schema version 2，保存工作区所有权描述。已知 version-1 记录仍可读取，不搬迁旧目录；未知版本不会被自动改写或清理。

`agent clean --gc` 遇到无法验证工作区所有权、无法解析 state 或 manifest，或无法删除工作区的 run 时会跳过该 run，并继续清理其他符合条件的 run。文本和 JSON 报告包含 `findings_runs_skipped` 与有界的 `findings_gc_warnings`，便于检查后重试。旧版 run 若把 `checkpoint-input` 放在运行记录目录内，在其他所有权检查通过后仍可清理；缺少版本字段的旧 manifest 仍可读取。

Libra 内部的任务沙箱接口可绑定已验证的 attempt，将写权限限制在该工作区和缓存路径，并拒绝读取敏感仓库元数据。`review` 和 `investigate` 在隔离副本中启动外部 agent CLI，不会把这套内部 OS 沙箱施加给子进程；实际文件访问由外部 CLI 自身的沙箱配置决定。`libra agent doctor` 只读报告托管 task 容器中的未知或无主目录，不删除它们。

## 范围化 checkpoint 的暂停恢复

`review --checkpoint <id>` 与 `investigate start --checkpoint <id>` 只把已证明属于普通角色的 checkpoint 文件，以只读形式物化到 `.libra/worktree/tasks/<run-id>/<attempt-id>/checkpoint-input/`。每次物化（包括 `investigate continue`）都在创建新 attempt 输入之前，按显式仓库 catalog 和 checkpoint manifest/tree 角色复验保存的 spec。受支持的无 manifest wrapped checkpoint 仍可恢复。旧 full-list spec 只有在 closed `reasoning/encrypted/<64 hex>` 条目的路径与 blob id 精确匹配 manifest 和树时才被接受；该条目被排除，不打开其 payload。改写的条目、未知路径、重复路径及普通角色不匹配均拒绝；保存的 id 若不是 blob，或字节的内容哈希不匹配该 id，也在创建新 attempt 输入之前拒绝。catalog 暂时不可用时，暂停中的 continue 保留原 state 与输入。review 设置阶段超时沿用基础设施错误，取消沿用 cancelled；设置阶段与后续每个 reviewer 的超时预算独立。有界文件操作持续检查实际调用者的取消与绝对期限，所属工作线程结束后才返回。普通 payload 仍为单文件 64 MiB、总量 256 MiB；物化限制不变，不新增稳定错误码。

## Scoped run 清理

`libra review clean` 与 `libra investigate clean` 只删除各自类型的已结束 run。清理先校验保存的工作区所有权描述，删除该 run 拥有的 task 工作区或 checkpoint 输入，再删除运行元数据。失败时保留所有权和重试记录；目录被替换、链接、未知描述版本或所有权不匹配均拒绝。仅凭目录名称不能授权删除。`--run` 和 `--all` 都先核对同一 run 目录内的生命周期 metadata，再处理只读 checkpoint 输入。批量清理保留其他类型、未结束、未知和损坏的 run；`removed` 只统计实际成功删除。指定 run 的 state 损坏或缺失时，只有该 run 自身有效的终态 manifest 能证明 run id、类型及 schema，才允许清理。可解析的其他类型或未结束 state，以及互相冲突的 metadata 都拒绝。`investigate clean` 保留既有 run 锁。有界输入清理拒绝链接根目录、链接 metadata 和超限输入；失败返回可操作错误，不报告删除成功。保留 run，核对提示中的链接、限制或文件系统权限后再重试。清理启动后若 I/O 或取消中断，部分输入可能已删除；失败不表示已经回滚。已经证明属于本类型的终态 state，在先写 state 的结束流程中断、匹配的 manifest 尚无终态标记时，仍允许清理。身份、schema、共有字段或不同的非空终态值冲突继续拒绝。批量资格读取的 I/O、链接或大小限制错误会中断清理，之前已经删除的 run 不会回滚。不新增清理 flag 或稳定错误码。

## 提交会话关联（CX-23 开发版）

CX-23 源码开发版使用本地 `agent_session_commit` 关联表，幂等键为
`(session_id, hash_kind, commit_oid)`，SHA-1、SHA-256 和 BLAKE3 分开记录。
历史导入不写关联。普通 commit 默认也不写；显式执行
`libra config agent.commit_trailer true` 后开启关联。若无法读取此配置，关联保持关闭并提示警告；
其它必需配置有效时提交仍可继续。可读取但值无效则拒绝提交。启用后只选择当前仓库/worktree 中有已提交
checkpoint 的会话：锚定当前父提交的会话，或已关联该父提交且仍活跃的会话，
最多 64 个；超过 64 个会拒绝提交而非静默截断。
`commit-msg` hook 结束后、可选 vault 签名前追加 `Libra-Agent-Session` trailer，
已有同值用户 trailer 不重复。`libra commit --no-agent-trailer` 仅省略本次
自动 trailer，仍保留已开启的本地关联；仓库 catalog、作用域、64 会话上限
或 workspace lease 检查失败仍在 HEAD 更新前拒绝提交。预演不写关联、也不
预览自动 trailer。ref 更新后在同一数据库事务内重新核验会话作用域及 workspace lease 才写关联，
失败会显示警告。amend 会保留原提交已关联且仍授权的会话，也纳入新符合条件的会话；
即使以 `-m` 替换消息也会重新注入自动 trailer。
无 trailer 提交会先在 SQLite 写锁内将带校验和的 prepared intent 写入
`.libra/agent-commit-links.v1` 并同步，再移动 ref；日志上限 10 MiB，在 Unix
上仅所有者可读写，拒绝符号链接。写入/同步失败不移动 ref，ref 失败可能留下
不可达提交的 intent。擦除的 tombstone 持久化后，日志中的对应本地身份会在
同一写锁下压缩清理；清理失败会明确报告部分擦除，可重试。`libra agent doctor`
按每页 100 个从当前 worktree HEAD 可达的提交只读检查，显式 `doctor --repair` 才回放当前仍授权的关联。
日志达到 10 MiB 时回收已完成的 prepared/linked 对；完整记录损坏时保留文件供
检查，仅截断真正未写完的末尾帧。`--commit-link-page <1..100>` 选择检查页，
`session show <id> --commit-page <1..100>` 每页显示最多 64 个当前作用域 Codex
本地关联。无论候选是否为空，证据完整性都标为 unknown。完整帧损坏或
无法回收的满日志须先检查；显式 `doctor --repair --reset-commit-link-journal` 会用空的
日志替换（Unix 模式 0600），丢弃尚存的无 trailer 本地关联恢复证据；先回放并回收当前页可用证据，可读取且有容量时不允许重置。可读取的满日志要求 lease 有效、修复和清理成功且无未完成的 64 关联批次；其他页、worktree 有证据或当前历史扫描不完整时，先检查，再用 `--acknowledge-commit-link-loss` 明确授权丢弃。`unmatched_scope_intents` 统计当前作用域中不在所选页或当前分支、不可达提交以及 hash-kind 不匹配的记录。权限、I/O、不安全文件入口和未来 schema 错误均拒绝重置，`commit_links.journal_unavailable_reason` 仅报告无正文类别。修复后日志清理失败会在 `commit_links.cleanup_incomplete` 中报告，已提交的 catalog 修复仍显示，部分擦除随后可重试。
cloud refs-only metadata 不备份本地关联表
或 journal。显式 opt-in 的可达提交 trailer 只是跨机器重建线索，仍须核验
仓库及会话作用域。

首个无父提交可选择锚定 unborn parent 的会话。此配置只作用于普通
`libra commit`；bridge `commit.create` 继续使用独立的 `agent_bridge_link`。
显式 trailer 会向所有可读取提交的人暴露原始 session ID。所谓无 trailer
关联，是用 `--no-agent-trailer` 建立的本地关联；当前 cloud sync 仅上传 refs，
不上传此 SQLite 表。若 ref 后关联失败，请保留已完成提交、journal 和
原始会话；在同一 worktree 运行 `libra agent doctor --repair` 回放合格的可达关联，
不要仅为重试关联而 amend。
复用旧消息的 amend 可能保留已擦除或移出作用域会话的可见 trailer，但不会为它
建立新本地关联；`--no-agent-trailer` 不删除继承或用户自行写入的消息文字。

## JSON 输出

支持结构化输出的子命令使用全局 `--json` 和 `--machine` 信封。例如：

```bash
libra --json agent status
libra --json agent list
libra --json agent graph <session>
libra --json agent checkpoint list
libra --json agent rpc list
```

`agent list --json` 携带稳定的 `schema_version`，并为每个受支持代理输出一行——首批 roster `claude-code`、`codex`、`opencode`。非首批代理（`gemini`、`cursor`、`copilot`、`factory-ai`）仍保留在注册表中以保证历史会话可读，但不会出现在该列表里。每行携带 `slug`、`agent_kind`、`stability`、`supported`、`support_wave`、`registered`、`transcript_readable`、`hook_installable`、`installed`、`launchable_review`、`launchable_investigate`、`external_binary`、`config_paths`、`protected_dirs`、`capabilities`。行结构是面向自动化的冻结契约。Claude Code 会声明 `capabilities.transcript_preparer=true`：Libra 先安全打开并 pin 已授权 descriptor，再只通过同一 descriptor 短暂等待末尾 JSONL 记录完成 flush；等待与 tail probe 均有界，preparer 不会按路径重新打开文件。

只有调用方理解扩展时才请求 `agent list --schema-version 2 --json`。其
`methods[]` 报告 transcript 发现、历史导入与 OpenCode export bridge 的
支持及当前可用状态；默认版本 1 的 payload 保持原有字段集合，不会隐式
增加扩展字段。OpenCode 的 `transcript_discoverable` 明确为 unsupported（不支持
批量发现）；显式 ID 的 `importable`/`export_bridge` 可用性取决于受信任离线
exporter 与 sandbox。

在 Unix 上，exporter 子程序及其后代的 `RLIMIT_CORE` soft/hard limit 均设为零。
当系统 core handler 尊重该限制时，预期的限制执法与意外 exporter 崩溃都不会
保存 core 文件；系统日志仍可能记录信号事件。Libra 保留退出状态及有界 stderr
诊断。该设置只作用于子程序，不修改 Libra 父程序限制或系统配置。

OpenCode 默认 export 预算为30秒，export lease与插件转发超时精确联动增加15秒余量，
即45秒。重新运行 `libra agent add opencode` 可刷新旧插件。schema v2 的
`importable` / `export_bridge` 在 Unix 上要求受信 exporter 和平台 sandbox 探测成功：
macOS 使用 Seatbelt，Linux 保留现有受信 bwrap 探测。Windows 要求受信的原生 exporter，
使用上文的 Job Object 进程边界，不提供 OS 沙箱。available 只表示这些前提满足，
不证明 OpenCode 启动成功、会话内容导出成功或 reasoning 的可信 native 来源。
超时仍拒绝部分输出并保持现有 metadata-only 降级，16 MiB export 上限不变。

macOS 的 OpenCode 导出要求 **Required Seatbelt server/export pair**：两端都拒绝
`process-fork`，使用私有 HOME/XDG 目录和固定摘要的二进制副本，只允许一个带认证的
loopback 端口。取消和绝对 deadline 回收仍持有的进程组，leader 已回收后不再发送
组信号。信任、隔离、readiness、输出上限或退出失败时保持 **metadata-only**，没有
未沙箱化回退。启用 exporter 的方式：先注册包含已核验 `opencode`
二进制的目录，再固定它——`libra agent rpc trust --dir <path>`，然后
`libra agent rpc trust opencode`。两步都不会打开 external-RPC 表面
（`agent.external_agents.enabled` 保持不变）；二进制只从已注册受信目录解析，
绝不扫描 `$PATH`。沙箱只映射被固定的那一个文件，因此它必须是 OpenCode 原生可执行文件。
受信目录中若是包管理器启动器——mise npm 后端（aube）或 pnpm 生成的 `.bin/opencode`
shim——`rpc trust opencode` 会改为固定该 shim 声明的原生可执行文件，前提是该文件
所在目录也已登记（拒绝信息会给出需要执行的 `rpc trust --dir` 命令）；JavaScript
启动器（`#!/usr/bin/env node`）会被拒绝。仍固定着启动器的旧信任记录会让
`export_bridge` 报告 unavailable。请先按需登记原生可执行文件的目录，再重新执行
`libra agent rpc trust opencode`。
Linux 内容导出还要求受信的 `bwrap` 通过 Libra 的 descriptor-native
`--bind-fd` 安全探测；不支持或过旧的 `bwrap` 会报告 unavailable 并 fail-closed 为
metadata-only 捕获。应安装或升级系统 `bwrap` 包，绝不启用未沙箱化回退。不支持的 schema version 以 usage 错误（exit 129、category
`cli`）返回 `LBR-AGENT-017`。当平台无法提供 Libra 的 provider-root 安全打开
原语时，Claude/Codex 的发现与导入会如实报告为 unavailable。

`agent graph --json` 输出冻结的 capture-graph schema version 1。其 `data` 对象只含
`schema_version`、`state`、`session`、`turns`、`subagents`。存在的 session
只暴露 session id、agent kind、状态和时间；indexed turn 暴露逻辑键、派生的
零基 ordinal、coverage schema/completeness/current revision、checkpoint id、
source channel 与 append-only revision 历史。同一个 whole-transcript checkpoint
可同时出现在多个 turn 下，绝不会因其中一个 turn 升级而被隐藏为 superseded。
M1 前的 capture 以 `coverage_state="unindexed"` checkpoint 时间线返回，不伪造
revision。subagent 节点只暴露 checkpoint/link 结构，并明确保留 `resolved` 或
`unresolved`。

Libra 0.30.50 及后续版本中，即使逻辑 turn key 或 checkpoint
相同，graph 仍分别保留 coverage 1、2 两版记录及各自 revision 历史；公共 JSON
schema version 1 不变。读到不支持的已存 coverage 版本时，整个查询以
`LBR-AGENT-009` 失败，不返回成功 graph 或候选内容。请使用支持该已存版本的 Libra
发布，并运行 `libra agent doctor` 检查目录；不要改写版本号或删除证据。
doctor 分别报告两版冲突，不输出原始身份或候选内容；包括 `--repair` 在内，
冲突仍需人工处理。这些 coverage-v2 读取保证要求 Libra 0.30.50 或后续版本。

JSON/machine graph 查询不会打开 transcript/object blob，也不 SELECT working directory、
description、metadata JSON、redaction report 或 coverage digest。**Breaking change（W5-08）：**
交互式 capture-graph TUI 已在 W5 breaking 发布中删除——其受限、已脱敏的内容预览
（事件计数与紧凑的 user/assistant 消息预览，从关联 checkpoint 读取、每个最多 256 KiB）
随之移除；上述冻结 JSON v1 schema 从未携带这些字段。
本地已擦除 session 成功返回 `state="erased"`、null session、空 turns 与 unavailable
subagents，且不会重建；session 与 tombstone 均不存在时返回 `LBR-AGENT-021`。未指定全局
`--json` 或 `--machine` 时，`libra agent graph` 在读取任何捕获状态之前以 usage error
加迁移提示退出（exit 129，`LBR-CLI-002`）。

`agent import` 返回 schema version 1 的 `results`、`skipped`、`partial_results`、
`failures` 与 `next_cursor`。每项状态固定为 `imported`、`noop`、`partial`、
`skipped` 或 `failed`。自动发现的跨仓库/已擦除候选进入 `skipped`，session id
哈希脱敏并携带稳定 reason code；显式 selector 遇到同类条件仍是失败。
`results` 仅含完整成功项；发生耐久 turn 进度但最终
失败的 selection 进入 `partial_results`，不会计入 `succeeded`。批次部分成功时以 `LBR-AGENT-018` 非零退出；结构化错误详情
保留成功摘要及逐项失败，失败 session id 使用哈希脱敏。单项归属、cwd、
已擦除与源授权失败分别保留 `LBR-AGENT-015`、`016`、`019`、`020`；provider 时间戳（含推导的 turn 时间）若超出导入主机时钟五分钟以上，会在影响 session 时序前被拒绝：该 selection 以固定消息 `the transcript contains timestamps beyond the permitted clock skew`（`LBR-AGENT-018`）失败且不写入任何耐久状态；失败详情
仍携带 `schema_version`，并原样保留 nullable `next_cursor`，不会把 `null` 改成 0。
Codex 批量发现同时覆盖配置的 Codex home 下 `sessions/YYYY/MM/DD` 和平铺的
`archived_sessions`。`--since` 使用有界首条 `session_meta` 记录中的 RFC3339
会话开始时间，不使用文件修改时间；会话虽在边界后续跑、但开始时间早于边界时仍被排除。
Claude 的发现仍按文件修改时间。`--since` 遇到缺失时间或无效首记录时，按哈希 session id
在 `skipped` 中报告 `codex_session_time_unknown` 或 `codex_session_header_invalid` 原因键；已打开来源的读取失败另用 `codex_session_source_unreadable`，
继续处理其它会话。每项发现跳过都会记录 warning；若批次本来成功，`--exit-code-on-warning` 返回 9，JSON 的 `data.warnings[]` 也包含警告。若 child-content 发现再次遇到同一坏文件（包括批次未选中的坏归档副本），父会话仍可能以 `LBR-AGENT-018` 部分失败。人类可读输出提示存在无法
筛选的 rollout，JSON 逐项列出原因。检查本地 rollout 后可用 `--session` 或
`--path` 重试。游标分页会重复报告这些哈希跳过项，以免续页隐藏它们；汇总各页计数时
应按哈希 session id 去重。首记录上限 1 MiB，累计 header 预算 256 MiB，无法筛选的记录最多 4096 条；任一累计上限耗尽都会以 `LBR-AGENT-018` 停止发现。`--since` 必须遍历整份本地历史并读取首记录；文件系统日期不能证明会话开始时间，所以时间窗口不降低 header 工作量。active 与 archive 树各有 20,000 项扫描预算，归档的 `--session` 最多先扫描 10,000 个 active rollout 文件，再在平铺目录中最多扫描 20,000 项；超过扫描预算同样返回 `LBR-AGENT-018`，不会静默漏掉会话。历史库过大时，可用封闭的 Codex SQLite 备份配合 `--codex-index-backup` 分页导入缺失会话；已有会话要刷新或需要指定单个 rollout 时，用 `--path`。
`--all` 的发现阶段不读取 header，各选中源仍由正常导入读取器校验。同一 session id
同时存在时优先 active 文件；同一根目录内优先文件名时间更新的 rollout。`--since`
在读取完整 transcript 前核对首记录 id 和有界 CLI 版本字段。JSON/非 TTY 调用必须指定 `--yes`；交互式批量调用会先征得同意再读
首记录。授权的 reasoning 摘要可从普通 checkpoint 导出取回，导入状态、metadata 和错误
不回显正文或源路径。
成功与 partial 摘要分别用 `checkpoints_written` 报告父 turn 写入、用
`subagent_checkpoints_written` 报告独立发现的 child-content 写入；只有 child
写入时状态仍为 `imported`，不是 `noop`。平台无法提供安全 child discovery 的
诊断本身不会让完整的父 transcript import 失败，因为它不能证明 child 内容存在。

历史导入严格限定当前仓库且 fail-closed：transcript 必须给出唯一无歧义的
`cwd`，Libra 解析其 storage 并要求与当前仓库相同。共享该 canonical storage
的 sibling linked worktree 属于同一仓库；其它仓库不属于。文件源必须位于所选
provider 的 protected root；Unix 使用 descriptor-relative no-follow 单次
打开。provider root 按 component 逐段安全打开；Claude 每个 source 与 Codex 的
year/month/day 各层都在同意前相对 pinned 目录 descriptor 打开，因此 root、嵌套
目录或 source-file symlink 均不能逃逸 provider root；没有等价安全打开的
平台直接拒绝。持久化内容仅含经过字段级脱敏的
coverage-v1 user/assistant/tool 类型投影；原始 provider envelope、provider-home
源路径和未知字段不会落盘；已验证仓库 `working_dir` 是文档化兼容例外。重放
幂等；incomplete turn 可前推为唯一 complete
revision，但不会改变 checkpoint 的结构仓库父节点。若之后有不同的 complete
payload 声明同一逻辑 turn，claim 会停在 `conflicted`，并在
`agent_coverage_conflict` 中只保留**第一个** challenger：typed canonical payload
先脱敏，再连同 digest、source channel、observed time 与确定性 redaction report
持久化；后续 challenger 不覆盖首份证据，raw provider envelope 与命中的密钥原文
永不落盘。operator 显式解决前，incumbent revision 保持 append-only 且仍为 current。
本地 session erase 会先
写耐久防复活 tombstone，再删除 catalog；自动发现和在途 writer 都不能重建。
唯一的本地绕过是经审计的 `--restore-erased --yes`。

新的 V2 导入只把来源保留为 repository-keyed、domain-separated 的
`source/hmac-v2/<64 lower-hex>` commitment。新的 V2 subagent content 则使用其独立
domain 的 `source/subagent-hmac-v2/<64-lower-hex>` commitment。raw source locator
或未加钥的 source SHA-256 不得进入 import/subagent 的 catalog metadata、claim、marker、
diagnostic 或 cloud record。可选的 V2 snapshot digest 是对脱敏 snapshot content
使用独立 domain 的 repository-keyed `source/hmac-v2/<64 lower-hex>` commitment；
helper 的瞬态 SHA-256 绝不持久化。bare 或带标签的未加钥 SHA-256 只可作为不可变
legacy evidence，不能写入新的 V2 或 cloud state。V1 行只可读作 proof，不能当作转换捷径：只有 exact
scoped proof 匹配且 identity、catalog 与可迁移 repair marker 都已 committed、
quiescent 时，Libra 才在同一原子事务中迁移它们。active、partial 或
repair-pending 的 V1 state 保持 V1，绝不创建并行 V2 record。

首次读取内容或执行 export 前，交互确认会显示 agent 范围、仅当前仓库边界、
候选数/上限、脱敏写入，以及后续 `libra agent push` 可能上传脱敏 traces 的
提示。`--yes` 只确认该隐私影响，不放宽 provider root、仓库、大小、deadline
或平台授权。跨 session 批次 best-effort；后续 turn 失败时仍精确报告该 session
已经耐久提交的进度。64 MiB 批次预算按 held descriptor 实际读取的字节计费；
成功、格式错误、未授权及超限候选均计费，也包括授权后文件继续增长的部分。
如果有界 child discovery 在返回可信字节数前失败，Libra 会保守地计入该 child
剩余的全部每源 allowance；重试不能利用 helper 失败绕过批次 cap。
120 秒绝对 deadline 在 discovery 前开始，并在 helper 阶段、遍历、解析、分批
reservation、对象构造与 CAS 前后检查。SQLite commit 一旦开始就不会被超时取消：Libra 在每次
commit 前立即检查 deadline，再等待该 commit 得到确定结果；因此最终 turn 已完整
提交时，即使等待结果期间越过 deadline 也仍报告成功。deadline 失败会 abandon
当前 owner 的全部未提交 lease/marker；若该恢复事务自身失败，命令会连同原错误
报告 cleanup 失败和可执行的 `agent doctor --repair` 提示，而不是静默吞掉。
discovery 和已授权 fd 读取 helper 都是可于超时杀死的私有进程。读取 helper 消费
父进程已经打开的同一 descriptor；其 control wire 只有固定的安全上限和 commitment，
绝不包含 locator、路径、provider session 值或 raw bytes。这些 helper 阶段共享该绝对
deadline。当前命令进程仍会在交接前同步执行 source 分类、canonicalize、安全打开和
rewind；此处 NFS/FUSE stall 无法被强制取消，因此 120 秒设置不是严格的端到端
wall-clock deadline。child discovery 使用较短的子 deadline，为独立有效的父
checkpoint 预留提交时间；父进程在遍历有界响应时也会持续复核该子 deadline。因此，
迟到或过大的 child 结果只会令父结果降级为 partial evidence，不会吞掉父 checkpoint。
consent 之后，持有 descriptor 的 preparation 阶段在可杀 helper 中完成 parse 和
redaction，且不能按 locator 重新打开 source。source-root 授权和初始安全打开在该
交接之前发生。checkpoint loose-object 写入以及 traces ref 拼接所需的 commit/tree
读取保留各自的有界路径。此设计保留 descriptor-bound raw read，却尚不能在单个
command/hook 进程中同时提供 strict deadline、FD-only wire 和 timeout 后的
autonomous replay；该组合需要 long-lived owner 或由 provider ABI 提供
pre-authorized descriptor，见 [Session Capture 计划](../../development/plan/plan-20260924.md)。
对象读取只接受请求的 `commit`/`tree` 类型，校验完整 OID 与声明长度，并在分配前执行
16 MiB 解压 payload 上限；恶意压缩对象不能把有界 helper 工作转化为无界内存占用。

批量 import 在进入下一个候选前，会等待当前已完成候选排队的 object-index 写入
落入 SQLite。若 drain 耗尽命令 deadline，partial 进度归属于当前候选，绝不错误
归到尚未开始的下一个候选。后台更新的终态错误同样是 barrier failure，不能只记
日志后假装成功。import 在 checkpoint 写入前取得 session 级耐久 repair barrier；
其中的 owner、generation 与 lease 会串行化并发进程，只有精确 owner/generation
可以退役它，索引失败也只能把产生该结果的精确 identity/fence 降为 partial。
超时或更新失败会令 barrier 进入 repair-pending，并把精确 identity 标为 partial；
进程崩溃则保留 active generation，lease 过期后的 takeover 必须先修复再写入。
replay 在允许返回 `noop` 前，会在可杀 helper 中幂等前台修复该 session 的完整 E4 object 集；helper
持有 SQLite writer slot，并在同一事务复核 session/erase tombstone、更新索引及保留
所属 barrier，因此并发 erase 后不会重新插入可上传行。成功候选只退役自己的
generation；若有界修复无法完成，运行 `libra agent doctor --repair` 后重试。

每个可能新建的 loose object 在写入前，都会先把 OID 作为 provisional preclaim
持久化到 attempt marker；只有本 writer 赢得不覆盖发布，并把 OID 耐久迁入
`created_oids` 后，才获得可删除的所有权。发布后、确认前崩溃会选择安全泄漏：
未确认 preclaim 永远不会被自动删除。
对象先压缩到共享私有目录 `objects/info/libra-tmp` 中的唯一临时文件，再以
不覆盖方式发布；若 final path 已存在，必须解压并逐字节验证后才可复用。后续写入
每次最多检查该私有目录 64 项，只清理超过 24 小时且名称精确匹配
`.<40-or-64-lowercase-hex-oid>.tmp-<decimal-pid>-<uuid>` 的普通文件；其它文件和目录保留。
只有启用 `--sync-data` 或 `LIBRA_SYNC_DATA` 时执行文件/目录 fsync；不覆盖原子发布
始终启用。在删除不可达 `object_index` 行前，`agent clean` 会取得仓库级 repair-marker
generation fence、重新核验每个候选 OID，并一直持有到 prune 事务提交。这样，即使 marker
在较早的命令 preflight 之后发布，cleanup 也会 fail-closed，而不会让其迟到队列更新复活
已删除行。启用 `--sync-data` 时，repair marker 退役还会 fsync 所在目录。正常 append、
失败 finalizer 与 erase 都不执行全仓库 reachability drain；
被拒绝 append 会把精确 generation 标为耐久 `cleanup_pending`，由
`agent doctor --repair` 忽略普通 writer TTL，立即执行有界、root-fenced 的 ownership
退役。同 session cleanup job 未退役时 erase 会拒绝；erase 本身
不运行该 drain。inline recovery 绝不 unlink 共享
loose object，也不删除其 `object_index` 行：worktree index writer 不共享 SQLite lock，
所以对象可达性证明与物理回收交给具备 grace/locking 策略的仓库 GC。doctor 覆盖
refs、reflog、全部已注册 worktree index、进行中的 sequencer 状态以及 marker/catalog
状态：先在 SQLite 写事务外快照，再在最终 ownership 退役事务内复核完整快照。由于
该路径不删除 payload 或 `object_index` 行，它不会遍历无关对象历史。每次 marker 注册还带
随机 writer `generation`；对象 preclaim、ownership 确认、最终 ref CAS 与 marker
清理都必须精确比较该 generation，过期 writer 不能接管或删除同 checkpoint 的
takeover marker。ref/reflog roots 最多收集 250,000 行；注册 index 快照位于 30 秒 deadline
控制的可杀 helper 中，以 no-follow/nonblocking 打开并要求普通文件，从持有 descriptor
执行一次 `limit + 1` 增长检查，再对同一批精确字节校验 checksum/解析。index 最多 256 个且合计
最多读取 64 MiB。任一上限触发时均 fail-closed 延后，保留 durable ownership
供诊断和后续重试；成功 drain 只退役 attempt ownership，孤儿 payload 留给仓库 GC。
lease takeover 后，零进度 provisional session 按其
持久化 `import_provisional` 标记清理；live/export 在 marker 注册后的失败会释放
claim/job lease，并且只清普通 marker，绝不清除 `cleanup_pending` job。

`agent clean --gc` 会物理删除最终 coverage 已被清除的终态、无 owner import identity，
也覆盖零 checkpoint identity；不会把它们重置为可再次 replay 的 `discovered`。
dry-run 在回滚事务中模拟 coverage 清理，因此 `import_identities_pruned` 与相同真实
GC 运行一致，同时不修改 catalog。
冲突证据跟随 claim 生命周期，不形成独立 retention root：erase session 或 prune
最终 coverage claim 会级联删除首个 challenger，dry-run 模拟同一删除；checkpoint
history rebuild/prune 将 current claim 回退到更早的存活 revision 时，会删除已经
失效的 challenger 证据，并把 claim 恢复为非冲突的 committed 状态。

tombstone 会传播。`libra cloud sync` 在 generation fence 下把它发布到 D1，
并删除该 session 在远端 catalog 的镜像行；`libra cloud restore` 则是**双向
tombstone 优先**——在 restore 事务内同时过滤带远端 tombstone 的行与匹配本仓库
`agent_import_tombstone` 的行。因此本机 erase 之后、再从尚未见过该 tombstone
的镜像 restore，也不会把 session 复活。`cloud sync` 在任何远端写入之前把
capture catalog 与快照的其余部分一起冻结，只有对象上传之后当前 catalog 仍与冻结
副本一致时才发布该副本；因此同步进行期间被擦除的 session、被 prune 的 checkpoint
或任何改动过的 catalog 行，都不会从较早的快照发布出去：agent-capture 阶段被拒绝并
提示重试，不写任何远端 catalog 行（见 [`cloud`](cloud.md)）。

**仍 deferred 的只有 R2 物理删除**：已擦除内容的 payload 仍留在 R2，未见过该
tombstone 的另一台机器仍可取回。所以 `agent erase` 应视为对**本仓库**不可逆，
而不是「所有副本都已消失」的跨机保证。

私有 capture recovery 证据只保存在本仓库。local erase 先提交防复活 tombstone，
再 prune checkpoint history，最后在同一 catalog 删除事务清除可归属的 recovery
headers/chunks 和 session aliases。删除依据本地 catalog PK/incarnation 归属，
不依赖 recovery MAC 或可被淘汰的 receipt；缺 key 不阻止可归属清理。foreign 或
无法归属的损坏证据保留，提交后只记录不含内容的 lost-capacity 诊断；它仍占用容量，
并可能阻止创建新关联。没有一致性的原 repository DB 备份时，这部分容量明确 lost，
doctor/retry 不提供 force-discard。整份 DB-copy 备份含敏感 alias 关联，不是匿名化
备份；恢复旧完整 DB 备份可能撤销删除及 tombstone。正常 receipt Complete 则在同一
事务回收对应 artifact，只有无剩余引用时才删除 alias。

pending 与 quarantine header 若冲突于同一 checkpoint key，清理只删除已确认
属于本 session 的 header；未知/外来 header 及共享 chunks 原样保留。explicit
erase 会报告保留容量，不证明每一份私有副本都已删除。

session incarnation 不可读，或同 PK 存在其他 incarnation 的已知 alias 时，最终
删除会拒绝：恢复一致的 session/alias metadata 后重试；第一阶段 tombstone 仍生效。
只要保留一份不可解码 header，就会阻断本仓库所有新建/重投 recovery artifact 及
destructive GC/root-walking maintenance，而不只是损失其自己的槽位。该 header
阻止证明无引用时，Complete 也会保留 alias；可归属的 explicit erase 仍可执行。
out-of-band 超容量状态最多读 17 份 header 和 17 份关联；未检查部分可能仍含本
session 的内容。警告不是「所有私有副本已删除」的证明。

`agent session list --json` 与 `agent checkpoint list --json` 每次返回一页：`data` 携带 `schema_version`、位于 `sessions` / `checkpoints` 下的行（单行结构不变），以及 `next_cursor`——传回 `--cursor` 的不透明游标，列表耗尽时为 `null`。页序为最新在前（`started_at` / `created_at` 降序，行 id 作为并列时的次序键）。

人类可读的 `agent session list` 表格会把 `started_at` 按当前机器时钟显示为相对时间（例如 `2 hours ago`）；JSON 输出仍保留原始 Unix 时间戳，供自动化使用。

`agent checkpoint show <id>` 刻意不是 metadata dump。默认的人类与 JSON 输出只含固定的 checkpoint 摘要：`checkpoint_id`、受限词表中的 `scope`、Unix `created_at`，以及是否记录了 parent snapshot。它绝不读取或渲染 `metadata.json`、session 标识、source locator 或 commitment、redaction 细节或 catalog 对象 ID。transcript 内容只能走显式的 checkpoint export 路径，并由该路径执行自己的授权和脱敏策略；不要把默认 `show` 输出当作内部 metadata 接口。

每个 checkpoint 行携带 `scope`。`committed` checkpoint 在 turn/session 边界（`Stop` / `SessionEnd`）写入，携带脱敏的 transcript 快照。`subagent` 下有两类刻意分离的证据：Codex 的 `SubagentStart` / `SubagentEnd` hook 写空 transcript 的 **boundary checkpoint**，并以 `parent_checkpoint_id` 表达结构归属；Claude `<session>/subagents/*.jsonl` 则逐文件写独立的 **content checkpoint**，其 `parent_checkpoint_id` 保持 null。新的 content claim 只会在安全打开 source 并重新核验 root、storage 绑定、scope 与 workspace fence 后，写入仓库密钥、域隔离的 HMAC V2 commitment（`source/subagent-hmac-v2/<64-lower-hex>`）；append-only revision 据此选出唯一 current content leaf，本地 project slug/文件名不落库，物理历史也不改写。历史 V1 `source/sha256/...` claim、revision 与 checkpoint metadata 是不可变、只读证据；命中同一 legacy claim 时不会静默创建并行 V2 行，必须继续走 recovery/`agent doctor` 路径。Claude 当前不提供能与 boundary 对齐的稳定 ID，因此常态是 `link_state=unresolved`；`agent doctor` 会报告但不会猜测。只有 provider 给出稳定 ID 且唯一匹配时，才在独立 association 行中关联 boundary，不修改不可变 traces commit。两类证据均可 list/show/export/prune，且 doctor 可见。

当前源码构建也把验证过的 Codex child rollout 保存为独立 content checkpoint。
`SubagentStop` 与父 checkpoint 边界、经同意的历史导入和 pending 恢复复用同一
collector。读取范围为绝对 `CODEX_HOME`，相对值则回退配置 home 下的 .codex；打开文件后
验证 header 的 root、当前 thread、immediate parent、child 和仓库 cwd。
nested child 的直接父会话可以不同于 root。legacy 根 header 缺 session_id 时可
回退自身 id；child 仍须提供 root，单靠路径或 child ID 不能授权读取。

child 按既有有界保留合同保存实际返回的 reasoning summary 正文、`encrypted_content` 和未知普通字段。
用 `agent checkpoint export <content-checkpoint-id>` 读取，show 仍是摘要。
opaque 保留不等于解密或可读 thinking。active/archive 移动及相同内容在
live/import/recovery 间重放复用同一 revision，新字节可以追加 revision；
删除 provider 源不会删除已存内容。

只有本次 scope、source digest、已结算 claim 与 observed turn 同时满足的
`SubagentStop` proof 才能关联对应 boundary。同 CID 的另一 turn 不会夺取旧关联。
同 callback 的晚到重放可补正文和关联，原 boundary 对象保持不可变。Libra 不会
重新投递宿主 callback；后续父采集、导入或恢复仍可保存晚到正文，而不据此建立旧边界关联。
没有 proof 时仍保存可导出的 unresolved 内容；历史导入本身不证明 live 关联
或 native 来源。

UUID 形状的 Codex 会话在 `SessionEnd` 等待已知必要 child claim 结算。provider 根
不可用时保留 pending；没有已知未结算 claim 的部分发现可以带 partial 提示完成。
managed 父 checkpoint 即使发现零 child 也可能带不完整提取提示，该固定 omission
提示不证明 deadline 已到期。旧任意 label callback 保留原 parent-only live 路径；
不新增 child dispatch 或 claim；已到期的发现窗口仍可能带固定 skip/不完整提取提示。
该 live 谓词不为 stored 导入/恢复新增标识限制；原有来源/selector 校验仍有效，
包括公共 `--path` import 对文件名 UUID 的要求。UUID 或 partial 均不证明
native 来源。用 `agent session show` 与 `agent doctor` 查看状态，恢复必要源后重试；
重试不能还原已删除源。worker 仍按原两秒 batch、至多五次 artifact 尝试工作，
不承诺后台最终必达。

Codex 的 `config.toml` 可用 `model_reasoning_summary = "detailed"` 和
`model_supports_reasoning_summaries = true` 请求可读摘要，实际是否返回仍取决于
model/provider。空摘要不等于 Libra 丢失正文，比对前应先解码 export 中的 JSON，并与实际源比较。
本节源码改动仍在本机验收中，不代表已发布运行时保证。

当前 Codex 捕获安装器登记以下生命周期事件：`SessionStart`、`SessionEnd`、`UserPromptSubmit`、`PreToolUse`、`PermissionRequest`、`PostToolUse`、`PreCompact`、`PostCompact`、`Stop`、`Interrupt`、`SubagentStart` 与 `SubagentStop`。checkpoint 的事件投影会保留 `turn_id`、`tool_use_id`、`agent_id`、`agent_type`、压缩触发原因、权限模式、工具输入和工具输出等关联字段。hook 事件投影不会读取 rollout reasoning；经同意的历史导入遵守上方普通保留合同，保存 opaque 值无需解密密钥。

持久化的 checkpoint 元数据只保留 Libra 能安全分类的字段。live hook capture 在任何持久化之前就会丢弃 provider 提供的 `model`、`source`、`tool_name` 与 session-reference（transcript 指针）字段，因此读取 `checkpoint export` 输出或 `refs/libra/traces` sidecar 的消费者不应再期待这些字段：在 hook 捕获的 checkpoint 中，`metadata.json` 始终记录 `"model": "unknown"`，其 `events/lifecycle.jsonl` 行从不包含 `model` 或 `tool_name` 且记录 `"source": null`，Codex subagent 边界元数据中的 `subagent.tool` / `subagent.source` 为 null。hook 捕获的 lifecycle 行的 `provenance.hook_event_name` 是 canonical snake_case lifecycle kind（例如 `tool_use` 或 `turn_end`，与 `kind` 一致），而不是 provider 的原生事件名。`events/lifecycle.jsonl` 为 schema v2：每行都带必填的 `identity_scheme`（`native_replay_hmac_v2`、`fallback_action_hmac_v1`、`generic_lifecycle_uuid_v5` 或 `import_uuid_v5`），声明其 `event_id` 的派生方式；它只是声明，而不是可验证的 replay 凭据。

云端镜像在 token-fenced generation 下按依赖顺序发布 catalog 批次（`session → checkpoint → revision → link → claim`）。session、checkpoint、link 与可变 claim 投影各自使用独立单调 sync generation，因此 prune 回退 current 子节点时不会制造同代冲突，retained traces 链的重写也不会被旧 clone 写回。显式 `--restore-erased` import 会为 session 与 opaque child-source namespace 启动新的耐久复制 incarnation；仅 R2 payload 物理删除仍 deferred 时也不会复用旧 key；D1 tombstone 传播已生效。generation 同时绑定远端 object index 的 canonical digest；sync 会在 D1 与 R2 复验每个 checkpoint 对象，restore 只有在读取前后 manifest 与 object-index digest 均未变化时才接受 catalog。当前客户端只使用版本化的 v2 远端 session/checkpoint 表；v2 激活后，D1 trigger 会以升级提示拒绝旧客户端的无 fence 写入，避免其破坏一致恢复快照。

## Claude Code hook 事件注册表

Libra 只用一份 Claude Code hook 事件注册表。空 stdout 且 exit 0 不会接管宿主动作的名字才是可安全安装的。`WorktreeCreate`、`WorktreeRemove` 和 `PreModelSwitch` 是控制敏感项，不会安装。`SessionStop`、`ModelUpdate` 和 `Compaction` 是已废弃别名，不在当前可安装集合里。未知事件名或未知字段记为 partial，诊断不包含 payload。`libra agent enable` 写入每一个可安全安装的事件。`SessionEnd` 不设置 timeout。`FileChanged` 不写 matcher，也不发出 `watchPaths`。`libra agent doctor` 把每个已知事件列为可安装或排除，并说明是否已有 Libra 拥有的配置。`libra agent hooks claude-code observe` 从 stdin 读一帧 JSON，把可安全安装的事件交给既有采集协调器，并且不打印内容。可控的成功和失败都是 exit 0。`WorktreeCreate`、`WorktreeRemove` 和 `PreModelSwitch` 不会安装，也不会输出路径或决定。MessageDisplay 每个会话跨 hook 进程最多保留 128 批。之后的批次不写采集 revision，只增加丢弃计数，会话 erase 时一并删除。需要 Libra 0.30.66 或更高版本。这不表示每个已知事件都已安装。

已授权的 Claude Code transcript 会把非空 `thinking` 字符串留在普通 snapshot 和 coverage 投影里。`thinking.signature` 与 `redacted_thinking.data` 留在普通 transcript 字节中；没有 native 证明时标为 Unproven。空的 thinking 字符串和缺键不同，空壳不算读取成功。没有 reasoning 记录的 turn 仍用 coverage schema 1，出现可读 thinking 记录时用 2。会话 metadata 和脱敏报告不含 thinking 正文。授权的 `libra agent checkpoint show --json` 和普通 checkpoint export 会返回 thinking 正文与 `thinking.signature`，并标为 unproven。`checkpoint list`、`session list` 和 `agent graph` 不打印正文。实时采集、导入和 snapshot 恢复从原始 JSONL 读取这些附带字段，并带上 provider、session、turn、record 和 content index。重复键拒绝该记录，已经保留的字节留在原地。会话文件不匹配则不关联。这种普通保留不创建 native artifact。显式 `libra agent doctor --repair` 为已授权的旧 Claude checkpoint 追加一条 coverage revision。这条 revision 的幂等键绑定该 checkpoint 和普通 transcript 内容。相同内容再次修复不会改写 revision，也不会改动已存储对象。erase 和云端 mirror 同时覆盖这条 replacement 与旧 checkpoint 引用，tombstone 阻止复活。

## 示例

```bash
# 显示已捕获会话数量和最近 checkpoint 摘要
libra agent status

# 显示代理能力矩阵（支持 roster、hook、安装状态）
libra agent list

# 协商带 import/export 方法的版本化能力矩阵
libra agent list --schema-version 2 --json

# 明确同意后导入一个历史 Claude Code session
libra agent import --session <provider-session-id> --agent claude-code --yes

# 按 session_meta 会话时间导入一页 active 或 archived Codex rollout
libra agent import --since 2026-07-01T00:00:00Z --agent codex --limit 20 --yes --json

# 以冻结 JSON v1 schema 读取一个捕获 session 的 turn/revision/subagent 结构
libra --json agent graph <session-id>

# 在自动化中或从另一个仓库安全读取同一图
libra --json agent graph <session-id> --repo /path/to/repo

# 启用 Claude Code 捕获并安装它的 hook（enable 的别名）
libra agent add claude-code

# 启用 Claude Code 捕获并安装它的 hook
libra agent enable --agent claude

# 以 60 秒宿主 timeout 安装 Claude Code hook（采集预算 59000ms）
libra agent enable --agent claude --hook-timeout 60

# 一次启用所有支持的代理
libra agent enable

# 禁用 Claude Code 捕获并卸载它的 hook（disable 的别名）
libra agent remove claude-code

# 移除历史 gemini hook（仅卸载通道；幂等）
libra agent remove gemini

# 禁用 Claude Code 捕获并卸载它的 hook
libra agent disable --agent claude

# 列出已捕获会话
libra agent session list

# 显示一个会话并复制其当前已验证的 Claude Code transcript 来源
libra agent session show <session-id> --extract-transcript /tmp/session.jsonl

# 停止一个已捕获会话
libra agent session stop <session-id>

# 继续一个已停止的已捕获会话
libra agent session resume <session-id>

# 列出已捕获 checkpoint
libra agent checkpoint list

# 分页浏览 checkpoint（默认每页 50；JSON 携带 next_cursor）
libra agent checkpoint list --limit 100
libra agent checkpoint list --cursor <next_cursor>

# 显示 checkpoint 的安全结构摘要
libra agent checkpoint show <id>

# 将 checkpoint 回放为 JSONL transcript
libra agent checkpoint rewind <id>

# 从最近停止的会话中丢弃临时 checkpoint
libra agent clean

# 从每个已停止会话中丢弃临时 checkpoint
libra agent clean --all

# 诊断 hook 安装和捕获状态
libra agent doctor

# 将 refs/libra/traces 推送到默认远程
libra agent push

# 将 refs/libra/traces 推送到具名远程
libra agent push --remote origin

# `libra agent clean` 重写 traces 链后重新推送（force-with-lease）
libra agent push --force-rewrite

# 发现 PATH 上的 libra-agent-<name> RPC 二进制文件
libra agent rpc list

# 在 libra-agent-<slug> 二进制文件上调用单个 JSON-RPC 方法
libra agent rpc invoke <slug> <method> --params '<json>'

# 在 stdio 上运行 DeepSeek Harness 桥接（JSON-RPC 2.0 NDJSON）；
# 喂入一个 fixture，stdout 每条响应恰好一帧
libra agent bridge --stdio < bridge-initialize.ndjson

# 在 stdio 上提供 legacy MCP。stdout 只含 JSON-RPC。
libra agent mcp --stdio

# 面向代理的结构化 JSON 信封
libra agent --json status
```

`libra agent --help` 会渲染同一横幅，因此文档和 CLI 表面保持同步（跨命令 `--help` EXAMPLES 推出，见 `docs/development/commands/_general.md` 条目 B）。

## 延后 parity（非目标）

以下 external-agent parity 表面在本波次**有意不**公开。它们连同处理方式与重启条件一并记录在 Agent tracing 契约（[`../../development/tracing/agent.md`](../../development/tracing/agent.md) 的「还未实现的功能」表）中，在此点名以免脚本或用户产生依赖：

- **`agent add`/`remove` 的 `--local-dev` / `--force` 标志**未发布——只使用规范的 `enable` / `disable`（及其 `add` / `remove` 别名）。若未来发布，会同时接到规范动词及其别名上。
- **Provider 专属 transcript compaction / reassemble trait** 是未来 parity 项。writer 已把大 transcript 存为 manifest 相对 chunk，但尚无 provider 专属 compactor/reassembler。
- **可选 capability trait**（`ProtectedFilesProvider`、`TranscriptCompactor`、`HookResponseWriter`、`RestoredSessionPathResolver` 等）在已落地的 `DeclaredAgentCaps` 矩阵之外尚无公开行为。
- **v2 `info` / capability gate 之外的 external-RPC method family** 未实现；未声明某项 capability 的代理继续 fail-closed。
- **非首批 roster 不受支持。** 仅 `claude-code`、`codex` 与 `opencode` 为 supported、hook-installable 且可用于 review/investigate 启动，另加上文的 `grok` 与 `hermes`。`gemini`（仅 uninstall，见上文说明）、`cursor`、`copilot` 与 `factory-ai` 均为 `supported=false`，不出现在 `agent list`，`add`/`enable` 返回可操作的 unsupported 错误；它们不可启动。`hermes` 可以本地采集，但不能启动 review 或 investigate。`pi` 是上文所述 `supported=false` 的已注册 identity：仓库 CHECK 接受它，但目前没有任何写入者写入 `pi` 会话。

## 说明

- 外部 `libra-agent-*` 代理**默认禁用**。使用 `libra config set agent.external_agents.enabled true`（仓库级）显式开启；开启前 `rpc list`/`libra-agent-*` 的 `rpc trust`/`rpc invoke` 会以 `LBR-AGENT-002` 拒绝（`rpc untrust` 始终可用——撤销信任只会收紧安全面；`rpc trust --dir <path>` 与 provider-exporter 信任如 `rpc trust opencode` 属纯准备动作——不扫描 `$PATH`、本身不启用任何东西，因此门控下亦可用）。已发现的二进制在 `rpc trust <slug>` 记录来源前保持隔离（world-writable 目录中的二进制拒绝信任）；每次 invoke 都会复验来源（漂移即撤销信任，`LBR-AGENT-005`）；子进程环境被清空为白名单注入，stderr 被捕获/限长/脱敏——绝不继承。invoke 超时、broken pipe、malformed frame 映射 `LBR-AGENT-012`；IO 硬上限超限映射 `LBR-AGENT-007`。

- `libra agent enable` 为 Claude Code 与 Codex 安装的 hook 配置调用顶层 `libra hooks claude|codex <verb>`，并带有 installer 根据 handler timeout 生成的受限隐藏 `--capture-budget-ms` 参数（默认 Claude 与 Codex 普通事件均为 `30s → 29000`；Claude `SessionEnd` 不设宿主 timeout，保持 `9000`；Codex `SessionEnd` 与 `Interrupt` 因宿主三秒上限为 `3s → 2000`；一秒的历史 handler 仍保留可用的 `500ms` capture 切片，另保留最多 1000ms 用于 terminal finalizer 清理）。可用 `libra agent enable --hook-timeout <seconds>` 选择其他 handler timeout；由此前 Claude 默认值（`10s → 9000`）或其他 timeout 写入的 hook 仍归 Libra 所有，会被原位替换。重跑 enable 会刷新旧配置，隐藏别名也接受该参数以兼容历史已安装命令。已安装的 OpenCode plugin（`.opencode/plugin/libra-hooks.js`）则通过隐藏的 `libra agent hooks opencode <verb>` 入口转发所有事件，不带 installer budget；其 capture deadline 为 OpenCode export deadline。Claude 与隐藏别名在 hook envelope 未通过大小 / UTF-8 / JSON / schema / reported cwd / transcript 路径校验时，会以 `LBR-AGENT-008`（退出码 128）fail-closed 拒绝，且绝不回显 raw stdin。两个路径类字段会在存储解析前限制为 4096 字节。已安装的 Codex 表面会有意确认 malformed/unbound callback 与辅助性的 nonterminal 捕获失败，以免失败阻断宿主任务：通用 ingress、scope 和存储失败只输出经净化诊断且不创建 capture 行。在任何 Libra 仓库之外触发、格式正确的回调没有可绑定的 scope：已安装的 Codex hook 会确认它（退出码 `0`，包括 `SessionEnd`），而 Claude 与隐藏别名返回固定、不含路径的仓库未找到错误 `LBR-REPO-001`（退出码 128）。已损坏的活动仓库不视为“仓库之外”：Claude、隐藏别名（Claude Code / OpenCode / Codex）、gemini 入口以及已安装 Codex 的 `SessionEnd`（仍以非零退出）返回与其他所有仓库命令相同、不含路径的 `LBR-REPO-003`（无法解析的 linked worktree storage，附 `libra worktree repair --confirm --path <worktree-path>` 修复建议）、`LBR-REPO-002`（数据库缺失或 object format 不受支持）或 `LBR-IO-001`（数据库无法打开——包括由更新版本 Libra 写入——或 object format 无法读取），绝不返回通用的 `LBR-INTERNAL-001` capture 失败（见 `libra hooks`）。与其他所有仓库命令一样，这些入口打开数据库时会自动应用待处理的仓库 schema 迁移。但已校验且已绑定 scope 的 `SessionEnd` 必须持久化或观察到 durable terminal completion/pending receipt；在该证据存在前发生的有界数据库/finalizer 失败会以非零退出，而不会被静默确认。已有 catalog reservation 后的狭义 checkpoint 或 maintenance-lock 失败，才可能仅持久化一条不含敏感内容、可重试的 catalog 诊断。该预算约束可杀 helper、数据库和其它 deadline-aware 阶段；它不是严格的 whole-process host deadline，因为 source 分类、canonicalize、安全打开和 rewind 当前仍在 descriptor 交接前同步发生。NFS/FUSE stall 因而无法被强制取消，这条路径也不承诺 timeout 后的 FD-only autonomous replay。对不一致 store 执行 checkpoint 操作（如 `checkpoint rewind`）——catalog 行的 `parent_commit` 非法或指向缺失的 traces 对象——会以 `LBR-AGENT-009`（退出码 128）失败；运行 `libra agent doctor` 检查 store。
- 受管 Session Capture 支持 Unix 和原生 Windows 安全初始化 repository-private deduplication key。在无法提供这些安全原语的平台上，会在创建 key file 前 fail-closed 并给出可操作的诊断，而不会使用较弱的 path-based publication fallback。该固定能力诊断会显式到达两个 Codex 入口：nonterminal `hooks codex` callback（包括隐藏历史 alias）向 stderr 输出不含路径的平台 remedy 但以 `0` 退出；不受支持的非 Windows 平台使用 Unix-host remedy，原生 Windows 初始化失败则提示使用匹配的 `libra.exe` helper 和本地存储；`SessionEnd` 以同一 remedy 非零退出。fail-closed 的已安装 Claude 表面与隐藏的 `agent hooks` Claude Code / OpenCode 别名对每个事件都携带同一 remedy 与 `LBR-UNSUPPORTED-001` 非零退出。这个例外不改变 alias 对 malformed envelope 的 fail-closed 契约。它只适用于在 Libra 仓库内触发的回调：在任何 Libra 仓库之外触发的回调在所有平台（无论是否为 Unix）上都遵循上文的仓库外契约。
- `checkpoint rewind --apply` 只恢复工作树文件；代理自身的 transcript 文件不会被重写。
- Hook 和捕获诊断采用 best-effort 方式，设计目标是报告可操作的安装状态，而不是静默忽略缺失的提供商。

### Doctor checkpoint 存储修复（`--repair`）

`libra agent doctor` 按 AG-20 修复矩阵扫描 checkpoint 存储及 writer marker registry；不带 `--repair` 时严格只读，仅报告 `--repair` 将执行的动作：


OpenCode 的 JSON hook 状态新增 `provider_hooks[].events[]`，每项包含 `event`
及与受管插件相同的可空 `installed`：`true` 表示安装了精确当前模板，`false`
表示缺失或过时，`null` 表示无法检查。人类输出逐项标为 `(managed template)`。
映射来自既有事件 registry，含 native bus、合并 inbox 及已接纳的兼容事件；
`session.error` 仍跳过。这只描述已安装映射，不证明 OpenCode 已运行或发过该事件。
模板过时后执行 `libra agent add opencode`，再启动新会话。

打开插件前，doctor 用 no-follow 元数据检查 `.opencode`、`plugin`/`plugins`
父目录及两个受管目标。symlink、FIFO/socket/device、文件父路径或目录目标会报告
固定 slot/type 原因及未知安装状态；包括 `--repair` 在内，不打开或修改该节点、
不输出正文或链接目标，也不误报 installed。仍假设目录名称稳定，不修复已延期的
并发目录移动问题。

`checkpoint_store.opencode_legacy_reasoning_checkpoints` 列出绑定本地 OpenCode
元数据中缺少非 null `reasoning_state` 字段的 canonical checkpoint ID。人类输出
标为 `legacy/unrecorded reasoning state; not evidence of complete capture or native origin`。
这是字段缺失诊断，不代表年代或 schema 版本；当前 checkpoint 也可能没有该字段，
字段存在本身同样不证明 reasoning 有效或来源可信。既有 coverage 完整性不提升、
不重写。最多检查 64 个 OpenCode catalog 行、每行 64 KiB 经校验元数据（最坏载荷
4 MiB）；对象不可用、超限、绑定不符或行数超限会显示不含正文的扫描不完整 note。
空列表不代表完整采集；不读取 transcript 或 reasoning artifact 正文。标记仅在读取时
计算，`--repair` 不改它；移除此诊断不需要数据迁移。此诊断随包含 OG-06 的版本首次
发布，源码交付本身不代表已发稳定版。


| `inconsistency_type` | 含义 | `--repair` 动作 |
|----------------------|------|----------------|
| `stale_catalog_row` | `agent_checkpoint` 行的 `traces_commit`/`tree_oid`/`metadata_blob_oid` 与仍可从 `refs/libra/traces` 到达的 checkpoint 不一致 | 从 ref 重建该行的 OID 列（幂等 UPDATE） |
| `missing_objects` | checkpoint 对象在对象库中真正缺失（且无法从 ref 重建）——best-effort 的对象/manifest 枚举覆盖 E4 协议树：`manifest.json`、`events/lifecycle.jsonl`、`transcript/<agent_kind>.jsonl`（含分片）、`redaction_report.json`、`content_hash.txt` 与中间 tree。它只下钻协议拥有的 `events`/`transcript` 子树，并限制 tree 条目、对象总数和 manifest 声明数；命中上限即标记 `manual_required`，绝不做部分修复。它不解析或验证 lifecycle 行 schema、UUID 派生或 `identity_scheme` 值。 | 无——标记 `manual_required`；doctor 绝不执行破坏性动作（可尝试 `libra fsck --heal` 或从云端/备份恢复） |
| `missing_catalog_row` | ref 可达的 checkpoint 没有 catalog 行（崩溃窗口 B 残留） | 通过 writer 同款「先探测再插入」的幂等路径重插该行，字段从 commit 的 `metadata.json` 重建（v1 与 v2 两种 shape 均可解析） |
| `missing_object_index` | checkpoint 对象在 `object_index` 中缺行（`libra cloud sync` 看不到）——覆盖 traces commit 加有界的 E4 协议对象集 | 按 writer 行语义幂等补插（tree 记 `tree`，transcript blob 记 `agent_transcript`，sidecar 记 `blob`）。修复大小前 doctor 会在 transcript cap 内流式完整性校验已持有的本地 loose object；绝不采用 manifest `byte_len`，也不渲染 payload。 |
| `expired_inflight_marker` | 有效 traces writer marker 已过 TTL，包括 provisional preclaim 与已确认新建 loose-object OID 的崩溃残留 | 在最终 ref 事务中 fence 过期 writer，执行串行化的全仓库 root 证明并退役 marker；inline recovery 不 unlink 共享 payload、不删除 `object_index`，物理回收交给仓库 GC |
| `invalid_inflight_marker` | marker JSON、行身份、commit 或 OID 损坏 | 无——标记 `manual_required`；无法解码所有权时自动删除不安全 |
| `conflicted_coverage_claim` | 两个不同的完整 payload 声明了同一逻辑 turn；doctor 只报告每次报告内的不透明序号、受限的 schema/revision/completeness 事实，以及已保留 challenger 脱敏证据这一事实 | 无——标记 `manual_required`；检查耐久脱敏候选并显式选择恢复方案，不能静默丢弃 provenance；`--repair` 绝不替 operator 选择 winner |
| `inconsistent_subagent_content` | current 子代理 content claim 与其不可变 revision、checkpoint catalog 行或 association link 缺失或不一致 | 无——标记 `manual_required`；在 companion 关系及 checkpoint 对象/ref 可达性恢复前，unchanged replay 一律 fail-closed |
| `unresolved_subagent_link` | current 子代理 content checkpoint 没有唯一、provider-stable 的 boundary 匹配（Claude 的常态） | 无——标记 `manual_required`；content 与 boundary 证据分别保留，doctor 绝不猜测关联 |

补充规则：

- 仓库与数据库失败沿用共享的仓库契约（exit 128）。在仓库之外运行时，doctor 以 `LBR-REPO-001` 失败，并给出与其他仓库命令相同的 `libra init` / Git 转换提示；已脱离（detached）、迁移中或损坏的 linked worktree 以 `LBR-REPO-003` 失败，并原样给出该 worktree 的修复指引。doctor 自行打开仓库数据库：数据库缺失为 `LBR-REPO-002`，无法打开（包括由更新版本 Libra 写入）为 `LBR-IO-001`，不支持的 `core.objectformat` 为 `LBR-REPO-002`。这些数据库错误消息不包含数据库路径，也不回显存储的 object format 值。
- **legacy-v1 checkpoint**（升级前布局，无 `manifest.json`）计入 `legacy_v1_checkpoints`，永不进入 checkpoint 对象修复类别，也永不被 `--repair` 改写。
- pending terminal-finalizer 诊断使用只读 **keyset pagination**：每页 32 个 session，每次最多扫描 512 个 session、返回 128 个 receipt finding，receipt metadata 在解码前限制为 1 MiB。坏行通过固定、无敏感内容的 note 报告，并继续扫描后续有效行；跳过坏 key 或达到上限意味着报告不完整，不能认为未列出的 receipt 健康或已修复。
- 没有 durable artifact 的 receipt 属于 **`pending_source`** 证据；doctor 不会重开 provider source 或制造可重放 snapshot。后续 stop/resume 使 receipt superseded 后，它仍标为 `manual_required`，session-quarantined receipt 也是如此；即使 `--repair` 也不改写当前 session 或原 receipt，quarantine 不代表成功捕获。有 artifact 的 receipt 达到原始重试上限时，quarantine routing 只移动私有 artifact header，保留原 receipt 的 revision、status、counter、generation 和 source fence。符合条件的 authenticated artifact 只有在 alias、receipt、marker 和 coverage fences 全部复核后才会本地重放。单次 `doctor --repair` 最多计入五次重放尝试；自动重放共享一个 2 秒 cooperative deadline，每次 audited 人工尝试各自取得新的 2 秒 deadline；因上限或 retry-later 而延期的 finding 不计数、保持未修复，重新运行命令可继续有界队列。只有 checkpoint durable 写入且原 terminal receipt 完成后，finding 才标记为已修复。过期 artifact 隔离后仅允许一次 audited manual replay；失败或 fence 过期的尝试仍需人工恢复。永久拒绝的自动 replay 只把 artifact header 停放到 quarantine；原始重试/时间上限到期前，doctor 不会自动重试该 header，也不会让它消耗后续 replay 配额。
- schema-v1 lifecycle JSONL 行没有类型化 `identity_scheme`，始终是 opaque legacy evidence。`doctor`、`checkpoint show` 与 `checkpoint export` 不会从这类行或 event UUID 推断 HMAC/replay 信任。
- 被**存活的 traces in-flight marker** 覆盖的 checkpoint 是写入中的 writer，不算不一致，会被跳过。
- finding 标识保持可操作，但绝不回显损坏输入：checkpoint-store finding 携带规范 checkpoint UUID（in-flight marker 类别为 writer attempt 的 checkpoint id，该 checkpoint 可能从未发布），`findings_store` finding 携带可用于 `libra review show` / `libra investigate show` 的 review/investigate `run_id`。存储的 checkpoint 标识若不是规范 UUID，只显示为 `checkpoint-id-redacted` 或每次报告内的不透明标签；`conflicted_coverage_claim` 始终使用每次报告内的序号。
- **过期的 terminal writer marker** 不授权将后来的 transcript 重绑到其 pending receipt。`--repair` 只退役陈旧 marker 的所有权，绝不改变 receipt 的 source 或 marker fence。只有原来的已授权 source 仍可用时，才可重跑原 hook/replay；同一 native terminal 事件若携带变更后的 source，会以 `source_digest_conflict` 隔离并要求人工恢复，绝不发布 checkpoint、traces ref 或 stopped 状态。
- **没有 checkpoint 的 session 是合法中间态**（active session 尚未产生首个 stop），绝不被标记；只有 checkpoint-without-session 才算 orphan。
- 已捕获的 **gemini 行保持可读**且绝不被标记；残留的 gemini hook **配置**会得到指向仅卸载通道（`libra agent remove gemini`）的提示。
- 对属于 **scoped** captured session 的 catalog 行修复，会把该 session 的 durable workspace lease 复核放在最终数据库写入处。若 lease 已 release、过期或被重新 fence，doctor 保持 finding 未修复并标为 `manual_required`；只有显式 `legacy_unknown` 行继续走历史上的无 fence 兼容修复路径。
- 所有修复均幂等——连续两次运行 `doctor --repair`，第二次不会做任何事。带 `--repair` 时，每次修复尝试发出一个 `agent.doctor.repair` tracing span（`inconsistency_type`、`repaired`、`manual_required`），transcript 内容绝不进入日志。

## Reasoning type contract (RG-01)

reasoning 类型契约区分受限元数据、经来源验证的内存 opaque 字节和
持久化 artifact；当前类型契约不改变上述运行时采集行为。
五态是 `provider_visible`、`encrypted_unavailable`、`opaque_archived`、
`not_present` 和 `unsupported_shape`。`encrypted_unavailable` 表示 provider
已声明密文但没有获授权的 decryptor，**不是采集失败**。未知 reasoning-like
形状在后续 provider 接线时必须使 turn partial 并写无 payload warning。
被选入密文字段校验器的 text/tool 块也按 `unsupported_shape` 拒绝，
即使带有未声明的 signature-like 键；普通内容须在调用校验器前分派。
`OpaqueEncryptedBytes` 不能序列化进普通 transcript，也不能转换为
`RedactedBytes`。只有 reasoning 模块能在已授权的来源上验证 Claude assistant
`thinking.signature` / `redacted_thinking.data` 的字段形状与 JSON 字符串类型后
构造该内存类型；原始转义 UTF-8 字节不重序列化、不做 base64 解码。
形状正确的 JSON 不是 provider 身份凭据。OpenCode 2.0.24 的
`reasoning.state` 是开放记录，不能仅凭它识别密文。此类型尚未接入持久化；当前尚无 live provider adapter 产生 artifact。
OpenCode 密文采集仍待注册经验证的 provider 字段。

本类型校验器仅接受最小合成 envelope；真实会话记录及额外 metadata 须在对应 provider adapter 卡审查后接入，当前不声称真实 JSONL 已支持。

## Readable reasoning projection (RG-04)

RG-04 加入可读 reasoning 投影路径：`provider_visible` 的 reasoning 文本是
唯一能进入 coverage 投影的 reasoning 内容，且必先经过 typed redaction ——
reasoning 文本中的 canary 秘密会计入 redaction report 并在投影、checkpoint
持久化或任何 metadata/digest 写入前移除。可读 reasoning 以独立的
`reasoning` 记录类型投影，携带 `provider` 与 `source_kind` 元数据；
它从不冒充 assistant 回答，也绝不写入未脱敏的 metadata 或日志。

`ProviderVisibleText` 的 serde 序列化/反序列化明确拒绝；调用者须经分类与 typed redaction 后使用 `canonical_turn_bytes` 或 `safe_turn_projection`。RG-04 不接入真实 provider adapter。


RG-02 将已核验的密文归档为 opaque artifact：每个 checkpoint manifest 可携带
`reasoning_artifacts[]` 数组（path/oid/sha256/byte_len/locator/provider/source_kind/
availability/decrypt_capability），以 `reasoning/encrypted/<sha256>` 存储逐字节精确对象；
相同密文字节去重为单对象、重复 locator 被拒绝、扇出有界且 fail-closed（512 条 /
256 KiB manifest / 32 MiB 总量）。`content_hash` 仍只覆盖四个普通角色；空 artifact
集保持 checkpoint 树逐字节不变。

新建 opaque artifact 对象使用标准 zlib stored blocks（level 0）以限制压缩 CPU；
canonical blob 字节和 Git OID 不变。高熵密文因此占用更多存储及镜像带宽；
普通对象仍用默认压缩，已存在的有效对象直接复用，不重新压缩。

### 受控 opaque reasoning 导出

Libra 保存 provider 已加密 bytes，不拥有也不猜测解密密钥。可读 reasoning 先经过 typed redaction；signature 认证 provider 输出，redactedData 表示 provider 已遮蔽的 reasoning 块，reasoningEncryptedContent 是 opaque provider 密文。这四类边界不混用；字段名称本身不能证明 native 来源，原有来源证明门禁继续保留。

OpenCode export 的 assistant reasoning 现在先经过普通 transcript Redactor 前的分类。
可见 reasoning 文本保留在普通 transcript 中并接受脱敏；含 reasoning 的 turn 在
coverage v1 下仍为 Incomplete，不新增 digest record。普通副本删除
`messages[].providerContext.messages`，assistant reasoning 仅保留可见的
`id`、`type` 和 `text`。
所有 role 的 part、message 封套，以及 export document／session info 中的
provider metadata 也在脱敏前删除，
但已完成工具的 `state.metadata.files[].file` 字符串保留最小修改文件投影；
其它工具 metadata 字段删除。producer 写入的布尔型 `state.metadata.truncated`
被识别后删除，不改变完整度；普通成功工具的 `false` 也适用。已完成工具中的
错型 `truncated`、其它 metadata 同级字段（包括输出被截断时写入的
`outputPath`）、`files[]` 未知子字段或错型值均删除并使捕获保持 partial；
`files[].diff` 删除但不改变完整度。运行中／失败工具的 metadata 删除但不改变
完整度，除非包含具名 opaque 提示。此类 partial export 无法写入 live checkpoint
（`SourceRejected`），也无法进行 verified import
（`ImportError::SourceAuthorization`）；partial 并非仅是提示。
native 已完成／失败工具的 `state.content` 经嵌套 metadata 清理后保留安全可见文本；
含具名 opaque 提示时转为 partial。classic 工具没有 `state.content` 字段，
遇到该字段即省略并标 partial。
已知的 classic 非 reasoning part 标识、step 字段、时间以及安全的工具
title／time／文件附件保留。native 的 model variant、compaction model 和
安全的 error／retry 摘要也保留；schema 形状的 native／classic fixture
必须维持 ordinary、coverage 与 extractor 投影等价。provider 响应正文／
头、classic format／summary／structured 载荷及 session fork、permission、
revert／share metadata 即使属于上游有效字段，也按设计省略并标 partial。
其中包括 classic user 的 `info.summary` 对象及 assistant 的 summary 标志；
非空 native user `files`、native `skills[].text` 和 native location 中额外的
workspace 标识也省略并标 partial。
保留的 message／part 封套采用封闭字段及类型投影；未知子字段或错型值标记
partial；嵌套 provider metadata 或具名
opaque 提示不能借用户附件、classic `info.model`、assistant `model`、错误等
对象进入普通副本。不安全的非 assistant part 整体省略，独立的安全 prompt part
仍保留。native／classic 工具的 `state.input` 是自由形状的普通用户／工具内容；
classic `state.output` 只有
String 类型才作为普通内容保留，object／array 会删除并标记 partial。自由内容中
同名 JSON 键本身不证明 native 来源。native
`state.output`、`state.raw`、`state.title`、`state.attachments` 和
`state.time` 不属于 2.0.26 native schema，遇到时剥离并标记 partial。
只要出现 `state.signature`、`state.redactedData`、
`state.reasoningEncryptedContent` 候选、未知 state 形状或错误 proof，捕获保持
partial 且不生成 artifact，直到 native 来源通过独立验证。这类 reasoning 的 `text`
也从普通副本移除，因为它可能重复或包含 opaque 值片段。错误或不支持的 export
封套只产生不含正文的 partial snapshot，原因使用现有通用
`SourceAuthorizationMismatch`。reasoning 上的同名
SDK metadata 也保持 partial，并从普通副本删除。安装版 OpenCode／模型验收只在本机
macOS 手工执行，不进入 CI。
采集前置分类只接受带 `messages` 数组的单个 JSON export 对象。flat 单条消息、
JSONL、缺失或错型的 `messages`、重复键及损坏 JSON 均得到无正文 partial，
原因是 `SourceAuthorizationMismatch`，不能转成普通 checkpoint。独立的
OpenCode metadata extractor 仍有 best-effort flat／JSONL 兼容，但不能绕过
采集门。可解析 export 中未知 root、message 或 classic-info 字段被剥离并标
partial；独立的安全可见文本仍保留。固定诊断不回显被拒正文；重试需提供
受支持的 export。

`checkpoint show` 保留固定的人工可读安全摘要。`checkpoint show --json` 仅在存在已声明 artifact 时增加 `reasoning_artifacts[]`，单条仅包含 `provider`、`source_kind`、`locator`、`byte_len`、`sha256`、`availability`、`decrypt_capability`；不返回路径、对象 id、任意 metadata 或正文。默认 show、普通 transcript export、scoped input 和 skill 读端不读取 artifact body。无效 checkpoint 布局或 manifest 拒绝且不回显其内容。 catalog 根 id 合法但对象尚未到位时保留旧安全摘要、省略可选 artifact 数组并发出固定无正文诊断；已存在但损坏的根仍拒绝。此兼容分支不授权导出。

```sh
libra agent checkpoint export <checkpoint-id> \
  --reasoning-artifact <lowercase-sha256> \
  --allow-raw --justification "operator recovery" -o <new-private-path>
```

导出要求 `--allow-raw`、非空白 `--justification`、`-o` 新路径及小写 SHA-256，不与 `--raw` 混用。独立 action `reasoning_artifact_export` 的授权/拒绝审计必须先于 checkpoint 查询、正文读取和文件创建；grant 记录已授权的尝试，后续失败不会撤销审计，审计写入失败则立即停止。

只允许导出选中 checkpoint 声明的 artifact；完整核验 manifest/tree 闭包、blob 类型/对象身份、SHA-256 与可选 byte_len。上限 32 MiB，截断前缀不导出。bytes 不经过脱敏、解码、stdout、模型上下文或解密。macOS/Unix 以目标旁的 0600 私有临时文件写完后 no-clobber 发布，已有文件/链接/目录不覆盖，不自动建父目录，失败清理临时文件。没有此 owner-only 权限原语的平台以 `LBR-AGENT-013` 和拒绝审计关闭导出；Windows 私有 ACL 导出延后，默认元数据和普通 transcript 读端仍可用。

`ReasoningDecryptor` 对 Codex、Claude Code、OpenCode 均返回 `None`，`decrypt_capability=none`，密文保持 `encrypted_unavailable`。将来的 provider 官方显式授权 capability 必须核验 checkpoint/provider 范围；本卡不实现 decryptor，也不接触解密密钥。

artifact 的树可达性由 RG-06 接入；受控读取及导出由 RG-03 接入。


Checkpoint 读取器在消费 manifest 或 metadata 正文前验证对象类型、内容寻址身份和普通角色绑定。Skill search 跳过不属于 catalog 中具名 checkpoint 的 metadata。首次 `--checkpoint` 解析以只读模式访问显式仓库 catalog；缺失或不一致时拒绝输入，不创建 run。保存输入的 resume 暂沿既有路径，由单独的恢复任务卡接入复验。默认 checkpoint show/list 摘要与 raw export 授权、审计保持原契约。每个 skill 候选独立复验：普通子树累计上限为 16 MiB，选中的 metadata blob 上限为 16 MiB；先检查具名 checkpoint 的 metadata 绑定，再遍历普通闭包。

对于 legacy OpenCode checkpoint，默认 `checkpoint export` 和已绑定的 metadata
读取器剥离 reasoning part 及预留 export/checkpoint metadata 中的
`metadata.anthropic.signature`、`metadata.openai.reasoningEncryptedContent` 和旧键
`encrypted_content`；保留可读 reasoning、其它 metadata、工具输入和用户 JSON。
provider 分类以该 checkpoint 的 session catalog 为准，metadata 中存在的 kind 必须一致。
缺少 session 绑定、metadata 不一致、重复 JSON 键、非法或不支持的 OpenCode JSON，
以及截断的非空导出会以不含 payload 的错误拒绝；空 transcript 仍合法。
`--raw --allow-raw` 保留原始有界字节和既有审计，`checkpoint show` 仍只输出安全摘要。
不改写已存对象，后续修复必须保留普通输出的剥离保护；本 legacy reader 不证明
native 来源，也不实现新 `state.*` reasoning artifact 通道。

macOS本地内容门是`agent_local_capture_smoke_test`中的
`local_capture_smoke_opencode_macos`。必须显式打开local-agent开关，固定
私有 OpenCode `2.0.26-libra-native.1`（`LIBRA_OPENCODE_NATIVE_BINARY`）并选择`volcengine-agent-plan/glm-5.3-flash`，先核对临时项目，
再请求一次固定文件写入；断言prompt/model/tokens/files与源投影一致、文件元数据持久化、
会话停止、doctor及卸载后数据保留。预检失败或超时记为失败，付费模型尚未调用。
当前验收机的OpenCode私有server未报告readiness，真实内容验收延后为计划外遗留
`DEFER-OG-OPENCODE-BOOT-01`；harness与合成测试不证明真实内容或native reasoning来源。


### 私有 native-origin issuer 登记（macOS）

安装挑战会在 owned 私有安装目录内，为每次启动建立独立的 home、config、data、cache、state 和 tmp 目录。初始环境逐项构造，不继承调用者的 `TMPDIR`、运行时加载覆盖变量或 HOME/XDG 配置。producer 启动前，tmp 必须为 canonical、当前用户所有、0700 且为空；前置检查或显式清理失败均通过既有固定挑战错误拒绝登记。

Scratch 在子进程生命周期内保留，并在正常退出、失败或超时后清理；取消时尽力清理。继承同一进程组的后代会在 leader PID 仍保留时终止。父进程崩溃或 SIGKILL 可能留下仅 owner 可访问的 scratch，后续挑战绝不重用。此隔离仅覆盖 Libra 发起的安装挑战；独立启动的 OpenCode 会话仍须满足各自运行条件，当前 `.3` SOURCE、Source A 与真实字段归档门保持未放行。

普通 `agent rpc trust opencode` 只固定 exporter binary，不登记 issuer，也不证明 reasoning 的 native 来源。历史 native 路径要求独立安装、已审查的 macOS arm64 `2.0.26-libra-native.1` 或 `.2` producer，以及为当前 canonical Libra 可执行文件路径渲染的精确冻结 v1 observer recipe。按已审私有交付包的说明，在隔离、owned 的环境准备该观察器；全局 OpenCode 保持独立。

不可变目录另钉住独立的 `2.0.26-libra-native.2` 候选（build `78c1f19e9249da8d0143a079f90aec9a`，macOS arm64 binary SHA-256 `28174e893a2b63b75f58af4c77212747f6e1eebce5dd0252cfe64847b342615c`）。新增 endpoint descriptor v2 只允许第一方 `anthropic-messages` 路由使用精确 URL `https://api.anthropic.com/v1/messages?beta=true`；receipt 与 Ed25519 安装挑战仍为 v1。Anthropic-compatible、代理与改写 URL 均不匹配。选择 `.2` 仍需实测 binary、新私有安装与 key、显式 exporter trust、按 epoch 显式登记 issuer；`.1` 旧 issuer 可保留为 verify-only，不自动提升旧 key。`.1` 仍是冻结的历史 NPR-18 harness 基线，不是当前普通 OpenCode 基线。目录匹配或合成测试不证明真实 `signature`/`redactedData` 的来源与归档资格；`reasoningEncryptedContent` 仍延期。依赖账号/模型的检查只在本机手工执行，不进入 CI。

历史 v1 观察器与当前普通 v2 观察器不同，**没有按目录过滤**，禁止用于共享多项目 OpenCode server。优先使用当前普通采集；任何 legacy 使用都必须隔离为每个 server 仅一个仓库。`agent doctor`/观察器状态因当前检测要求 v2 header，会把旧 v1 报为未安装。`libra agent enable --agent opencode` 会把 managed v1 替换为 v2，导致旧私有 recipe 不匹配。因此，通用重装或 `agent enable` 不是 legacy issuer 登记的修复方式。本兼容修复不改变普通插件的目录过滤，也不新增 legacy 安装器。

下面登记示例以精确的已审历史观察器已就位为前提，刻意不执行 `agent enable`。正式安装版兼容门使用新的 owned 仓库、HOME、XDG 目录与 epoch-0 registry，保留原 `.2` 公开 descriptor 和 issuer/key 不变；仅由实测 producer 的登记挑战访问原 key。不运行 Session、账号/模型请求或 OpenCode server，清理时撤销新 registry。成功仅证明登记兼容，不证明 Source A、当前 `.3` producer 或真实 native 字段归档。

```sh
libra agent rpc trust --dir /absolute/approved-private/bin
libra agent rpc trust opencode
libra agent rpc trust opencode \
  --native-origin-manifest /absolute/private-installation/installation.json \
  --native-origin-expected-epoch 0
```

两个 native 参数必须同时提供，并且只能使用字面 slug `opencode`。首次 epoch 为 0，后续显式登记使用当前仓库 registry 的 epoch；重试前在本机用 `libra config --get agent.native_origin.opencode.v1` 查看。冲突不会改写 exporter trust 或 registry；registry 含本机安装路径，保留在本机。

公开 descriptor 必须是当前用户拥有的 regular、非 symlink `installation.json`，权限 0600，父目录 canonical、owned、0700，大小不超过 64 KiB，符合私有 producer 的封闭最小 JSON 格式。Libra 不读取私钥。原 exporter trust、实测 approved producer、当前 canonical Libra 路径及实际安装的 `.opencode/plugin/libra-hooks.js` 必须匹配；manifest/export 自报公钥、路径、endpoint 或 build 不建立许可。

实际 producer 必须在 30 秒内成功回答一次新随机、独立 domain 的 Ed25519 challenge，stdout 上限 4096 bytes，stderr 有界捕获且失败时不回显。随后在同一写事务中重查当前 trust 和 trusted dirs、执行 expected-epoch CAS，只提交一个完整 registry 值。重复物理行、大小写 alias、encrypted/malformed/超限记录均拒绝，不自动修复。上限 64 KiB / 128 issuer；不删除吊销历史腾位置。

新的独立安装 key 为 active，旧 active 转 verify-only；已有 issuer 不由登记或普通 re-trust 重新激活。登记成功仅证明安装 key/binary 关联，不代表 native 字段或归档验收通过；仍须验证 receipt 签名、当前许可、来源坐标和原字段，未知来源保持 partial。JSON 输出中的当前 Libra executable SHA256 仅用于本机 transient 审计，不进入 signed policy，也不固定为历史 Libra 构建。

若人工针对未发布开发版 `cd53…` observer policy 建立了未许可记录，仍会拒绝。请完整保留并备份 registry 和仓库，暂停该仓库受影响的 native 登记/归档，等待单独审查的修复；不得删行、自动迁移或把记录视为已许可。有界兼容检查不代表扫描了所有用户仓库；可在新的隔离工作区使用受支持的普通采集，但不能声称原仓库已修复。



### OpenCode native-origin 轮换与撤销

轮换时准备新的独立私有安装 key，以当前 epoch 显式登记；原 active issuer 转 verify-only。verify-only 和 revoked 均不能为新的 native capture/import/pending replay 授权。共享许可读取必须使用同一事务快照、独立新测 producer provenance、匹配的当前 exporter trust 与 trusted directory；返回的仍是安装数据，不是已验证 receipt 或 native artifact。

`libra agent rpc untrust opencode` 在同一个有界写事务中移除 exporter trust，并永久吊销所有尚为 active/verify-only 的 issuer。保留全部墓碑和既存 artifact，external RPC 关闭时也可执行。registry 损坏/重复、容量或 epoch 失败、写入故障均拒绝，不留下部分 trust/registry 修改；先保留并核对本机 registry 再重试，不自动修复或删历史。重复成功 untrust 保持幂等，已经全吊销的 registry 不再递增 epoch。

JSON 保留 `slug` 和 `removed`（后者只表示是否移除了 exporter trust），OpenCode 额外返回本次新吊销数量 `native_revoked` 和结果 `native_epoch`（不存在 registry 时为 null）；其它 slug 沿用原输出与行为。

若 registry 损坏导致 combined untrust 拒绝，可用 `libra config unset agent.trust.opencode` 先暂停 exporter 访问。此命令只移除 binary trust，保留 registry 及全部墓碑；显式重新 trust 同一已审 binary 后，仍为 active 的 issuer 可恢复许可。因此须保留全部记录修复 registry，再成功执行 `libra agent rpc untrust opencode`，才完成永久 native 吊销。

自动 provenance 重验发现 binary 不可读/漂移或 trusted directory 被撤回时，仍只移除 binary trust，暂停 native admission，registry 保持原字节。显式重新 trust 同一已审 build 可恢复从未被 native 吊销的 active issuer；verify-only/revoked 绝不复活。重新登记已有 issuer 仍拒绝，必须准备新独立 key。轮换/撤销不回写既存 receipt、安装 descriptor 或 immutable artifact；成功不代表 native 字段已证明，未知来源继续 partial。


### 冻结的历史私有 OpenCode 验收基线（NPR-18）

NPR-18 历史 macOS arm64 harness 唯一允许的验收构建是 `2.0.26-libra-native.1`，build
`bac21cc4027896b8d1836133c347d20f`，SHA-256
`547c36eb928da163ef61d019bad669860d519f2da0b67737c9a3920e831c9196`，
大小 187635570 字节。官方基底是 OpenCode `2.0.26` commit
`9b4ec5714d481559990db0a816d5dec19541a814`；这个 commit 只标识官方基底，
定制构建另由 source、patch、lock、build-input 摘要标识。
公开的 `native-origin-build.json` 仅包含已审构建身份，不包含安装策略、issuer 或私钥。

macOS 本地采集验收前，将 `LIBRA_OPENCODE_NATIVE_BINARY` 设为独立安装的私有
`opencode` 的绝对路径。harness 在登录或模型调用前校验已审版本、摘要和大小，
version 与 login 预检均限制 30 秒；所有 capture/export 子进程的 PATH 首项
使用同一个私有 binary 目录。缺少显式 opt-in 或发生漂移就失败，不能回退到全局
OpenCode；官方全局安装和既有 observer ABI 保持独立。

NPR-12 的 SOURCE_READY_GO 仅涵盖此 darwin-arm64 构建与当前 OpenAI Responses
transport 机制。原官方 source record、event/export fixture 和合成 receipt/signature
向量保留历史身份及原字节。Source A 仍为 UNPROVEN；确定性测试、版本、登记或
descriptor 不能证明真实 native provider 字段，也不能放行 reasoning 归档。
readiness 或配额失败保持 partial；唯一授权付费 smoke 模型是
`volcengine-agent-plan/glm-5.3-flash`，不使用其它模型回退。本卡没有新增模型调用，
Linux 本地功能验收留给使用者。


已证明为本类型终态的 state 在 manifest 缺失时仍可清理。review 基础设施失败可能没有 reviewer 结果行，而 manifest 仍保留请求的 reviewer；该受支持形状可以清理。无法解析的 manifest 即使伴随可证明的终态 state 也仍拒绝：保留 run，修复其元数据后再重试。


### Provider 已加密 reasoning 的生命周期

`libra cloud sync` 在开始 generation 前校验 reasoning artifact 的 manifest role、规范树、`object_index`、解码后的 `byte_len` 与完整 SHA-256。首次缺失先正常上传/修复，上传后的读回仍失败则不能完成 generation；校验过的 checkpoint 树与 artifact 索引还必须匹配最终 fenced generation。校验不输出密文内容。

会话擦除使 checkpoint list/show 和 session graph 不再暴露 artifact ref；`agent clean --gc` 与仓库 GC 按既有保留窗口回收不可达的本地 artifact。`agent doctor` 以 `inconsistent_reasoning_artifact` 和精确的规范 `checkpoint_id` 报告不一致，要求人工修复；这是只读 finding，不新增持久化 status 列。R2 物理 payload 删除仍延期；云 tombstone 删除镜像目录行并阻止正常 restore 复活。

非 quiet 的 `agent enable` 和 `agent import --yes` 披露 provider 已加密 reasoning 原字节可能在不解密的情况下归档，并随 checkpoint 云镜像。合成生命周期 fixture 在可见时始终为 `encrypted_unavailable`，擦除后不再有引用；它们不证明 native 来源或 decryptor。native 来源证明要求继续保留，未获证明的 OpenCode 字段不归档。


### 授权 reasoning 源载体（OG-16）

已选 registered adapter 的脱敏前预处理 capability 默认为 None。它只分流已授权、受限的内存源，不重开 provider 文件，不访问 checkpoint store。未启用或没有候选时，snapshot、live 和 store 的普通字节行为保持兼容。启用后，候选载荷在普通 Redactor 之前剥离；未证明来源或未映射候选保留不含载荷的 partial，即使零 artifact 也不能转换为普通 complete。

sealed native carrier 将原始 opaque 字节和来源绑定证明一起移入共享 checkpoint payload。在 object/index 写入前验证 issuer、provider、source session、message、part、field、wire SHA-256 与已授权 capture/turn 关联。metadata-only identity adapter 不暴露载荷、不签发 native 权限；Debug 不含 native 坐标或载荷。source、artifact 字节与条目数量上限保持。

认证的私有导入传输将已证明 native 来源的 typed 集合从授权 descriptor helper 交给父进程。单请求密钥把 helper 响应绑定到请求、授权源、仓库 scope 和集合；父进程验证绑定并保留完整 native proof，再将 owned item 分配给已 reserve 的 turn 与共享 checkpoint store。未认证或未证明来源的非空集合固定拒绝，不把 wire DTO 当作 native 权限，也不让候选字节回到普通 transcript。空集合 preparation 的旧响应和源投影字节保持不变。pending v1 仍在 envelope 写入前拒绝非 ordinary payload；pending v2 仅接受已验证借用的 native 载体及其预备 identity plan，并沿用既有 receipt、fence 和 lease 规则。已启用 Claude deadline-FD 路线时，在解析 source 前拒绝，不回退普通捕获。已有 Pending receipt 不尝试 wrapper failure 诊断；NotTracked 仅沿用既有有界诊断尝试，不推进 sync_revision。

生产 provider capability 仍未启用，生产 native issuer 仍未准入。合成正例只证明载体和导入传输机制；Source A 仍为 UNPROVEN，真实 native 字段接入已延期，未获证明的 OpenCode 字段不归档。durable reasoning replay 仍需通过后续门。本机制不增加解密能力或 public metadata enum 值。

### OpenCode native 准入前的 sealed candidate（OG-26）

对已授权的 OpenCode export，采集在构建 JSON 解析树前检查结构预算，再于普通脱敏前私有分离封闭的 assistant reasoning `state` 与原始 `originProof` 字节。候选保留解码后的 UTF-8 原值及 source session、message、part、field 坐标；限额为 16 MiB 来源、65,536 个 JSON 节点、64 层嵌套、每条 receipt 8 KiB，以及既有 512 条 artifact 数量上限。普通副本不含这些私有值；未验证候选仍是 partial snapshot，不能交给普通 checkpoint consumer。损坏或超预算输入成为无正文 partial；结构预算超限报告 `source_oversize`，不误报为来源授权不匹配。长会话即使小于 16 MiB，也可能超过 65,536 节点限额；请开启较小的新采集会话，并在重试前查看不含正文的 partial 原因。在来源拒绝与 sealed candidate 路径，自有原始来源、候选与解析树分配会清零，但 Serde 在成功及失败解析过程中的内部 scratch 与部分构造值无法逐项清零。此候选不签发 native 证明、不写 reasoning artifact；issuer 验证、真实字段准入与 replay 仍各有独立门。调用 OpenCode CLI／hook／export 并进入 Session 采集管道的集成验收（包括确定性 fake-exporter）在 CI 中显式 ignored，仅在 macOS 本机执行；纯解析器和沙箱单元测试可留在 CI。依赖已安装 OpenCode 和模型实际响应的检查亦只在 macOS 本机验收。


### Pending reasoning v2 codec（OG-17）

私有 pending codec 已把封闭 inner payload v2 与 live SessionEnd writer 和认证 reader 接通。仅在已准入 producer 提供绑定的 native-proven 集合时，writer 才封存原来源记录和各 turn 显式的 `verified_nonempty`／`verified_empty` identity。普通 pending 仍逐字节编码为 v1；外层 header/envelope 保持 v1、原 MAC domain、64 MiB 总包、128 个 chunk 与每 chunk 512 KiB 上限。旧 reader 拒绝 v2 并保留证据。新版 reader 先验证原完整 envelope MAC、canonical 成员／claim 关联和当前 catalog、claim、marker 围栏，随后 lower reasoning issuer 才复核固定的 native 来源证明。恢复仅将本次 reserved turn 的 artifact 与原稳定 identity 同 checkpoint、claim、ref 事务提交。append 失败保留 pending 和 claims；每次重试重新认证原字节及当前围栏，不重开 provider 来源，也不补造私钥。缺失或损坏的私钥／来源证据需要 doctor 诊断与人工恢复。DTO 或 envelope MAC 本身不证明 provider native 来源；未证字段继续 partial、不归档。生产 native issuer 与真实 OpenCode Source A 证据仍未放行；合成测试只验证该传输机制，不代表真实 native 归档。

### 私有 reasoning identity 与垃圾回收

垃圾回收将保留的 `agent_reasoning_identity` scope 整体排除在通用 metadata 对象引用扫描之外。即使整个存储值损坏为 JSON 对象 ID，或包含嵌套对象 ID，也不会由这一路径触发对象加载。该隔离不需要私有 reasoning 密钥。其它 metadata 对象根、私有 capture 恢复 scope 的排除和 capture pending 父提交根继续遵守原有规则。前滚恢复中，停止或修复 reasoning identity writer 时必须同时保留私有记录与该隔离。这个护栏不创建 identity 记录、不签发 native 来源证明，也不启用 OpenCode reasoning 归档。

### Reasoning identity 覆盖与会话上限

reasoning 覆盖无法安全比较时，live capture 和 export 会 fail closed，并返回 `reasoning coverage is partial; run libra agent doctor before retrying capture`。doctor 会用 `reasoning_identity_changed` 报告完整 turn 的 identity 冲突；该情况需要人工检查已授权的 reasoning artifact，doctor 不会替用户选择保留哪一份。

同一 session 的私有 identity 命名空间有独立的 `identity-v1` 与 `identity-v2` 键；两者合计最多 4096 条记录、1 MiB 已编码存储值。达到任一上限时会停止写入，并返回 `private reasoning identity session map reached its safe capacity; start a new capture session`。map 缺失属于 legacy 状态：没有 candidate 的普通 turn 保持原 coverage 行为，首个符合准入条件且带 identity 的 turn 可以创建 map。格式错误、超限、含未预期内容或无法通过认证的 map 会使整个 reservation 在任何 claim 写入前以 `private reasoning identity is invalid; start a new capture session` 失败，包括不含 candidate 的 turn；import receipt 会记录为 failed。doctor 不会修复无效 identity map；没有 conflict finding 也不代表 session 已 complete。不要手动编辑私有 metadata scope。

这些检查只比较 capture identity，不证明来源来自 provider 的 native 字段。未获证明的 OpenCode reasoning 保持 partial，不会归档。


### OpenCode 原生导入的目录绑定

OpenCode 原生 export 通过 `info.location.directory` 绑定会话目录。导入同时
兼容旧格式 `info.directory` 和 `info.cwd`，收集所有已声明的字符串目录，
要求它们 canonicalize 后只有一个目录且处于当前 Libra storage 边界。字段
顺序不会掩盖冲突；目录事实采集不强制转换非字符串值，原有来源与 schema
校验继续适用。等价词法路径或 symlink 别名仍经过原 canonical scope 检查。
目录缺失或冲突返回 `LBR-AGENT-016`，另一仓库的目录继续拒绝。
本兼容修复不授予 reasoning 的 native 来源权限。


### OpenCode 2.0.29 idle 错误投影

公开2.0.29 idle消息schema新增可选 `error`；缺省error保持ordinary投影。
诊断header必须含string `type` 和 `message`，可选HTTP `status`只能是
100到599的整数。必需字段缺失、错型或非法status删除整个error并标partial；
`response`整段和未知成员剥离且partial，opaque提示删除整个error。
已有compaction/retry的error处理不变。

这些是普通typed诊断，不是native producer或字段来源证明。新增回归fixture
为synthetic；真实模型Session的idle.error失败输出仍未证明，成功普通会话
不授此失败路径PASS。真实OpenCode/账号/模型验收只在macOS本机，不进CI。
reasoning Source A仍是独立门。


### OpenCode 普通 partial live checkpoint

自动 live 采集可以保存已授权 OpenCode export 经安全擦除后的普通副本。
checkpoint 始终标为 `Unproven` / `Partial`；保存内容不代表 reasoning 完整
覆盖或 provider native 来源。至少要保留一个含实际可用内容的已知非 reasoning
text/message part；只有 metadata 或已擦除 reasoning 骨架的源仍拒绝。
该路径只支持当前 native `messages` export；旧 classic `info` / `parts` export 仍排除。

该 storage-only 路径要求严格单 JSON、exporter 授权、精确会话映射、有界脱敏及
repository-scoped source HMAC 绑定全部成功。源 `info.id` 必须等于所选 provider
session。owning carrier 在绑定时记录 `repo_id`、Libra session、provider session
及 agent kind，并与实际 checkpoint request 比较。HMAC 是 opaque string，格式
正确本身不验证 scope，也不证明 native 来源。generic ordinary consumer 继续拒绝
`Unproven`。

unsupported reasoning state（包括 `reasoningField`）与可读的关联 reasoning text/time
一并写入普通 transcript，包含 text、time 与 opaque state。opaque hints 包括 `signature`、`redactedData`、
`reasoningEncryptedContent`、`thoughtSignature`、`originProof`。原始解析树任意
位置出现 `originProof` key（包括错型或未知嵌套值）或 sealed candidate 都不能走
该 fallback。只有完成擦除和 Redactor 的结果可入库，metadata flag 不能授此权限。

在这个已验证的 storage-only 路径，`source_authorization_mismatch` 也表示删除了
reasoning 或 unrecognized data；exporter 授权已经成功。其它路径保留原拒绝含义。
该 marker 仍为 `Partial`，Source A 仍为 UNPROVEN。不创建 reasoning artifact、
私有 reasoning identity、Complete coverage claim 或 native ACK。

每份 snapshot 的 source 输入单独受上限约束（最多 16 MiB）。不同 idle action
可以在 content-addressed storage 内保存重复 whole-source snapshot。每个 Session
没有累计总量上限，保留内容可能随 idle 次数 × source 大小增长，另有脱敏及存储开销；
本路径不新增跨 idle deduplication 协议。已有 action、
generation、lease、scoped tombstone 和 ref-CAS 围栏保持。只有实际确认 durable
partial checkpoint 写入后才产生普通 lifecycle 成功收据；直接写入失败保持 dirty
或 content-free pending intent，不产生成功 ACK、不提前推进 generation。
当前不支持 durable partial payload replay。

此 unsupported-state 源的显式 import 仍以 `LBR-AGENT-020` 拒绝，live 修复不启用
import 或 native admission。普通 payload 仍为单个已脱敏 JSON document，无新增
public reason/metadata enum value；本构建的 checkpoint/export 读端读取普通 payload。
旧二进制兼容必须另外实测读回，不能仅由 source 形状推断。
V2 import 对 partial projection 的 zero bytes/zero redaction metrics 规则不放宽。

正式 0.30.49 已包含普通 live 修复：它可以按上述封闭规则保存安全的普通副本。
正式发布不证明 native 来源，也不完成后续 COV 家族门。含 unsupported reasoning state 的历史 OpenCode import
仍以 `LBR-AGENT-020` 拒绝，修复已延期。
OpenCode Session/账号/模型检查只在 macOS 本机运行，不进 CI；synthetic 不证明
native 来源。


### 当前官方 OpenCode 普通来源基线

当前官方ordinary来源参考为OpenCode **2.0.29** /
`042d0832f5f3a73eaa039a58a7d565776f6d4290`。本机验收显式选择已测量的
官方binary和同版本server/exporter。冻结2.0.26 source record、全部既有JSON
fixture及私有 `.1` / `.2` 保留原身份；旧 `.1` helper只接收原精确version、
binary和build ID。

官方版本/来源匹配不批准 native producer，不登记issuer或证明opaque字段。
普通exporter trust不是native来源授权；Source A仍独立，native.3登记不阻塞
普通基线。151个机械producer案例不是whole-producer source/full验收。

2.0.29 idle header 要求 string type/message，以及可选的 100 到 599 整数
HTTP status；错型或敏感 error 保持 partial。adapter 的离线 synthetic 测试
不证明真实 idle.error 失败输出。

本普通基线验收自然 automatic capture、durable checkpoint 及该 checkpoint 的
普通脱敏 export，并将 prompt、model、token 投影与已授权源比较。删除
unsupported reasoning state 后，来源/已存 snapshot 的状态对为 `Unproven` / `Partial`：
`Unproven` 仅指内部 source 分类，`Partial` 才是持久化 completeness 值。
可读 reasoning text 保留。关联 reasoning time 与未证明字段一并保留。model 比较使用 native assistant record 的
`extraction.model`；顶层 `model` 来自 hook metadata，可以不同。token 只累加
不同 native assistant record，不重复累加 session summary。无工具调用的样本
只证明 empty-files consistency（空文件一致性），不证明 write/edit/apply_patch 正例。

含 unsupported reasoning state 的历史 OpenCode import
仍以 `LBR-AGENT-020` 拒绝，修复已延期。本 automatic capture 基线不启用历史
import、pending replay 或 native admission。官方版本/来源匹配不批准 native producer，
普通 exporter trust 不是 native producer 批准或 Source A。Source A 仍为 UNPROVEN。

只有核对最终源码、配置，并用实际新 binary 验证读回后，才能复用此前的自动采集
证据；未核实项保持待验。成功普通样本不证明 native 字段、完整 reasoning、真实
idle.error 失败、自动终态或 full-suite 验收。真实 OpenCode Session/账号/模型检查
只在 macOS 本机运行，不进 CI；metadata-only、空输出或 skip 不算内容验收。

<!-- Compatibility anchor for earlier links; not a release-status assertion. -->
<a id="coverage-v2-采集未发布"></a>

## coverage-v2 采集（Libra 0.30.50）

从首个包含 coverage-v2 的稳定版本开始，OpenCode 导出采集与历史导入会保留受支持的
provider-visible 可读 reasoning，并在脱敏后作为 typed turn 内容写入。含这类内容的
turn 使用 coverage schema 2，普通 turn 仍使用 schema 1 并保留原有规范字节；
计算 digest 和 checkpoint 投影前均先脱敏。这不证明 `signature`、`redactedData`
或 `reasoningEncryptedContent` 的 native 来源；未获证明的字段仍为 partial，不能
归档为可信 artifact。

保留的可读 reasoning 属于脱敏 checkpoint；执行导出、`libra agent push` 或云同步时，
它会随 checkpoint 一同处理。目前没有单独排除 reasoning、同时保留其他采集内容的参数。

升级后，如果某个中断动作的 checkpoint 已按 v1（或旧空 revision 集）提交，会先只读
结算原回执，不新增 claim 或改写 checkpoint。随后一次**新动作**可以按 v2 重采可读
reasoning 一次，之后重复采集会去重；原 v1 对象、digest 和 claim 保持可读且不改写。

如果来源分类被拒绝，而同一个 Session 已有 v2 claim，本次完整采集／导入都会停止，
包括新出现的普通 turn；回执和导出任务保持待重试，不创建降级 claim 或成功 ACK。
持续超限或不受支持的来源可能让该 Session 一直停采。请保留已记录数据，使用
`libra agent doctor` 检查，并新建 OpenCode Session 继续受支持的采集。候选或未验证
reasoning 不能将这种失败变为 complete coverage。这些改动不增加命令参数，要求 Libra 0.30.50 或后续版本；依赖 OpenCode Session／exporter／模型的验收只在 macOS
本机执行，不进入 CI 集成测试。

支持能力标记 4 的 Libra 构建打开仓库后会记录该标记。已发布的 0.30.66 及更早版本会拒绝该仓库中的所有命令，包括 `agent clean`；继续操作前请安装支持能力 4 的构建。已发布二进制从 Linux v0.30.67、macOS/Windows v0.30.68 起提供该能力；v0.30.83 覆盖全部平台。现有 Agent 工作目录不迁移。

Codex CX 新增测试（包括合成用例）只在本机手动运行，不加入 CI。入口为
`cargo test --features test-live-agent --test codex_capture_local_test -- --ignored --test-threads=1`
及 `cargo test --features test-live-agent --lib codex_summary_ -- --ignored --test-threads=1`。
默认 ignored 或未执行不是通过证据。

The revised legacy Codex boundary case is also operator-local:
`cargo test --features test-live-agent --test agent_checkpoint_redaction_test capture_foundation_codex_reasoning_boundaries_live_and_import -- --ignored --exact --test-threads=1`.

冻结 V1 投影删除空语义 turn 后，存活 turn 的 thinking 来源随原 turn 同步重编号。被删除 turn 的 thinking 仅在首个普通 checkpoint 保存一次，并标记 `turn_attribution_unproven=true`、`partial=true`；不声称这些正文属于该 checkpoint 的语义 turn。

sidecar 统计预算为 4096 个去重后的显式 turn 或 reasoning ID；重复回放不额外消耗 ID 预算。超出时标记统计不可用及 partial，普通来源副本仍保留 thinking 记录。

当前源码采用 Codex 0.162.1 的十二事件契约，包括 `Interrupt`。重新运行 `libra agent enable --agent codex` 只刷新 Libra 自有 handler 与信任条目；`Interrupt` 和 `SessionEnd` 使用 3 秒宿主 timeout、2000ms 采集预算，其余默认 30 秒、29000ms。`Interrupt` 沿用安装的 `stop` 命令但保留 `turn_interrupted` 语义：保留会话当前 phase，不生成正常完成的 turn checkpoint。`PostCompact` 共用 `compaction` 命令但保留 `compaction_completed`。既有事件身份 ordinal、用户信任及配置不变。事件支持不证明已经获取 rollout thinking。

这套十二事件安装的最低 Codex CLI 版本为 **0.162.1**；旧客户端先升级 Codex，再重跑 enable。既有十一事件配置在刷新前报告未完整安装，但已批准条目不被误报为 trust gap。此次同时修复共用 `compaction` 命令对 `PostCompact` 的旧拒绝行为；迟到的中断保留已停止会话状态，不重新激活会话。

Codex hook 设置只读取普通文件：`hooks.json`、`config.toml` 各限 16 MiB，完整性恢复 journal 限 64 KiB。安装、移除和状态读取拒绝 symlink/特殊文件 leaf，Unix FIFO 打开不阻塞读取器。坏型或超限文件原样保留；先移走特殊文件或减少文件大小，再重试。生成的配置超过上限时，在两文件任一发布前拒绝。解析错误只说明文件，不回显配置正文；保留 `CODEX_HOME` 所指定父目录别名，不承诺严格 whole-process 文件系统 deadline。


`libra agent doctor` 的 Codex 安装诊断逐项列出全部 12 个 managed 事件。固定 `reason` 为 `configured`、`missing`、`drifted`、`untrusted` 或 `disabled`，JSON 位于 `data.provider_hooks[].events`；仅当每项均匹配当前 handler、enabled 且 trusted 时，汇总 `installed=true`。带 Libra statusMessage 的修改或过期重复项使对应事件未配置；没有该标记的用户 handler 不属于此诊断。显式 `enabled=false` 会显示 disabled；哈希仍正确时，不新增信任缺口横幅。这些只读设置检查不输出 hook 命令、哈希或配置正文，也不证明宿主已执行 hook。使用 Codex 0.162.1 或更新版本；缺失或未受信时可用 `libra agent enable --agent codex` 刷新 Libra handler；刷新前先确认用户主动禁用的意图。漂移时先检查 `hooks.json` 以及 `config.toml` 中对应的 `[hooks.state]` 条目：enable 会刷新归属仍可证明的 handler，但保留仅有 statusMessage、命令却已不符合规范的条目；此类条目需经检查后手动修正或移除，重复 enable 不一定清除诊断。


## Codex 普通 thinking 与附带数据（CX-16/CX-06）

本开发源码中，`libra agent checkpoint show <id> --json` 在
`data.reasoning.readable` 返回经过凭据扫描的普通 thinking 正文，在
`data.reasoning.encrypted_content` 返回已保存的非空 opaque 值。
`availability` 为 `unproven`；遇截断、损坏或不支持的记录时为 `partial`。
`readable_obtained` 只说明此视图实际是否含可读正文，不证明 native 来源或解密。
普通 rollout 还保留 reasoning `response_item` 及
`compacted.payload.replacement_history` 中非空的 `encrypted_content`。
索引视图先核对 rollout 文件名、`session_meta` 会话及支持的 Codex 写入版本，
再把解码后的 JSON 字符串绑定到 turn 与记录序号。重复键和损坏记录不进入
索引视图，但不会删除已保存的普通来源字节。索引值仍为 `unproven`，不代表
解密成功或 native artifact。
metadata 必须属于此 checkpoint 的 ordinary tree；存在 `agent_kind` 时必须
与 session 一致，旧 metadata 可以缺少此字段。关联或 transcript 读取失败时
返回不含正文的 `unavailable` 状态，不退回原 blob。空 transcript 同样返回
`unavailable`、`reason=no_transcript`、`readable_obtained=false`；只有 metadata
不算取得 thinking。

默认 `agent checkpoint export <id>` 对旧 transcript 的 JSON 字符串解码后
扫描转义凭据，同时保留未知关联值、精确数字和重复键。既有读取上限仍是
`agent.max_transcript_read_bytes`（默认 256 MiB），不会改用新采集的 16 MiB
上限。`redacted=true` 说明 JSON 视图发生了凭据遮罩或不可解码字符串的局部替换。JSON show 最多查看 16 MiB、逐记录解析最多 1 MiB；超出视图的数据通过
默认 export 获取。截断视图明确为 partial，不能据此断言后面没有 thinking。
export 仍按配置上限读取源前缀。完整旧字符串含孤立 surrogate 或无效 UTF-8
时，仅用容错解码副本做凭据扫描；没有命中则保留 token 的原字节。仍无法
解码的字符串（包括转义或 UTF-8 中截断）会局部替换为 `?`，保留结构 partial。
即使遮罩后的 JSON 可以解析，show 仍保留源数据不完整的 partial 状态。完整取回更大 checkpoint 时需提高读取上限。
扫描器只解码 JSON 字符串，不为字符串之外或不支持的畸形结构中的任意转义
赋予第二次解释。

`libra agent doctor --json` 对当前 scope、ref 可达、已提交且保存了普通 Codex
thinking 但缺少内容绑定修订的 checkpoint 报告
`codex_legacy_reasoning_revision`。普通 `doctor` 只报告；
`doctor --repair` 追加以 checkpoint 与内容摘要为键的 metadata 修订。
旧 checkpoint 和 transcript 不被改写，重复修复幂等，已擦除会话不会复活。
每个扫描页最多检查 64 条候选，累计读取普通 transcript 不超过 128 MiB，单个 checkpoint 不超过 16 MiB；使用 `--repair` 时，符合条件的正文可能在另一组 128 MiB 页面预算内再读取一次。报告给出续扫游标时，下一次运行 `libra agent doctor --codex-legacy-after <checkpoint-id>`；每页需要修复时加 `--repair`。不适用、不可读及跳过的 checkpoint 不会因后续页面而自动获得认证。
这不会创建 native reasoning artifact，也不需要 schema 迁移。

人类 show、checkpoint/session 列表、graph、doctor 不展开正文；读取不改写
原对象、traces 或 coverage claim。raw 导出仍需 `--raw --allow-raw` 并审计
允许或拒绝，默认导出不写 raw-access 审计。合成读取测试只在本机显式运行：
`cargo test --features test-live-agent --test codex_capture_local_test codex_legacy_ -- --ignored --test-threads=1`。
它们不证明真实模型采集，不加入 CI。


## Codex 终态预算（CX-02 开发中）

管理的 Codex `SessionEnd` hook 的 host 窗口为 3 秒、主采集预算为 2000 ms。
ingress、数据库等待、生命周期预留、source 和写入共用原 monotonic/absolute
截止对。既有 finalizer 的 250 ms 无正文收尾窗口从原 deadline 派生，同一回调
内不重新计时。后续授权回调拥有各自的有界 I/O 窗口，恢复仍保留原 receipt 的
finalizer deadline 和重试计数。异步 exporter 即使不遵守传入截止，也会在主 deadline 被采集端取消；仅
取消 source 取得，不取消 runner 结算或已派发 SQL/ref COMMIT 的确认等待。
超时只记录不含正文的 reason=export_deadline_elapsed；取消在 future 让出执行时
生效，不抢占单次 poll 内的同步阻塞工作。
checkpoint 尚未持久化时保持 terminal receipt pending，不提前发布 stopped。

回调尚无可信 session/scope 帧就结束时，沿用 Codex advisory no-evidence 的
退出行为，不凭空建立 receipt 或完成 session。scope 已验证后无法保存必要
terminal 证据则明确失败。host 在 receipt 产生前杀掉进程，ledger 不能重建一个
未观察到的回调；请保留 rollout，有既有证据时使用 owner-expiry/doctor 路径，
或显式授权 `agent import --path <rollout.jsonl> --agent codex --yes` 取回实际
源数据。metadata-only 生命周期成功不算取得 thinking。终态完成以 receipt
complete 和持久 checkpoint 为准；保留的 finalizer 只记录 pending/quarantined
恢复证据，没有独立的 complete 状态。

本卡复用现有 finalizer/ledger，不激活 Codex live locator/exporter 或模型采集。
本机合成用例覆盖 CLI 数据库忙、2 秒 stdin 等待、host 中止、native 重放、
marker 后取消及 owner 到期恢复、用测试能力驱动的实际慢 source 管线；不证明真实 thinking。
先 `cargo build --features test-live-agent --bin libra`，再手动运行：

```bash
cargo test --features test-live-agent --test codex_capture_local_test codex_terminal_ -- --ignored --test-threads=1
cargo test --features test-live-agent --lib codex_terminal_ -- --ignored --test-threads=1
```

全部新 Codex 断言均受 test-live-agent 与 ignore 双门限制，仅本机执行，不加入
CI；原 CI 用例不改写。

## Codex 终态诊断（CX-17 开发中）

`libra agent session show <session-id> --json` 为 Codex 增加 `terminal_capture` 对象，人类可读输出显示相同状态和 owner。它读取原 catalog 收据与 writer marker；查看会话不会推进状态、消耗重试或修复 checkpoint。

- `status` 为最新终态收据的 `pending`、`completed` 或 `unavailable`。`completed` 仅指收据完成，不证明 thinking 可读、transcript 可用或对象完整；正文请检查 checkpoint show/export。
- `owner` 为 `live`、`expired`、`absent`、`cleanup_pending` 或 `unavailable`，复用原 marker generation 与绝对过期时间。即使会话 active 超过 24 小时，也不能覆盖有效 owner。
- `pending_receipts`、`completed_receipts` 统计有界 ledger 中的终态收据；`superseded` 对 pending 复用原修订 fence，对 completed 比较当前阶段与原终态时间；计数可能包含历史收据。不同操作若具有相同的已存终态时间，可能无法区分；重放完成若保留较早终态时间，也可能显示 superseded，该诊断不授予恢复权限。
- `reason` 是不含正文的固定诊断。缺失、损坏、超限、歧义或不可访问证据显式显示 unavailable；已释放或变更的 workspace 租约不能授权恢复 pending 工作；已完成收据在 workspace 释放或租约过期后仍可查看，历史完成事实不要求当前修复权限。新增诊断不输出源路径、transcript、marker 载荷或重放凭据。

`libra agent doctor --json` 继续只检测，使用原 pending-finalizer finding 和 live-marker 计数。session show 是仓库范围的指定会话视图，也可查看同仓库其它 worktree 的会话；pending/owner 描述已存会话，并不保证当前 worktree 有修复权限。只有从原 worktree/workspace 显式 `libra agent doctor --repair` 才尝试现有受 fence 约束的恢复：保留有效 owner；没有持久 checkpoint/artifact 时须由 provider 重投或显式 import。repair 不重新打开任意 provider 源，不绕过已变更租约，也不为已经持久化的终态重复创建 checkpoint；重复 repair 幂等。

新增字段只影响 Codex session show，list 与其它 provider 输出保持原合同。Codex metadata 非文本或超过 1 MiB 时，旧 capture_status/error 字段不再显示，terminal_capture 显式为 unavailable，不回退原始数据。本机合成验证不等于真实 Codex 模型验证，先编译当前版本再运行：

```sh
cargo build --locked --features test-live-agent --bin libra
source .env.test
cargo test --locked --features test-live-agent --test codex_capture_local_test codex_finalize_ -- --ignored --test-threads=1
```

这些新增 Codex 用例不在 CI 运行。


## Codex hook 观察日志（CX-03 开发中）

公共 `libra hooks codex` 与兼容 `libra agent hooks codex` 在 ingress 校验、仓库绑定成功后，记录相同的固定 runtime 结果，各入口既有退出策略保持。未校验或未知事件不能取得可信日志 scope，不创建持久日志；继续保留现有固定诊断。其它 provider 不变。

每条仅含 schema、仓内序号、固定 provider `codex`、固定 hook 命令 `event`、结果 `runtime_ok` / `runtime_failed` / `terminal_failed` 和 `count=1`。不含会话标识、正文、model、源文件/仓库路径、凭据或原始错误。`runtime_ok` 只表示 runtime 返回无错，不能证明取得 thinking 或写入 checkpoint；采集事实和终态恢复以 catalog 为准。

核心 capture owner 独占日志 IO：`.libra/private/capture-diagnostics/` 下两个固定槽 `0.jsonl`、`1.jsonl` 各最多 5 MiB，另有空 `lock`，轮转只清空旧槽。Unix 目录/文件须当前 owner、0700/0600，文件为普通且单链接；Windows 复用受保护 owner ACL、句柄相对打开和拒绝 reparse 的原语。本机 macOS 测试不能证明 Windows 运行通过。权限、链接、大小或内容异常保留原物，汇总显示 unavailable。

`agent status --json` 与 `agent doctor --json` 新增 `data.capture_diagnostics`，人类输出同样显示窗口计数。`state` 为 `absent`、`available` 或 `unavailable`；`window=retained_records`、`may_have_gaps=true` 始终适用。计数及可选首末序号仅覆盖仍保留且有效的记录，不是累计次数或成功证据。读者不创建目录/锁、不修复或回显日志正文；初始化不完整、锁竞争、坏记录或不安全文件显示 unavailable，不返回部分计数。

日志 best-effort，不递归记录自身错误；非阻塞锁、有界尾读均按原 capture deadline 与 20ms 软 IO 窗口中较早者检查，不重置或延长采集预算。同步文件系统调用本身不能被软检查抢占。竞争、预算耗尽、进程退出、IO 失败可能遗漏记录，不改变 runtime 结果、pending receipt 或入口退出策略。10MiB 上限仅约束此观察日志，既有全局 tracing logger 独立。

若竞争消退后仍 unavailable，先停止 hook 回调并检查日志目录本身的链接/权限，不跟随链接地移走该日志目录，再由下次已验证 hook 重建；需要排查时保留移走的内容。这只重置观察窗口。不要删除上级 private 目录、catalog 或 pending capture receipts。

Unix 上级 `.libra/private` 也须属于当前用户且为 0700。确认它是预期目录并检查 ownership 和内容后，在原位恢复 owner-only 权限，不删除目录；日志读者不会修正父目录。既有 capture-key writer 有独立规则：拒绝其它 owner 或 group/other 可写目录，并在采集前把允许的当前 owner 目录收紧到 0700。

以下合成回归仅本机运行，绝不加入 CI：

```sh
cargo test --locked --features test-live-agent --test codex_capture_local_test capture_diagnostic_ -- --ignored --test-threads=1
```


## Codex 会话摘要（CX-07 开发中）

`libra agent session show <session-id> --json` 对 Codex 新增 `data.summary`，包括 `status`、固定 `source_status=unproven`、`as_of_checkpoint`、`model`、`title`、`prompt_count`、`token_usage`、`modified_file_count`。人类输出显示同样事实；list 和其它 provider 输出保持。

摘要来自一个已保存的 checkpoint snapshot，按既有 committed checkpoint 次序（created_at 倒序，再 checkpoint ID 倒序）选择，出处由 as_of_checkpoint 明示。它是源报告的 snapshot 事实，不代表实时 usage、全会话累计、native 资格或所有观察到的 turn 已保存。生命周期、pending probe 和 checkpoint 身份先固定在同一数据库读快照中，再由 typed reader 在预算内验证普通 metadata 的角色、类型和内容哈希；不重读 provider，不读取 transcript 或 reasoning artifact 正文来补字段。

status 为 available / partial / pending / unknown / unavailable；没有已知值的字段是 JSON null、人类输出 unknown。pending receipt 或正在导入时，即使旧 checkpoint 有 token 值，也将当前 usage 显示 unknown。只有 metadata 确实测得 0 才显示 0。旧格式缺少 extraction 仍 unknown；角色/对象损坏、catalog 畸形或读预算耗尽显示 unavailable。title 仅使用已保存的 typed extraction，不从 prompt 猜测。model/title 在输出前做大小限制、凭据脱敏和终端控制字符移除。

live metadata 原有 extraction 槽；历史 Codex import 现在通过既有私有 retention response 携带同一 redacted snapshot 的有界可选事实，并写入该槽。可选 helper 事实损坏时只省略摘要，普通 transcript 仍保留。重放、追加不改已完成 checkpoint 对象和 V1 records/digest；不新增摘要 cache/表或源重读 fallback。

managed Codex hook 现在会在原采集预算内，从绝对 CODEX_HOME（优先）或当前 home 的 .codex 定位会话 rollout。源文件首条 session_meta 的 ID 和 cwd 必须与已验证 hook 作用域一致；普通可读 thinking 与可选摘要保存在同一 checkpoint，source_status 仍为 unproven。删除 provider 源后，默认导出与摘要仍来自已保存对象，摘要 reader 不会补读源。当前仅合成 fixture 的真实 hook/store/export 链路通过；真实 Codex 模型和 Windows runtime 尚未验证。hook 自报任意路径不授予 fallback 资格；无采集 deadline 时不读取 Codex 源。

以下回归只在本机显式执行，不加入 CI：

```sh
cargo test --locked --features test-live-agent --test codex_capture_local_test codex_session_summary_ -- --ignored --test-threads=1
cargo build --locked --features test-live-agent --bin libra
cargo test --locked --features test-live-agent --lib codex_session_summary_ -- --ignored --test-threads=1
```

摘要的人类可读输出同样包含来源状态，以及已知的 cached、reasoning、total token 计数；文本会剥除终端、双向文本和不可见显示控制字符。

pending 包括当前 `capture_status=retryable` 与旧 `retryable_error`。CX-22 与 CX-07 摘要共用此 receipt 判断，因此没有 ledger 行的 retryable receipt 也会将摘要 usage 标为 unknown。导入进程崩溃后会保持 pending，直到后续导入恢复持久化的租约状态；租约过期不能证明采集已完成。partial 也包括显示字段经过清理。

显示过滤只覆盖终端控制及常见双向、零宽、tag、行间注解控制字符，不执行完整 Unicode 规范化。pending 仍可能携带部分 snapshot 计数，不能据此认定这些计数完整。库测试前的 build 必须使用当前源码及相同 feature。


## Codex 来源采集（CX-08 开发中）

使用 `libra agent enable --agent codex` 安装的 managed hook；其命令已经携带采集预算。覆盖默认目录时，`CODEX_HOME` 必须为绝对路径。发现从最新 rollout 目录开始，并受原预算和数量上限约束。scope 不匹配、链接、源超限和预算耗尽只能返回 partial/unavailable，不能证明 thinking 已完整取得。首 header 的 cwd 必须与同一个 worktree 子目录一致；已知 macOS 系统目录别名按等价路径比较。

普通 checkpoint 保留脱敏 rollout，包括可读 `reasoning.summary[].text`；decoded credential 值在持久化前扫描，opaque encrypted content 保持 opaque。本路径不增加 native 来源资格，也不改变已有 V1 coverage 记录及已完成 checkpoint 的身份。

内部 index reader 只接受显式提供的、已关闭且一致、无硬链接的单文件备份，不自动打开热 `state_5.sqlite` 或复制 WAL。发现 sidecar、源变化/截断、迁移失败/不支持、虚拟表 schema 或资源超限时为 unavailable，不报零会话。该 reader 留待对账 consumer 使用，本卡没有新增公开 index 命令或配置项。

新增 Codex 回归只在本机执行，必须开启 feature 并显式运行 ignored：

```sh
cargo test --locked --features test-live-agent --test codex_capture_local_test codex_index_ -- --ignored --test-threads=1
cargo build --locked --features test-live-agent --bin libra
cargo test --locked --features test-live-agent --lib codex_index_ -- --ignored --test-threads=1
```



### 从已关闭的 Codex 索引备份对账（CX-22 开发中）

```sh
libra agent doctor --codex-index-backup ./closed-index.sqlite --json
libra agent doctor --codex-index-backup ./closed-index.sqlite --codex-index-page 2
```

显式提供已关闭且一致的备份。相对路径按调用目录作词法解析；
backup 与 `--repair` 互斥，page 取值 1–10 且必须提供 backup。
未提供 backup 时原 doctor 输出不变。此视图不打开 home 热数据库、不读 rollout
正文、不导入、修复或 hydrate 对象。

JSON 的 `data.codex_reconciliation` 区分备份覆盖区间
`cutoff_ms` 至 `coverage_until_ms` 与本次窗口锚点。每次调用重新锚定七天窗口，
分页不是稳定 cursor。`scope_candidate_count` 为全部 scope 候选，行状态和
`page_totals` 仅对应本页，null 表示未计算。超出候选集的页给出
`page_out_of_range` 和 null totals；过旧备份为 `outside_window`，
totals 也为 null。`empty_snapshot` 只表示提供的备份为空。

状态为 `stored_source/missing/missing_data/partial/pending/erased/unknown`。
stored_source 要求具名 metadata、非空本地 ordinary 对象及角色、哈希、大小和
manifest 声明的已存长度均验证通过。snapshot 的整源字节数可以与 import 的单 turn
内容长度不同。它不证明可读 thinking 或所有 turn 完整；旧/不明 metadata、本地缺失、
损坏或仅在 pack 中的对象保持 unknown。坏 Libra catalog 行不拖垮其余行；provider 索引的坏行或结构错误会令整个 section unavailable。
已验证空 transcript 为 missing_data/stored_transcript_empty；manifest 长度不一致为
unknown/stored_size_mismatch，不给 import hint。quarantined 会话、committed identity
却没有 session 的坏账本为 unknown；schema-pending 和 retryable 收据为 pending。
优先级为 erased、当前 Libra scope 的 pending 证据、目录及数据证明。reason 使用固定
snake_case key，如 scope_root_unproven、response_inconsistent、row_invalid、
timestamp_schema、budget_exhausted。

只有 proven scope 的 missing 与 missing_data 给出不带 `--yes` 的显式 import
提示；erased 不给提示。无 session 的 pending 也检查；过期 import lease 仍为 pending，
另标 `import_lease_expired`。若先前采集在 SHA-256/BLAKE3 仓库因对象格式不匹配
失败，请使用包含 helper 格式修复的构建重试原采集或 import；写入成功前仍为 pending，
doctor 不自动修复或清理先前 loose objects。预算不足明确标 `budget_exhausted`。

`scope_unproven` 表示目录归属未证明；`child_attribution_unproven` 表示 child 没有
独立已验证 source，edge 也不能证明其正文归属 parent。archived 行仍参与对账。
active 会话在首个 checkpoint 前可能为 missing_data；import hint 仅为显式预览，
不自动写入。大小写/NFC-NFD 与 macOS Data volume 可识别别名只列为 ambiguous，
不授予 source 读取权限；无法识别的 root 外 symlink 拼写排除，root 内遇到链接为 unknown。
备份 parent 目录的 sidecar 枚举超过 4096 项时令整个 section unavailable。

任何毫秒时间超出当前时间五分钟都会令备份 unavailable。上限为所有仓库合计 10000
近期候选、thread 文本 1 MiB、edge 独立上限、每个 scope 目录 4096 项及 1024 个目录
身份缓存。大目录可能因此 unknown/unavailable。一次对账共用两秒预算，catalog/
checkpoint metadata 及解压后的 tree/manifest 证明字节合计 8 MiB、ordinary 验证 64 MiB；原 doctor 扫描和
worker 收尾保留原行为。失败或截断不报可靠的零缺失。所有新增 Codex 测试仅在本机
以 `test-live-agent` + `--ignored` 运行，不加入 CI。

CX-22 固定 section reason 还包括 `schema_unsupported`、`migration_unsupported`、`integrity_failed`、`scope_unavailable`、`worker_unavailable`、`catalog_unavailable`、`scope_or_catalog_unavailable` 与 `backup_unreadable`。catalog deadline 或 typed budget 错误明确为 `budget_exhausted`；其它 catalog/scope 错误为 `scope_or_catalog_unavailable`。


### 从完整 Codex 索引补导缺失会话（CX-19 开发中）

```sh
libra agent import --codex-index-backup ./closed-index.sqlite --limit 20 --yes --json
libra agent import --codex-index-backup ./closed-index.sqlite --limit 20 --cursor 20 --yes --json
```

显式提供已关闭且一致的Codex SQLite备份，补导当前worktree catalog中缺失的会话。
范围是完整受支持历史，包含七天以前与归档行，不要求后来的毫秒迁移。此selector默认
Codex；其它agent、其它selector及restore-erased与它互斥。相对备份路径按调用目录
解析。实际读取provider正文或写trace之前仍须原交互同意或--yes。

先将整个当前scope结果按ID字节序排序，再应用cursor/limit（默认20，最多100），
之后才判定catalog是否缺失。第一页写入不会移动后续slot。跨页完整性要求使用同一
备份及不变的目录布局；任一变化后从cursor 0重跑。只表示本页slot，不是实时全局计数。

只有Missing进入原import写者，普通可读thinking沿原保存/导出链取得。已有catalog
会话在新增的非空data.existing数组中报告status=existing、agent_kind、哈希session_id
及selection_reason=existing_catalog_session，不读正文、不刷新，也不计为import成功。
它不能证明thinking完整；已有partial/metadata-only/后来追加turn的会话用原--session、
--all或--since重试。provider文件被删除不会删除已保存checkpoint。

原schema-v1的results/partial_results/skipped/failures/next_cursor语义保留；partial
沿旧合同同时保留summary和failure。existing在部分失败details和人类计数中也可见，
旧selectors不增加此数组。erased跳过019，pending/unknown/catalog预算问题逐项018，
目录不明或坏源路径逐项020。legacy/unscoped及nested worktree等slot可持续非零，应在
真实拥有的worktree使用原显式session导入恢复，不能用备份推断跨scope权限。

索引中的source路径必须绝对、root后仅Normal组件，叶名rollout-…-<同一ID>.jsonl；
auth/config/history/state等不合该形状的索引路径在源reader派发前拒绝。合格叶仍须原
configured-root、descriptor、header ID/cwd及当前权限证明。CODEX_HOME/archived_sessions
同样处理，不增加日期布局要求。路径已移动或来自其它host时逐项020，用--session走原
授权发现重试；此selector无定位fallback。索引edge不授予parent link/child读取权限。

导入循环开始前的backup/root/catalog快照/response/资源失败为018加固定reason_key，
不输出成功batch文档。
备份16MiB、全历史线程/edge各10000、合计文本1MiB、helper JSON 4MiB（转义可能使
原文未超限但JSON超限）；分页不会减少整库验证工作。初始索引阶段2s包含在原120s总
导入deadline内，原累计source限额64MiB保留。超出索引范围的源可显式session/path导入。
Windows运行及真实模型未由本开发面验证。失败/existing ID的本地映射方法见
[英文说明](../agent.md#codex-full-history-index-backfill-cx-19-development)：SHA256(provider ID)
前12个hex加sha256:，用同一已关闭备份查询原ID后再显式重试。

目录scope扫描每目录最多4096项、memo最多1024项；耗尽时，本属当前worktree的条目也可能
按layout unknown逐项020，并不证明它来自其它仓库。可改用显式session/path导入。

创建一致备份前先退出Codex。下面示例使用Python SQLite backup API、只读源和一个
尚不存在的目标文件；固定版本源为state_5.sqlite，其它版本使用其实际state文件名：

```sh
python3 - <<'PYBACKUP'
import contextlib, os, pathlib, sqlite3

home = pathlib.Path(os.environ.get("CODEX_HOME", pathlib.Path.home() / ".codex"))
state = home / "state_5.sqlite"
destination = pathlib.Path("closed-index.sqlite")
with contextlib.closing(sqlite3.connect(state.resolve().as_uri() + "?mode=ro", uri=True)) as source:
    destination.touch(exist_ok=False)
    try:
        with contextlib.closing(sqlite3.connect(destination)) as target:
            source.backup(target)
    except BaseException:
        destination.unlink()
        raise
PYBACKUP
```

不要直接传入运行中的provider数据库。reader拒绝WAL/SHM/journal sidecar；没有sidecar
本身也不能证明数据库已关闭。

若已导入部分会话后root
证明失效，会停止后续读源；错误details保留results和partial进度，未尝试slot为018并携带
固定根原因。循环中的tombstone查询失败同样保留已完成结果，使用catalog_unavailable；
截止时间耗尽时为budget_exhausted。
