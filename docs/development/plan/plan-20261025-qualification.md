> **2026-11-04 UTC FIX09 与 A9 资格证据（终态文档 D 待）：** FIX09 原测试缺陷及两次 RED 均保留；修复仅令沙箱测试内嵌套 commit 显式 `--no-gpg-sign -s`，不放宽沙箱或修改用户签名默认。签名+DCO `6819631a1d23b6110b250f8a0a1f347b2525f997` 已由上游合并，提交本机公钥实际 VALIDSIG，GitHub `unknown_key` 不称 verified。原提交自身 base run `41887408072` 部分 job cancelled，原事实不改；包含其代码的当前 `c426de6977678a60bef0d89f032c162a90686bbf` 在同树本机 A9 六阶段均 exit0：fmt、strict Clippy、双环境全量 11274/11274 passed（39 slow、0 flaky/重试/leak，50 原 ignored）、doc、network 3/3、release build；候选 binary SHA256 `cb4a08b6f90395b4ce78604dda44792d2114f8f82f3be53d6acd850d20fba26e`，terminal `8724bb2a67e1005dd5e4444c8066c4124822164c404a4eb7fd617721345fac0c`，源、用户数据、H1 后检不变。全量输出逐项审计至少 47 项测试体自行跳过，非相关功能覆盖；审计 `5ee8eca75340c4c58691069d442ce43e5e470c7524dfb55374884d8dbff43fea`，较早 v1–v3 解析错误原件保留。当前树实际 main base `41897658714` 八 job 全 success、CodeQL `41897658711` 和 Code Quality `41897658299` 均 success，收据 `6551724b02d2ffc2e2ef08912d7e8373fe9d051dfdba3325a849a283eb62b978`；历史 v0.30.92 原 base attempt1 failure 仍保留，同 SHA/ref attempt2 实际 success，F2 根因 unknown，缺失 user-decisions.json 原件未伪造。用户明确免交叉 Review，不声称审查 PASS；本段自身提交、其适用 D 与原工作区受控整合仍待，`DEP-QUAL-HANDOFF` / `DEP-TR-QUAL-REPAIRS` 尚未解除，A8 flaky 风险仍 open，H1/LTR live 不由本窗口替代。

> **2026-11-04 UTC 来源资格恢复新增 FIX-LTR-09：** 最新上游 `a0632237f596ffbcbeadb98e3c386b6dc58209a8` 的本地 A5 窗口在 nextest 10947/11274 run、10946 passed、1 failed、50 原 ignored 时 STOP，327 未运行；fmt/Clippy exit 0，后续 doc/network/build 未执行，终态 SHA256 `007d4c52ac54233dd70640459fc9986fd7b6f9826e03117a24f9f91ff599e0dd`，资源后检不变。最终失败 `rebase_exec_failure_stops_and_continue_retries_the_command` 是沙箱内嵌套 `libra commit` 请求默认签名，而 macOS sandbox 凭据读保护拒绝访问该测试 HOME 的 vault key；在安全短 `/private/tmp` 与 `/Volumes/Data` 两种独立目录复现。新增下列 test-only 修复卡，先保留原 RED，再验证明确无签名的嵌套提交不影响原 rebase 断言；不放宽沙箱或用户的签名策略。A1/A2/A3/A5 FAIL、A4 定向失败与旧 v0.30.92 D failure 均保留，新的来源窗口尚未准入；用户明确豁免交叉 Review，不冒称 PASS。

### Task FIX-LTR-09: 隔离 rebase exec 回归测试的嵌套签名

**Task type:** `implementation`  
**Lifecycle / Acceptance:** `in-progress` / `locally-accepted`（两次定向 RED 保留，修复后单例 1/1、同 target 32/32；FIX-LTR-09 签名+DCO 提交 `6819631a1d23b6110b250f8a0a1f347b2525f997` 已合入当前上游；A9 最终来源树本地六阶段 exit0，当前树 main D 成功。终态文档自身提交及其 D、原工作区受控整合仍待，不提前记 complete）。
**Current evidence:** 两个未修改源码的单例失败收据 `f60ebeda31b53bab26e62c5c35d6f2e97e67f8ce7423971d577d0ea9fbecdcb2` / `25fee5d188c7272724d7b44cacbe92585a967168d46b995d7e9bc05501b29895`；修复后单例 1/1 `864bf8b0925a69968abb001b2f9c2bace3296acf6a7edd82d4dc67e24a35da88`、完整 target 32/32 `27313e43b0aae0c74f16c409539d0cd90ce5ef15796a1de691ff5356d506b522`；测试文件 SHA256 `8a1978974f8b60514aca6790e21d0094a245d3bf645b6cf020aa684828143e68`。仅测试命令增加 `--no-gpg-sign`，未改断言或沙箱。  
**Description:** 在 `rebase_exec_failure_stops_and_continue_retries_the_command` 的测试命令中对嵌套测试提交显式传 `--no-gpg-sign`，使该 rebase 重试/引用更新用例不依赖沙箱禁止读取的 vault 私钥；保持 DCO、全部断言和沙箱凭据保护不变。  
**Out of scope:** 产品源码、沙箱策略、真实用户默认签名、其它测试及既有结果。  
**Acceptance criteria:** 原用例三次最终失败及两个单项 RED 保留；目标用例在新源码、隔离双 env 和原 sandbox 下实际通过；原 exit 23、LBR-CONFLICT-002、continue、HEAD/ref 与文件断言保持；默认签名及凭据保护不改；完成同树 fmt、strict Clippy、双 env 全量 nextest、doc、network 三例、release build、签名+DCO code-only 提交及适用 main D。  
**Verification:** 原选择器 `cargo nextest run --test compat_noninteractive_history_controls -E 'test(=rebase_exec_failure_stops_and_continue_retries_the_command)' --no-fail-fast --retries 0`；整个相同测试 target；随后最终树资格全部原门，记录 ignored、运行时 skip 和实际环境。  
**Dependencies:** A5 STOP/受保护资源后检、当前源码与共享写集核验、GOV-LTR-CO-01；后续 `DEP-QUAL-HANDOFF` 仍需旧版 D 归属和资格终态登记。  
**Implementation write set:** `tests/compat/noninteractive_history_controls_test.rs`；协调写集 `docs/development/plan/plan-20261025-qualification.md`，其余计划索引由最终交接按精确前像整合。  
**Release boundary / Version:** `code-only independent` / `N/A`，不 bump/tag/Release；提交前原生 fetch/rebase 并重新核受影响门；受控 signed+DCO 与适用 PR/main D。  
**Full-suite trigger:** 测试逻辑变更触发 T-1/T-4；最终来源树须执行本计划双环境全量、doc、network 和 build；纯文档后补按模板 v2.16 执行文档校验及适用 D。
**Docs and compatibility impact:** 产品行为、CLI、全局配置及网站无变更；本计划和 `plan-status.md` 仅记录真实证据与未完成边界。
**Security and privacy:** 只改变测试用嵌套提交，不读取或输出 vault 私钥；保留真实 sandbox、受保护数据及 H1 服务。
**Performance budget:** 不改变产品运行路径；按资格窗口 10800 秒含后检执行。
**Estimated scope / Granularity:** `S`；一个测试命令、同测试目标与协调文档，code-only independent。
**Rollback mode:** `revert`；不清理既有资源，不改变 H1 服务或历史结果。

> **2026-10-31 15:28 UTC GOV C3 本地结果对资格入口：** GOV-LTR-CO-01 在 f9 同树的唯一 C3 focused1/1、原双 env full11083/11083（25原 ignored、0失败/FLAKY/retry）实际 exit0；原 terminal `09b87ad5…`、full `d52f56fb…`，源/全库 DB/stash 后检同一。成功 Summary 解析审计 v1 假阴性 exit1 原件保留，v2 exit0/25 ignored 逐名同一；39个既有条件性运行时跳过不计相应功能覆盖。回填后七文档 B/VER7/fmt 已按最终字节 exit0（仓外 `governance-final-docs-c3/final-3/` 原件）；fresh upstream 差异、精确签名提交/推送及适用 main D 待，故 `DEP-QUAL-CODEONLY-GOV` 仍 incoming；本资格计划来源最终窗口、H1 和旧 v0.30.92 base D FAIL 均未通过。C1/P1/C2 STOP 与 R4 OAuth 无判词保留；用户仅 GOV 后续 Claude Review 豁免，不构成资格审查 PASS。精确哈希/计数见 `plan-20261015.md` GOV 卡。

> **2026-10-31 14:35 UTC GOV C2 STOP 对资格入口：** P2 两相完整前检 exit0，唯一 C2 claim 后 focused1/1 PASS，但 full 仅6539/11083运行、6469 PASS/70 FAIL/1 FLAKY/25原 ignored，4544因控制器进程扫描竞态发出的 SIGTERM 未运行；child umask0002 造成组可写 `.libra`/Pi UnsafeDirectory/模式断言失败。C2 controller exit1/STOP，原件 raw `5aede707…`、terminal `4aecb6d4…`，源/DB/stash后检同一；C3 尚属一次新恢复候选、未claim。旧 C1/P1/C2 STOP 保留，用户仅 GOV 本轮免 Claude、R4 OAuth 无 PASS，`DEP-QUAL-CODEONLY-GOV` 仍 incoming；本资格计划、H1、旧 base D FAIL 均未通过。精确收据见 `plan-20261015.md` GOV 卡。

> **2026-10-31 13:41:01 UTC GOV P1 前检 STOP 历史资格入口：** 用户限 GOV 本轮免后续 Claude Review，不将 R4 OAuth exit1 记为 PASS；f9 静态三门 exit0 后，第一条完整 nextest list 超时1800.076秒/exit -15/零清单，P1 STOP、C2 当时0 claim。新 P2 当时仅拟限编译 jobs=4，旧 P1 原件与 C1 STOP 保留。故 `DEP-QUAL-CODEONLY-GOV` 仍 incoming，本资格计划与 H1、旧 base D FAIL 均未通过；详细退出/哈希/后检见 `plan-20261015.md` GOV 卡。

> **2026-10-31 13:02:44 UTC GOV R3 静态失败对资格的影响：** 上游 f9 的 GOV 控制器 R2 旧字节静态 PASS 不等于执行准入；R3 四文件真实 FAIL/P1×3，恢复脚本另修后重审，C2 未 claim。故 `DEP-QUAL-CODEONLY-GOV` 仍 incoming，来源资格/H1、旧 base D FAIL 和 C1 STOP 不变；本资格计划未借 GOV 修复取得任何 PASS。

> **2026-10-31 12:27:28 UTC GOV 静态控制器 R1 FAIL / f9 来源索引：** 仓外控制器本机 Claude R1 为 P1×2/P2×8、`VERDICT: FAIL`，不能执行 C2；后代清理和退出后输入重核待修。独立 clone 已原生整合 `f9afd41627be53482f0d24a6931cf0b6b74962d4`，仅另一日期计划一行上游变更，七 GOV 文档已按私有 SHA/mode 清单恢复，stage 空、九 stash 留存。C2 未 claim，旧 C1 STOP、资格 base D FAIL 与 `DEP-QUAL-CODEONLY-GOV`/H1 均不变。

> **2026-10-31 12:14:50 UTC GOV b8b 来源前滚资格索引：** 独立 clone 已原生 fast-forward 至 `b8b76a6979d132bbf58acc73b8e55259ebd69dcf`；上游仅改另一日期计划一行，本卡七文档与 97f 上游原件同字节，stash 应用报错后依私有 SHA/mode 清单精确恢复本地七文件，stage 空、八份 stash 保留。R9 恢复计划 PASS 只限 97f 旧输入；b8b 执行审、C2、签名提交及适用 main D 仍待，`DEP-QUAL-CODEONLY-GOV`、来源资格、H1 与旧 D FAIL 均未解除。

> **2026-10-31 12:12:14 UTC GOV R9 计划 PASS 后资格索引：** GOV 恢复计划 R9 对冻结输入五 PASS/P0P1=0，仅为同卡 ER10 计划准入；三 P2 新字节与执行器仍须本机 Claude 执行审，C2 未认领。旧 C1 STOP、v0.30.92 base D FAIL 保留，GOV 未提交/推送/取得 main D，`DEP-QUAL-CODEONLY-GOV`、来源资格与 H1 仍 incoming。

> **2026-10-31 12:05:30 UTC GOV R8 FAIL / R9 资格索引：** GOV R8 计划审 ER10/VERDICT FAIL/P1×1；R7 G03 问题已关闭，但 C1 DB 迁移遗漏上游 `DEFER-PI01-01` 高概率来源。R9 已具名该缺陷与 owner、未证唯一写者限制和 C2 claim 前迁移 parity，AC17/VER19，待本机 Claude 新计划审；C1 STOP、旧 v0.30.92 D FAIL 保留。GOV 尚无 C2/签名提交/适用 main D，`DEP-QUAL-CODEONLY-GOV`、本计划最终资格及 H1 仍 incoming。

> **2026-10-31 11:54:15 UTC GOV R6 C1 / R7 审查资格索引：** GOV e3d R6 准入 Claude PASS、VER-7 1/1 PASS，但原双 env full 真 exit100/15 FAIL，C1 STOP；97f 上游重基后旧 C 不承接。R7 恢复计划审 ER03/ER10/VERDICT FAIL/P1×2，R8 修订 AC18/VER19 与 DB unknown/后检保护待本机 Claude 再审，一次 C2 尚未 claim。GOV 当前无签名提交/PR/main D，`DEP-QUAL-CODEONLY-GOV` 仍 incoming；本计划来源资格最终窗口、旧 v0.30.92 base D FAIL、H1 阻塞均不因此改变。

> **2026-10-31 07:54:50 UTC GOV R4 准入后资格索引：** R4 对 608efc3 旧输入真实 PASS/P0P1=0/P2×7，仅准入；资格 07:43 行已补其 07:30 修正的原件归属，新 GOV 字节待最终 C/Claude。`DEP-QUAL-CODEONLY-GOV` 和 `DEP-QUAL-HANDOFF` 未解除，旧 v0.30.92 base D FAIL/H1 不变；无 source/full/提交/push。

> **2026-10-31 07:43:04 UTC GOV R3/上游整合后资格索引：** R3 仅对 cfa 七文件准入 PASS/P0P1=0，后续 608efc3 三向整合及 P2 新字节待审；`DEP-QUAL-CODEONLY-GOV` 仍 incoming。旧 v0.30.92 base D FAIL、来源完整窗口、H1 阻塞不变；无 source/full/提交/push。

> **2026-10-31 07:30:54 UTC GOV 准入 R2 后资格前滚：** 本机 Claude R2 实际 FAIL/P0=0/P1=1/P2=9；GOV 本轮逐门计数/入口/D 补强待 R3 审查，`DEP-QUAL-CODEONLY-GOV` 仍 incoming。旧 v0.30.92 base D FAIL、完整来源资格窗口与 H1 阻塞原样保留；未运行 full、未提交/推送。

> **2026-10-31 07:15:00 UTC GOV 准入失败后来源资格前滚：** 治理卡准入 R1 实际 FAIL/P0×1/P1×4，未审候选误加的进程内 `cargo test --all` 必过门已撤回，资格计划 `DEFER-QUAL-INPROCESS-ISOLATION` 原失败保留；本计划待闭合 code-only 收口增加 `DEP-QUAL-CODEONLY-GOV`，只有 GOV 实际 main D 全绿后可交付，不追认已发布 FIX 卡。最终来源资格双环境窗口、本机 Claude、旧 v0.30.92 base D FAIL 与 H1 阻塞不变；本轮治理新字节待审，无提交/push。

> **2026-10-31 07:06:18 UTC code-only 治理前置：** `GOV-LTR-CO-01` 独立治理卡已起草、执行门 R1 实际 FAIL/P1×1 后前滚；本机七文件最终 Review、两项全量、签名提交和适用 PR/main D 均待。本资格来源的已发布 v0.30.92、同 SHA 旧 base D FAIL、最终来源双环境窗口与 H1 阻塞原样保留；本行不把治理卡验收当作资格 PASS。

> **2026-10-31 06:36:15 UTC R8 治理审查后来源资格状态：** 本机 Claude 对七份治理草稿 R8 实际 `VERDICT: PASS`、P0/P1=0，不代表本来源资格 PASS。旧文中“待 R5 审查采纳”为该时点快照；R5 实际 FAIL，R8 只审治理合同，最终来源资格需新树双环境完整窗口与其自身本机 Claude 审查。v0.30.92 同 SHA 旧 base D FAIL、producer outgoing/consumer incoming 与 H1 阻塞均保留；本行新增字节待 R9 终态审查，无提交或推送。

> **2026-10-31 06:29:12 UTC R7 后前滚：** R7 本机 Claude 实际 FAIL/P1×1（计划 06:09 旧修订行误改），旧行已在本次精确恢复；签名工具 `-S` 事实随模板/AGENTS/CLAUDE 修正，R8 待审。本 qualification 产品/来源资格未 claim，`DEP-QUAL-HANDOFF` 仍未解除。

> **2026-10-31 06:19:29 UTC 来源资格治理审查：** R6 本机 Claude 实际 FAIL/P1×1，旧时点误写已在 status/long 精确恢复；R7 待审。来源资格与 `DEP-QUAL-HANDOFF` 未解除，旧 v0.30.92 base D FAIL 保留。

> **2026-10-31 06:09:09 UTC 来源资格前滚草稿：** 本机 Claude R5 实际 FAIL/P1×1（版本改号时间线），本轮已补真实初稿 v2.15 与 05:22:01 UTC 更名 v2.16 的记录；R6 待审。`DEP-QUAL-HANDOFF` producer outgoing 未解除 / consumer incoming 保持，旧 D FAIL 不变。

# Libra 2026-10-25 来源资格前滚修复

> **2026-10-31 现行前滚恢复边界（隔离 clone 初稿 `8c8986d76f13f2d3c6d8eff8caf641faee2a334f`，现已原生前滚 `cfa86fa54cf82ea77d4a1d18dd567b7b0426bb8d`，本机 Claude 审查待）：** 用户指定后续 Libra 代码交付不再发布新版本。此前 FIX-LTR-01/05/04/02/03/06/07/08 的实际提交、版本、tag、Release、A/B/C 及 FAIL/STOP 均保留原时点；v0.30.92 的 Release 工作流 `40915183029` success 只证明该工作流，`release/v0.30.92` 同源 SHA `7c4b68c9f3237e915423dd3c6a240426bf6526b6` 的 `base.yml` run `40995496573`/job `123867538989` 实际 failure（Linux opencode-export sandbox 外层取消后 marker 1→2），未找到同 SHA/同 ref 后续成功，不能把五卡来源资格或 `DEP-QUAL-HANDOFF` 倒记完成。失败原因尚未证明；旧本机 Bubblewrap 0.9.0 与 CI 0.13.0 不同，单机诊断不归因代码。
>
> **后续执行：** 仅在现行 ER-03 归属及本机 Claude 审查 PASS 后，以独立 `FIX-*` 前滚卡处理实际代码缺陷或具名 runner 恢复；新代码卡按模板 v2.16 的受限 `code-only` 模式完整 A/B/C、signed+DCO、提交前最新上游 fetch+rebase、实际 PR/main D，不 bump/tag/Release。既有发布卡历史合同不改写。最终来源资格仍须在纳入最新上游的冻结最终树上新开受审窗口，执行原 nightly fmt、strict Clippy、双 env 完整 nextest、doc 与三例 network，以及本机 Claude 来源资格审查；旧 10800 秒窗口和 v0.30.92 C 不自动复用。终态文档需记录本次实际窗口、最终 SHA、适用 branch D 与旧 D FAIL 的归属；未取得这些证据前 `DEP-QUAL-HANDOFF` 尚未解除（本 producer 侧 outgoing、LTR consumer 侧 incoming），LTR-10 不开正式验收。现行远端前滚、并发写者及窗口预算须在新卡开工前复核，本段不预授验收 claim。

## 2026-10-27 来源资格交接复核（当前要求）

本段由用户指定的 Claude 执行 Agent 写入（2026-10-27 起草，2026-10-28 按本地 Claude R1 FAIL 前滚修订），覆盖下方 2026-10-26 入口中「当前」的现状表述；旧入口、旧计数和全部 FAIL/STOP 保留为历史。本段只登记已核事实和待决事项，不把来源资格写成完成，`DEP-QUAL-HANDOFF` 与原计划 `DEP-TR-QUAL-REPAIRS` 仍是 incoming。

**写者与所有权。** 2026-10-27T15:51Z 的首次核验是执行器观察，未留原始输出；2026-10-28T06:11Z 重新观察并保存原件（`observations-20261028/writers.txt`、`root-copy.txt`、`ancestry.txt`）：Grok 会话 `01a17bc9-9d7b-7f82-b295-4d91217837d2` 的 `last_active_at` 为 2026-10-27T10:24:30Z，无 grok 进程，`~/.grok/active_sessions.json` 为 `[]`；原仓 `.libra` 实目录、无 `.git`。root（Codex）推进 Mega2 FIX-MTR-19；其独立 Libra 副本 `/media/eli/data/libra-h0-sync-20261026` HEAD `2c87fcd`，其 `origin/main` 为 `380c262` 且是 HEAD 祖先（`observations-20261028/root-copy-origin.txt`），plan-20261015/plan-long/plan-status 三个文件与 HEAD blob 不同，未交接，本 Agent 不消费、不修改（只用 rev-parse/log/cat-file/sha256sum/merge-base --is-ancestor 读取；`root-copy-origin.txt` 第 4 行的祖先结果由执行器用 `libra merge-base --is-ancestor <origin/main> HEAD` 得到，该原件表头只写了 read-only rev-parse，未记录这条命令）。全机 Cargo/测试/发布窗口按 `DEP-TR-ENV-DIAG-CARGO` 由 root 串行交接；root 于 2026-10-28T07:49:04Z 以 `coordination/root-grant-20261028T074904Z.json` 授予窗口（范围 DOC-LTR-03 本地门，之后 FIX-LTR-06 A、C 与最终资格，各自先经准入）；截至本段冻结时本 Agent 尚未 claim、未运行任何 Cargo 命令；每个窗口仍须满足其开工条件与各自准入，未取得不编译、不测试、不发布。

**上游变化。** 2026-10-27T16:50:25Z 另一写者直接向 main 推送 `29eaa97b3854d6a337b493072374b05a8d2b3509`（`fix(index): stop writing assume-unchanged on index entries (#637)`，9 路径，含 `git-internal` 0.10.2→0.10.4、`src/utils/index_ext.rs` 等生产改动，无版本变化，无 tag）。其签名 `NO_PUBKEY FD937770ADCF0EF6B73E48DED406339EBA910E88`（GitHub `unknown_key`，见 `observations-20261028/github-verification.txt`；keys.openpgp.org 404，见 `keyservers.txt`），DCO 有；D：Build/Test 40544991374 8/8、CodeQL 40544991453 2/2 success。本 Agent 已把本地 main 快进到该提交，三份计划文件的改动按原差异（SHA256 `e7097c3b…`）原样重放，三份文件在 db331cd 与 29eaa97 的 blob 相同。2026-10-28T06:07:23Z（提交时间；PR #649 的 `merged_at` 为 06:07:24Z，见 `observations-20261028/github-8abdcf7-pr649.txt`）又合入 `8abdcf7633f8fef8f6d0994d9f0621bb074de292`（单亲、committer 为 GitHub，PR #649 的 `merge_commit_sha` 即此提交，合并方式由此推断为 squash 或 rebase；web-flow 公钥 GOODSIG/VALIDSIG、GitHub `valid`，DCO 有；只新增 `docs/development/plan/plan-20261028-status-performance.md` 并在 plan-status 计划一览加一行，无产品改动）；D：Build/Test 40641949469 8/8、CodeQL 40641949418 2/2 success（`ci/d-8abdcf7.json`）。本 Agent 在使用 root 授予的窗口前快进到该提交（`docs-change/rebase-8abdcf7/`）；快进重放时，三份计划文件的新增文字逐行与受审字节相同，upstream 新增行保留。整合范围因此为 e24c88b..8abdcf7，其中 29eaa97..8abdcf7 只有文档。FIX-LTR-06 的源随之为 8abdcf7。

**用户决定（2026-10-28T06:03:22Z，本会话 AskUserQuestion，原文存仓外 `user-decisions.json`）：**

- `DEP-QUAL-F2-GATE`：「批准限定关闭条件：F2 历史根因永久记为 unknown。FIX-LTR-06 完整 C/D 发布、F2 用例保留诊断，并且本地 Claude 关闭审查 PASS 后，F2 P1 关闭。仅限 F2。以今天的 UTC 接受 FIX04/02/03 已在 CLOSE=unknown 下发布的事实，不把 19:12:37Z 例外改写成历史批准。」该决定只改变 F2 的关闭条件，不改变其它 P0/P1 的关闭要求，也不追认 19:12:37Z 例外。
- `DEP-QUAL-CARGO-WINDOW`：「root A3 结束后交给 Libra：用户转告 root。root 确认 A3 终态和资源已关闭、明确交接后，Libra 依次使用窗口，期间 root 不启动 Cargo。」root 2026-10-28T05:59:20Z 的协调收据写明尚未向 Libra 发放窗口，Libra 保持不编译、不测试、不发布。
- `DEP-QUAL-SIGNING-KEY` / `DEP-QUAL-REMOTE-ORIGINALS`：「取不到：签名保持未验证，原件登记为缺失。用最终树上的新资格窗口作为新证据，不补造旧 PASS。」

**证据位置。** 仓外持久目录 `/media/eli/data/libra-source-qual-20261027/`：`inputs-manifest.sha256`（79 项）、`inputs/local-tmp-originals.sha256`（2959 项，本机 `/tmp` 下 FIX01/DOC01/FIX02/FIX04 历史原件的逐字节副本）、`observations-20261028.sha256`（18 项）及 `sig/`、`ci/`、`integration/`、`f2/` 各自的哈希文件。root 复核原件 `report.md`、`receipt.json`、`ci.stdout`、`jobs.stdout`、`annotations.stdout`、`compare.stdout`、`public-key-check.json`、`signature-audit.json`、`fee4bd5f-….review.md` 均存在，哈希已入清单。217cea… 迁机包仍不可得。

| 组 | 已核结论 | 未解决与 owner |
|---|---|---|
| 1 F2 | ATTRIB 维持 unknown；历史 TRY1 根因不可证。当前源有可证缺陷：全局配置库存在但不可读时，`libra --json reset --pathspec-from-file=paths.txt` 退出 129、stderr 首行为纯文本 `Warning: failed to resolve LIBRA_STORAGE_TYPE …`，stderr 不是 JSON（garbage/chmod000 两种受控复现，不存在全局库时为单一 JSON）。复现用的是 0.30.91 debug 二进制（SHA256 `874ce7d3…`，2026-10-27T07:24:13Z 构建，非受控窗口内编译）。对 db331cd 而言，其后唯一改动的源文件改动位于 `#[cfg(test)]`，源与二进制的对应是推断；29eaa97 另有生产改动（`src/utils/index_ext.rs`、`src/command/refresh.rs`、`src/command/update_index.rs`、`git-internal` 0.10.4 与 `Cargo.lock`），不涉及 `client_storage.rs` 与警告通道（`output.rs`、`error.rs`、`cli.rs`），对 29eaa97 的对应同样是推断、未复测，由 FIX-LTR-06 VER-5 的 RED 在实际树上确认。这与 TRY1 形状相同，也与 FIX05 AC-3 勾选不一致；F2 原场景中全局库不存在（`observations-20261028/f2-scenario-global-db-listing.txt`），所以不能据此认定历史根因。按 ER-03 登记下方 FIX-LTR-06。2026-10-26T19:12:37Z 例外是执行器记录，不是用户批准。 | CLOSE 未关闭（P1）。按 `DEP-QUAL-F2-GATE` 用户决定，FIX-LTR-06 完整 C/D、F2 用例诊断与本地 Claude 关闭审查 PASS 后才关闭；owner 本 Agent |
| 2 每卡 C / 最终窗口 | FIX01 C/D 原件本机可得，D 终态 `4e2009fa…` 与 job 收据 `c66879aa…` 哈希一致，可限域 carry-forward。FIX05、FIX04、FIX02、FIX03 的 C 与 e24c88b 最终资格窗口在另一台机器执行，本机无原件，只有计划中的 run ID 与计数；用户已确认取不到。FIX05 卡：VER-6 未勾却 done；VER-1 同步屏障真锁未落地；VER-4 的 F2 诊断未落地（`reset_test.rs` 与 b73 逐字相同）；AC-3 被上面的复现反证。 | 原件永久缺失（RISK-QUAL-ORIGINALS-MISSING）。不补造旧 PASS；最终源树已不同于 e24c88b，须在最终树重新执行计划最终资格窗口；owner 本 Agent（窗口待 root 交接） |
| 3 来源资格 Claude | 计划中没有任何来源资格审查 session；`fee4bd5f` 只审交接文字；`bde085f1` 是终态文档审查，原件本机不可得 | 最终源树本地验收有效后另起本地 Claude 来源资格审查；owner 本 Agent |
| 4 签名 | f33be958 用 GitHub web-flow 公钥 `968479A1AFF927E37D1A566BB5690EEEBB952194` 实际 GOODSIG/VALIDSIG（exit 0）。e24c88b..db331cd 中 15 个提交用本仓 vault 公钥 `E5F0A2A2D2D7BFFCDAD104649D3B6B4C1CD756A1` 实际验签通过。e85470a、ff30d4a、98852b7、e24c88b、e5424c5、380c262 与 tag v0.30.88–v0.30.91 均为 `NO_PUBKEY 3F5F84D5E79CEF6C414339F20005024EAF59B8E4`；该公钥不在本机任一 Libra vault（`observations-20261028/vault-key-search.txt`）、不在 genedna 的 GitHub 公钥，keys.openpgp.org 与 keyserver.ubuntu.com 均返回 404（`keyservers.txt`）。GitHub 对这六个提交为 `unknown_key`，对四个附注 tag 为 `unverified_email`，对 f33be958 为 `valid`（`observations-20261028/github-verification.txt`）。全部提交 gpgsig 头与 DCO 存在。v0.30.87 是轻量 tag，直接指向 f33be958；v0.30.88–v0.30.91 是附注 tag，均解引用到对应发布提交且为 main 祖先，tagger 为 `user <user@example.com>`（RISK-QUAL-TAGGER-IDENTITY，P2） | 3F5F84D5… 未验证，不是无效；用户确认取不到公钥（RISK-QUAL-SIGNING-UNVERIFIED）。不改写历史、不移动 tag |
| 5 整合与 D | e24c88b..db331cd 共 17 个提交、9 路径：`verify.rs` 改动在 `#[cfg(test)]` 模块内；`.cargo/config.toml` 把 `RUST_MIN_STACK` 由 16 MiB 改为 32 MiB，影响所有 cargo 启动的测试进程，触发全量；AGENTS/CLAUDE/测试说明为 blaze 用法；其余为治理文档。其中 12 个原工作区治理提交（10 个日期为 10-24/25，`ecd6150`、`cc09e57` 为 10-27）已由 Grok 以新 SHA 线性推送（`ed566b0`、`7541e9b`、`2755eb7` 不是 db331cd 或 29eaa97 的祖先），这与本计划「不直接推送原 main 整条 ahead 历史」的约定不符，现已不可撤回（RISK-QUAL-GOV-PUSHED）。之后又有 29eaa97（见上）。db331cd 的 Build/Test 40484795115 attempt 1 failure：原始日志 2108115 字节（SHA256 `30c89e18ab06a177c35a85672029cee906cfedba4fd4a7971a63181d73703d3a`）止于 11:35:07Z 的 9706/10822，之后 runner 失联，根因未证；同次运行 `init_from_git_rejects_unknown_source_format` 与 `init_from_git_rejects_unreadable_source_config` TRY1 FAIL→TRY2 PASS，原因是执行前快照含夹具 `git commit` 触发的 `git-src/.git/objects/maintenance.lock`（RISK-QUAL-GIT-MAINT-LOCK，P2）。2026-10-27T16:11:24Z 在确认 runner 在线空闲后只重跑一次失败 job（attempt 2，job 16:11:33Z 在 mi-omarchy 开始），attempt 1 保留；29eaa97 推送后，取消旧 main 运行的 workflow 40544991510（16:51:21Z–16:53:07Z）期间该 job 于 16:52:16Z 变为 cancelled。db331cd 的 D 因此未闭合，已被 29eaa97 取代 | root 独立治理增量未交接；本 Agent 最终源 SHA 的 D 尚未取得 |

**恢复顺序（候选，待各自审查）：** `FIX-LTR-06（ER-03 → A → ER-05 → C → v0.30.92 → D）→ F2 关闭审查（依 DEP-QUAL-F2-GATE）→ 在 v0.30.92 发布树执行计划最终资格窗口 → 本地 Claude 来源资格审查 → 终态登记提交及其 D`。每一步的 Cargo 窗口都要先取得 root 交接。

### 2026-10-28/29 FIX-LTR-06 A 执行记录、用户决定与新增卡（当前要求）

本段由用户指定的 Claude 执行 Agent 写入，覆盖上方「恢复顺序」中 FIX-LTR-06 之后的现状；历史 FAIL/STOP 与原文保留。本段相对证据路径以仓外 `/media/eli/data/libra-source-qual-20261027/` 为根。

**执行经过：**
- 2026-10-28：
  - root 于 07:49:04Z 授予窗口。RED0、RED1 PASS；GREEN 的 fmt、strict clippy、VER-1..VER-4 PASS。
  - GREEN 的 nextest 全量于 11:00:26Z 依用户决定 DEC-QUAL-BLAZE-FULL 停止。停止前 `command::replace::tests::delete_mutation_path_invalidates_the_cache` 已 3/3 失败，全量覆盖 6262/10832（`fix-ltr06/run-green*`）。
  - 随后 blaze 全量 TIMEOUT：ra-jit 守护进程未就绪（`fix-ltr06/run-blaze/`）。
  - 14:48:41Z 归还 root 窗口（`coordination/libra-return-20261028T144841Z.json`），root 此后未重新授予。之后的 Cargo/测试依用户指令执行，每次先在协调目录发通知，结束发收据。
- replace 用例根因已确认：夹具为 0 字节库，加上 FIX-LTR-05 的 fail-closed（`fix-ltr06/diag-replace/`）。处置为 FIX-LTR-07。
- 2026-10-29，FIX-LTR-06+07 合并树 nextest 全量（DEC-QUAL-RESTART-180）：10832 例，2 例 3/3 失败、13 例 FLAKY；15 例中 12 例的失败文字含存储配置拒绝，3 例机制未证，其余归入同一机制为推断（见 FIX-LTR-08 卡）（`fix-ltr06/run-r2-nextest/`、`fix-ltr06/r2-nextest-failures.json`）。依用户决定 DEC-QUAL-R2-SKIP-BLAZE（作答 01:35:43Z；`run-r2-blaze.STOPPED-BY-USER.txt` 中「约 01:2xZ」为执行器当时的估计，以 user-decisions.json 为准）停止 blaze 并归还窗口（`coordination/libra-complete-20261029T013714Z.json`）。处置为 FIX-LTR-08。
- 2026-10-29 R3（FIX-LTR-06+07+08 树，main `21f8e4c`）：nextest 全量 PASS，10836/10836，0 FLAKY、0 重试（`fix-ltr06/run-r3-nextest/`）。blaze 以 rustc 1.99.0 编译时，FIX-LTR-08 的 `AtomicU64::fetch_update` 报弃用警告；执行器在 blaze 跑任何测试前停止（`fix-ltr06/run-r3-blaze.STOPPED.txt`），按 FIX-LTR-08 green3 改为 `fetch_add`，并在新树上重跑全量（R4）。R3 blaze 期间 root 的 Mega2 构建并行运行（`postrun-r3-blaze.json` 记录其 stable cargo/rustc 进程；cwd `/media/eli/data/mega2-blob-index-red-20261029` 由执行器观察并写入 `coordination/libra-test-notice-20261029T054814Z.json`），Libra 未触及其目录。
- 2026-10-29 R4（green3 后的三卡树，main `ba38e30`，18 个被测文件逐文件存档于 `fix-ltr08/tested-tree-snapshot-r4/`）：nextest 全量 PASS，10836/10836，0 FLAKY、0 重试（`fix-ltr06/run-r4-nextest/`）；blaze 的 ra-jit 守护进程再次停在「loading」、未跑任何测试，依用户决定 DEC-QUAL-R4-CARGO-TEST 于 08:08:47Z 停止（`run-r4-blaze.STOPPED-BY-USER.txt`；执行器 08:08:53Z 退出），改跑普通进程内 `cargo test --all`：在窗口结束时 TIMEOUT（`command_test` 中途），已完成的 69 个二进制中 lib 15 例失败、`command_test` 已运行部分 3 例 fsck 失败，均在 nextest 下通过；依 DEC-QUAL-R4-BASELINE 在发布代码上对照，对照显示：最终树两次进程内 lib 运行合计 21 例失败，其中 17 例在发布代码上同样失败，另 4 例只在最终树失败（判为同族是推断）；依 DEC-QUAL-INPROCESS-DEFER（含接受这 4 例）登记为 DEFER-QUAL-INPROCESS-ISOLATION；其中 4 例带存储配置拒绝文字，依 DEC-QUAL-FIX08-NEXTEST-RULE 不作为 FIX-LTR-08 的依据；A 的全量门以 nextest PASS 为准（`fix-ltr06/r4-gate.json`）。
- 2026-10-29 R5（main `1e5310c` 加三卡，被测文件逐文件存档于 `fix-ltr08/tested-tree-snapshot-r5/`）：fmt、strict clippy PASS；nextest 全量 PASS，10836/10836，0 FLAKY、0 重试（`fix-ltr06/run-r5/`）；门文件 `fix-ltr06/r5-gate.json`。
- main 于 2026-10-29 前进到 `21f8e4c`（PR #651，只增 `issues/650.md` 与 `plan-status.md` 两行，与本计划工作区 14 个文件不重叠）。快进后逐文件哈希核对，然后继续（`sync-21f8e4c/`）。随后又前进到 `ba38e30`（PR #653，只改 issue 650 文档与 `plan-status.md` 中 issue 650 的两行）；本计划 `plan-status.md` 行以补丁重放，其余文件逐字节核对（`sync-ba38e30/`）。R4 之后 main 再前进到 `1e5310c`（PR #654，CLP-01：`src/utils/util.rs` 与四个 AI capture/coverage 文件的 `nonminimal_bool` 化简及一个测试）；与本计划工作区只在 `plan-status.md` 重叠（不同行，以补丁重放，`sync-1e5310c/`），另改了 `issues/650.md` 与一个测试文件；因改动了产品代码，因此在合并树上重跑 R5（fmt、strict clippy、nextest 全量），R5 PASS 后才提交和推送。

**用户决定（原文与作答时间存 `user-decisions.json`）：**
- DEC-QUAL-BLAZE-FULL（2026-10-28T10:59:49Z）：「请关闭之前的测试, 采用 blaze 测试重新开始, 并且在交接中说明采用这个更快速的方案」。
- DEC-QUAL-RESTART-180（14:59:25Z）：「請重新啓動 blaze + nextest 的測試， 堆棧設置爲 32M ，超時時間設置爲 180 分鐘，全量測試通過後，提交和推送。然後完成交接。」
- DEC-QUAL-FIX-FIRST：先修复，再跑全量。
- DEC-QUAL-FIX07-RELEASE（作答 23:51:29Z）：FIX-LTR-07 随 FIX-LTR-06 的 v0.30.92 发布。
- DEC-QUAL-R2-SKIP-BLAZE 与 DEC-QUAL-FIX08-RELEASE（均作答于 2026-10-29T01:35:43Z）：跳过 R2 的 blaze 并归还窗口；FIX-LTR-08 随 v0.30.92 发布。
- DEC-QUAL-R4-CARGO-TEST（作答 2026-10-29T08:08:20Z）：R4 的 blaze 再次停在 ra-jit「loading」、未跑任何测试，停止 blaze，改在同一 180 分钟窗口剩余时间内跑普通进程内 `cargo test --all`（CLAUDE.md 的权威验收门），通过才提交和推送；记录 blaze 在本机两次未能启动。
- DEC-QUAL-R4-BASELINE（作答 2026-10-29T09:10:42Z）：先在本机以发布代码（main `ba38e30`，不含三卡）同样进程内运行作对照；相同失败即为既有进程内隔离缺陷，登记 FIX/DEFER 并在提交前回报用户。
- DEC-QUAL-INPROCESS-DEFER（作答 2026-10-29T10:37:52Z）：登记 DEFER-QUAL-INPROCESS-ISOLATION，应用 ER-05 计划修订并复审 FIX-LTR-06 ER-05/DOCS，PASS 后三提交、推送、观察 CI，再提交网站改动。
- DEC-QUAL-FIX08-NEXTEST-RULE（作答 2026-10-29T10:56:05Z）：进程内运行观测不到 FIX-LTR-08（进程级预算共用），其失败处置与关闭只以 nextest 与 C/D 为依据；进程内带该文字的病例归 DEFER-QUAL-INPROCESS-ISOLATION；审查意见修正后复审再提交推送；不改代码。这是对 FIX-LTR-08 卡已登记规则的变更，已在提问中向用户说明；提问时写的是 3 例，复核后为 4 例（另有 `client_storage_reads_pack_sha1`，只在第二次进程内运行出现，发布代码上同文字），归入同一规则，交接时告知用户。
- 越过 G-07 只限 FIX-LTR-07 与 FIX-LTR-08 两卡。两卡各有自己的卡、审查与提交，共用 FIX-LTR-06 的一次全量 C/D。v0.30.92 成员为 FIX-LTR-06、FIX-LTR-07、FIX-LTR-08。

**blaze 使用说明（依 DEC-QUAL-BLAZE-FULL 写明）：**
- 本计划 A 阶段的全量依用户决定改用更快的 `cargo blaze test --all --no-fail-fast`（ra-jit，`RUST_MIN_STACK=33554432`、`RUSTUP_TOOLCHAIN=1.99.0`）。
- 自 DEC-QUAL-RESTART-180 起，顺序为先 `cargo nextest run --all --no-fail-fast --retries 2`、后 blaze（R4 起 blaze 部分依 DEC-QUAL-R4-CARGO-TEST 改为进程内 `cargo test --all`，其结果依 DEC-QUAL-INPROCESS-DEFER 延期；最终门为 R5 的 nextest 全量），合计不超过 180 分钟，两者栈均为 32 MiB。
- 与 nextest 的差异：
  - 进程内线程并行，无重试；
  - 工具链 1.99.0，nextest 为 1.98.1；
  - 运行时 skip 不可见（doctest 两者都不跑：A 的全量没有 doctest 证据，由 CI 的 `cargo test --doc` 与最终资格的 doc 门覆盖）；
  - `.config/nextest.toml` 的测试组互斥不适用。
- blaze 结果按 `work/blaze_summary.py` 判定，盘点下限为 170 个二进制、10500 例。
- 远端 D 仍为 nextest。C 窗口与最终资格窗口是否也用 blaze，待交接时请用户确认；确认前按计划原文用 nextest。

**FIX-LTR-06 卡的更正（C 冻结时以本段为准，原卡文字保留；其中写集与 Description 两处措辞已就地改正，原文见 `review-R6/` 与 `card-correction.md`）：**
1. VER-3 用例更名为 `json_local_storage_type_warnings_leave_stderr_json`。探测证明 `init` 不输出 local 警告，改用有提交仓库的 `cat-file -p HEAD`（`fix-ltr06/card-correction.md` 第一项）。写集与 Description 的两处措辞按 R6 改正。
2. A 不做 release 构建，default/keyring release 构建、安装与资源后检只在 C 执行。`--retries 2` 下的 FLAKY 按 (target, case) 逐条登记，新 flake 不以重试通过视为通过。RED 的失败原因逐条核对（第三项）。
3. GREEN 停止与改用 blaze 见上（第四项）。原 10800 秒 A 窗口已随 blaze TIMEOUT 与窗口归还结束，不续用。
4. A 的全量在 FIX-LTR-06 + FIX-LTR-07 + FIX-LTR-08 合并树上重跑（第五项）。
5. Release boundary 仍为 `independent`，但 v0.30.92 同时携带 FIX-LTR-07 与 FIX-LTR-08 的提交；C/D coverage 为 `self`，并覆盖这两卡。新增依赖边：`FIX-LTR-07 -> FIX-LTR-06`、`FIX-LTR-08 -> FIX-LTR-06`。
6. 提交顺序：FIX-LTR-06 → FIX-LTR-07 → FIX-LTR-08（含本计划与 plan-status 的治理文字）→ v0.30.92 版本面。重叠文件按 FIX-LTR-08 卡的 Commit split 用逐字节存档拆分（`commit-20261029/commit.py`）。网站：主仓推送且 CI 通过后，在 `../libra-backend` 的 `cf` 分支提交 FIX-LTR-06 的两页，内容与 `fix-ltr06/impl/site.diff` 逐字节一致，先跑站点检查再推送，并记录提交哈希。

**FIX-LTR-05 系列注记：** FIX-LTR-05 父卡与 05P、05Q、05R、05S、05T、05U、05V、05W、05X、05Y、05Z 各卡 Performance budget 中「不增加重试 / 不新增重试 / 保持 200ms 配置库锁上限 / 不延长锁」各条（共 12 处），就仓库库被占用时的存储配置读取一项，由 FIX-LTR-08 前滚取代。其余内容不变，原文保留。

### Task FIX-LTR-07: replace -d 用例使用已迁移的仓库数据库

**Task type:** `implementation`（仅测试）

**Lifecycle / Acceptance:** `in-progress` / 空（ER-03 R2 PASS，session `dbbe2623-2a1b-4629-a57f-c3eb62ca2ae6`；VER-1..VER-4 PASS，`fix-ltr07/run-ver/`；ER-05 PASS，session `ce62cf45-699c-4aac-bed8-1102ebd1f6bd`；待 FIX-LTR-06 的 C/D）

**Description:** 单一行为轴：`command::replace::tests::delete_mutation_path_invalidates_the_cache` 的夹具。`repo_fixture()` 只写一个 0 字节的 `libra.db` 标记；该用例先用 `ClientStorage::init(objects).put` 写入对象，触发后台对象索引登记，在空库上执行全部 schema 迁移（strace 下约 150 次带日志的事务、约 2.5 秒）。紧接着 `util::objects_storage()` 读取 `LIBRA_STORAGE_TYPE` 时仓库库仍被占用，200 ms 忙等待超时；按 FIX-LTR-05 的 fail-closed 规则返回被拒绝的客户端，`resolve_any` 退回提交解析并失败。CI 上该用例通过（推断为迁移在 200 ms 内完成）；本机当前稳定失败，0.30.87 时本机通过（推断由旧的告警降级掩盖；0.30.88–0.30.90 的 C 在另一台机器执行，本机无这几个版本的证据）。修复为该用例使用 `crate::utils::test::setup_with_new_libra_in` 建立真实、已迁移的仓库，并把串行车道从 `serial(cwd)` 改为 `serial(cwd, env)`（该 helper 经 `mark_test_process_non_interactive` 写进程环境、init 读环境与全局配置，进程内运行器需要 env 车道），同步 `tests/SRC_SERIAL_REGISTRY.tsv` 与 `tests/SRC_SERIAL_CENSUS.tsv` 的对应行；断言与被测路径（真实 `replace -d`、原始 oid 经仓库存储解析）不变。

**Out of scope:** 不改产品代码；不改 FIX-LTR-05 的 fail-closed 语义或 200 ms 上限（FIX-LTR-05 卡已具名保持；注：其后的 FIX-LTR-08 保持每次尝试 200 ms，另加进程级有界重试）；`repo_fixture()` 其余三个用例（`replace.rs:608`、`:634`、`:684`）不写对象、不受影响，不改；后台索引写入与前台存储解析的自身争用在慢盘上的产品风险登记为 RISK-QUAL-INDEX-SELF-CONTENTION（P2，尚未排期；关联计划中既有的 RISK-QUAL-STASH-QUIET（SQLite code 5 与存储配置降级），继承其「资格计划完成前登记 FIX/DEFER 并取得 baseline」的义务；fail-closed 下其后果由告警升级为命令失败）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 夹具为 0 字节库 | `src/command/replace.rs:591-597` `repo_fixture` |
| 失败机制 | 临时诊断：`objects_storage().configuration_variable()` 为 `Some("LIBRA_STORAGE_TYPE")`，`get` 报 `failed to read storage configuration for LIBRA_STORAGE_TYPE`，耗时 206 ms（`fix-ltr06/diag-replace/H-diag.out`） |
| 等待迁移完成即通过 | 临时在 `objects_storage()` 前等待 5 秒：`configuration_variable()=None`，用例 PASS（`I-wait5s.out`） |
| 后台迁移事务 | `strace.txt`：空库上 journal 的 O_CREAT 与 unlink 各 150 次，持续约 2.5 秒 |
| 非 FIX-LTR-06、非环境、非 cwd | `diag-replace` A–G：GREEN 环境、隔离 HOME、无 env、生产文件恢复为 d0a5c28、非仓库 cwd 均失败 |
| 历史 | 本机 0.30.87 通过 0.220–0.230 秒（`inputs/local-tmp-originals/` 的 fix-ltr01-A/C、fix-ltr02-A1/A2/C1、fix-ltr04-A1/C1-draft）；CI 在 db331cd、29eaa97 通过 0.17–0.49 秒（`ci/job-122123779875.log`、`ci/job-122333596291.log`） |

**Acceptance criteria:**

- [ ] AC-1: 该用例在本机与 CI 都稳定通过，不依赖等待或重试（修复后后台写入只剩单行索引登记；「约十几毫秒」是由 strace 均值推算的每事务耗时（约 2.5 秒 ÷ 150 ≈ 17 ms），不是在已迁移库上实测的单行登记；本机空载连续 20 次单例运行作补充证据，满载证据由 FIX-LTR-06 合并树的全量与 CI D 提供）。
- [ ] AC-2: 断言、被测函数与路径不变；只替换夹具建立方式。
- [ ] AC-3: replace 模块其余用例不受影响。

**Verification:**

- [ ] VER-1: `source .env.test && source .env.live-test && cargo nextest run --lib -E 'test(command::replace::)' --no-fail-fast --retries 0`（完整模块；修复后全部通过。修复前 RED 不在本卡 VER 合约内，证据为 `fix-ltr06/diag-replace/` A–D、H 与 FIX-LTR-06 GREEN 全量中该用例 3/3 失败（树 8cfcff1d…））。
- [ ] VER-2: `source .env.test && source .env.live-test && cargo test --lib command::replace::`（进程内并发运行，覆盖 env/cwd 车道）。
- [ ] VER-3: `source .env.test && source .env.live-test && cargo test --test compat_serial_registry`。
- [ ] VER-4: `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`。

**Full-suite trigger:** none（注：合入 FIX-LTR-08 后，本卡 AC-1「不依赖等待或重试」不能在合并树上单独观察；本卡 VER 在只含 FIX-LTR-06+07 的树上执行。仅测试；完整默认套件由 FIX-LTR-06 的 A/C 覆盖，本卡改动包含在其被测树中；`tests/SRC_SERIAL_*.tsv` 属测试基础设施，相关 compat 守卫列入 VER-3）。

**Dependencies:** `DEP-QUAL-SOURCE`（d0a5c28）。新增依赖边 `FIX-LTR-07 -> FIX-LTR-06`。执行环境依据：Libra 已于 2026-10-28T14:48:41Z 归还 root 窗口（`coordination/libra-return-20261028T144841Z.json`），此后未取得新的 root 授权；本卡 VER（2026-10-29T00:06:43Z–00:12:25Z）与 FIX-LTR-06 合并树全量的依据是用户直接指令 DEC-QUAL-RESTART-180 与 DEC-QUAL-FIX-FIRST，以及 14:59:25Z 发给 root 的测试通知（00:13:56Z 的刷新通知在 VER 之后发出）。VER 合约 `deviations.CARGO_BUILD_JOBS` 中「the window is exclusive under root's grant」是沿用 DOC-LTR-03 合约的过时文字，冻结合约不改，以本段为准。

**Deliverables:** N/A。

**Implementation write set:** `src/command/replace.rs`（仅 `delete_mutation_path_invalidates_the_cache` 的串行属性、夹具改为真实仓库、cwd/scope guard 移到写入对象之前，及注释）、`tests/SRC_SERIAL_REGISTRY.tsv`（该用例一行：`lane:cwd+env`）、`tests/SRC_SERIAL_CENSUS.tsv`（该用例一行：`touches=env+cwd`）、本计划、`docs/development/plan/plan-status.md` 本计划行。

**Release write set:** N/A（依用户决定 DEC-QUAL-FIX07-RELEASE，本卡改动随 FIX-LTR-06 的 v0.30.92 发布，不单独 bump）。

**Docs and compatibility impact:** N/A（测试代码，无外部可见行为）。

**Rollback mode:** `immutable-release`（随 v0.30.92 发布后只能前滚）

**Estimated scope:** `S`

**Version increment:** `N/A`

**Release boundary:** 依用户具名决定 `DEC-QUAL-FIX07-RELEASE`（user-decisions.json；用户作答 2026-10-28T23:51:29Z，记录于 23:52:42Z），仅本卡越过 G-07：作为单独的卡与单独的提交，并入 FIX-LTR-06 的 v0.30.92 发布，共用一次全量 C/D。

**C/D coverage from:** `FIX-LTR-06`

**Granularity:** `type=implementation; axis=replace -d 用例夹具的数据库状态; recovery=revert; complete=yes; self-contained=yes; AC=3/8; VER=4/8; landing=1; prod-files=0; scope=S; deps=DEP-QUAL-SOURCE; writeset=no-overlap; release=G-07 user exception (rides FIX-LTR-06 v0.30.92); split-from=FIX-LTR-06 A GREEN; exception=N/A; governance=DEC-QUAL-FIX07-RELEASE`

### Task FIX-LTR-08: 仓库库被其他写者占用时，存储配置读取在进程级有界预算内重试再判定

**Task type:** `implementation`

**Lifecycle / Acceptance:** `in-progress` / 空（ER-03 R3 PASS，session `6254861f-e64f-4956-98f8-74cd94f98cdb`；VER-2 RED PASS，`fix-ltr08/run-red/`；green2 后 A1 PASS，`fix-ltr08/run-green2/`，A2 PASS，`fix-ltr08/run-ver4b/`；ER-05 PASS，session `7d2e6c72-2dbd-4180-9446-fc99b33e32df`；C/D 由 FIX-LTR-06 覆盖）

**Description:** 单一行为轴：读取对象存储配置时，若仓库数据库（`.libra/libra.db`）只是被写者占用，不再在第一次 200 ms 忙等待超时后就按「仓库库不可读」fail-closed，而是退避后重试。占用判定看内层 SQLite 错误文字是否为 `database is locked`、`database schema is locked` 或 `database table is locked`；写者可以是其它进程，也可以是本进程的后台线程。

所有这类等待共用一个**进程级**预算（5 秒）。预算用尽后，同进程后续读取都只尝试一次，与现在相同。预算内一直忙、预算用尽后忙、任何非忙错误，结果都与现在逐字相同：按 FIX-LTR-05 fail-closed，只报设置名。

实现落在 `src/utils/client_storage.rs` 的 `read_config_env_value`（`:3589`），拆为单次尝试函数加重试外壳：
- 单次尝试只在 Repository 分支标记「忙」，两处判定点：
  - 连接返回的 `io::Error` 文字（`:3627-3638`）。该连接函数内部完成 schema 检查，并在需要时就地迁移；其 DbErr 已被拼进这个 `io::Error`。
  - `ConfigKv` 查询的错误（`:3648-3650`）。此查询由全局库与仓库库两个分支共用，只在 `role == Repository` 时标记。该错误是 `anyhow::Error`，外层 context 为 `failed to query config_kv`（`src/internal/config.rs:175`），其 Display 不含内层 SQLite 文字，所以按整条错误链判定（`is_sqlite_busy_error`）。
  - 只看格式化前的内层错误文字，不看拼入路径后的外层字符串。匹配做法同 `src/internal/reflog.rs:31-34`。
- 全局库分支、SystemConfig/Derived 分支以及解密失败，一律不标记忙。
- 重试外壳：判忙且进程预算有余时，按 50 ms × 尝试次数退避（上限 500 ms，且不超过预算余量），然后整段重试（连接、内部 schema 检查/迁移、查询）。本次判忙尝试的耗时加上休眠，计入进程级已用预算（原子量）。预算已用尽时直接返回原错误，不计入。
- 不提供生产开关。测试经子进程隔离预算（见 VER-1），不需要复位函数。

现实影响：自 v0.30.88 起，任何在同一仓库并发写库的 libra 进程或线程，都可能让读对象的命令以 `LBR-IO-001`/`LBR-REPO-002` 失败。写者包括另一终端写对象、后台对象索引登记、命令自身的索引修复预检、IDE 插件。0.30.87 时这条路径只打印警告并退回本地存储。

本卡承接 RISK-QUAL-STASH-QUIET 的仓库库部分（计划 FIX-LTR-04 卡「五项P2处置」第（1）项：资格完成前为 SQLite BUSY 导致的存储配置问题登记 FIX/DEFER 并取得 baseline，锁持有者当时未证）与 RISK-QUAL-FIX05-VER-GAP（风险登记表该行：存储配置锁故障复现则另立 FIX 卡）。
- 关闭条件：本卡 C/D 完成，且 FIX-LTR-06 合并树的 nextest 全量中不再出现带该文字的失败（依用户决定 DEC-QUAL-FIX08-NEXTEST-RULE，2026-10-29T10:56:05Z：进程内运行中进程级预算由同进程上千用例共用，很快耗尽，之后的行为与发布代码相同，所以进程内运行不是本卡的观测点）。
- RISK-QUAL-STASH-QUIET 当初观察到的「退回本地」警告可能来自全局库分支，该部分转入 RISK-QUAL-GLOBAL-BUSY-DEGRADE，不由本卡关闭。

本卡显式前滚取代 FIX-LTR-05 系列卡 Performance budget 中「不增加重试 / 不新增重试 / 保持 200ms 配置库锁上限 / 不延长锁」的条款（FIX-LTR-05 父卡与 05P、05Q、05R、05S、05T、05U、05V、05W、05X、05Y、05Z 各卡的 Performance budget，共 12 处）。
- 范围仅限上述 Repository 读取：每次尝试的 200 ms 不变，新增的是进程级重试。
- 依据是用户的发布决定 DEC-QUAL-FIX08-RELEASE，加上本卡自身的 ER-03 准入。
- 历史卡文字不改，由计划 2026-10-28/29 段「FIX-LTR-05 系列注记」一处集中注记指向本卡。FIX-LTR-07 卡 Out of scope「不改…200 ms 上限」已加注。

**Out of scope:**
- **全局配置库忙**（`DatabaseRole::GlobalConfig` 分支，`:3596-3618`）：后果仍是既有的退回本地加警告（输出通道由 FIX-LTR-06 管），0.30.87 起即如此。RISK-QUAL-GLOBAL-BUSY-DEGRADE，P2，open，尚未排期。
- **需要 schema 升级的仓库库**：连接时就地迁移。
  - 部分子步骤保留忙文字，会按本卡重试（`db.rs:744`、`:752`、`:759`、`:764`、`:817`、`:843`、`inspect_supported_schema`）。
  - `run_builtin_migrations` 的外层 context 会丢掉忙文字（`db.rs:839-841`、`migration.rs:2816/2834`）。
  - 读锁升写锁时 SQLite 不走 busy handler，直接返回 BUSY（`db.rs:395-401`），但该错误仍带「database is locked」文字，只要外层保留文字就会被重试。
  - 因此这条路径「部分重试、部分立即 fail-closed」，立即 fail-closed 的只有 `run_builtin_migrations` 丢掉文字的那一处。RISK-QUAL-STORAGE-MIGRATE-BUSY，P2，open，尚未排期。
  - 重试会重跑迁移/top-up，这是既有行为，本卡不改变。
- **不经 `read_config_env_value` 的读取**（`inspect_global_config_schema_future_at_path`、`repo_id_prefix_for_configured_remote` 等）：不改，状态同现在。
- **对象索引后台写入的批量化、事务合并或改 WAL**：RISK-QUAL-INDEX-SELF-CONTENTION 保留 open；补注其后果现受本卡预算约束，争用本身未处理。
- **测试中进程内写对象后不等后台索引排空就启动子进程的写法**：不逐个改测试。
- **200 ms 以外的其它锁超时**：不改。
- **FIX-LTR-05 的 fail-closed 判定本身**：不改。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 全量失败 | FIX-LTR-06 A 合并树 nextest（`fix-ltr06/run-r2-nextest/full.junit.xml`，10832 例：2 FAIL、13 FLAKY）。逐例见 `fix-ltr06/r2-nextest-failures.json`，按每次失败的完整文字（含 stdout 中的 JSON 信封）匹配后重生成。15 例中 12 例至少一次失败含 `failed to read (object) storage configuration for LIBRA_STORAGE_TYPE`。另 3 例机制未证：merge restart 未打印 stderr；am --continue 只有「am --continue failed」；diff_rename_limit 截断在 `failed to store object for 'exac`。锁持有者只在 gc 例查明，其余 11 例归入同一机制是推断 |
| 历史 | 同机 0.30.87（FIX-LTR-05 之前）的 6 份 JUnit 原件中，这些用例全部 0 重试通过（`inputs/local-tmp-originals/`） |
| 无负载也复现 | 修复前对照（`fix-ltr08/baseline/summary.txt`，已构建二进制单例顺序各 20 次）：gc 4/20 失败；fetch mirror 11/20 失败；worktree linked fetch 0/20 失败，单独运行未复现。15 次失败全部含存储配置拒绝文字（`baseline/*.out`）。另见 `diag/D1-*` |
| 锁持有者 | `diag/S-6.strace` 与 `S-6.attribution.txt`（`attribute_locks.py` 按 trace 顺序把 fd 对到 `.libra/libra.db`）。gc 子进程存活的约 0.8 秒内：<br>· 测试进程完成 36 次写提交（PENDING/RESERVED/EXCLUSIVE 字节上的 F_WRLCK）。这些提交来自 10500 次进程内 `save_object` 排入的后台索引登记，每个对象一个连接、一次 `synchronous=FULL` 写提交（`client_storage.rs:4240-4290`）。<br>· gc 进程自身的索引修复预检也完成 1 次写提交。<br>· gc 进程在 PENDING 字节取共享锁 51 次，其中 15 次 EAGAIN。<br>数据库为回滚日志模式。strace 放慢后该次未超 200 ms，用例通过。写者是密集的短提交串，重试有效靠的是多次探测，而不是等过一次占用 |
| 200 ms 与无重试 | `client_storage.rs:3579-3580` 注释「200 ms busy timeout so background storage init cannot block on foreground writers」；FIX-LTR-05 系列条款见上；当时的降级只是警告 |
| 未决结果不缓存 | `util.rs:1392-1394`；FIX-LTR-05X/05Y/05Z 卡 Performance budget：每次 `objects_storage` 都重新解析。按单次读取设预算会累加，所以改用进程级预算 |
| 写者一侧已有重试 | 后台索引 `INDEX_UPDATE_MAX_ATTEMPTS = 12`（`client_storage.rs:1161`、`:4199-4217`，合计约 12×0.2 s + 0.1 s×(1+…+11) ≈ 9 秒）；reflog `SQLITE_BUSY_MAX_RETRIES = 15`（`reflog.rs:28`）；普通前台连接默认忙超时 30 秒（`db.rs:149`） |
| 预算依据 | 5 秒：长于已观测最长单次占用（FIX-LTR-07 空库迁移约 2.5 秒，`fix-ltr06/diag-replace/strace.txt`）的两倍，短于写者侧约 9 秒的耐心，远短于前台默认 30 秒。S-6 中读者共享锁探测失败比例约 15/51。进程级预算使一次命令因忙而多等的总时长不超过约 5 秒，另加之后各次读取的单次 200 ms 尝试 |
| hook 路径 | 生产中的 capture 写入使用 `init_local_existing`（`checkpoint.rs:4900`、`recovery.rs:1167`，不读配置）；`ClientStorage::init` 的其它命中在测试中。间接路径未穷举，登记 RISK-QUAL-HOOK-STORAGE-WAIT（P2，open）：托管 hook 若经间接路径初始化存储，最多多等约 5 秒 |
| 二进制 | 诊断与对照用测试二进制为 nextest 构建（rustc 1.98.1）；`target/debug/libra` 已被中止的 blaze 以 rustc 1.99.0 重链（`diag/binaries.sha256`、`baseline/context.txt`）；源码相同。命令、cwd 与环境说明见 `diag/commands.txt` 与 `baseline/run_baseline.sh` |

**Acceptance criteria:**

- [ ] AC-1: 仓库库（当前 schema）被另一进程以写锁占用、且在进程级预算内释放时，存储配置解析成功（无 `LIBRA_STORAGE_TYPE` 拒绝），随后对象写入与读回成功。
- [ ] AC-2: 占用持续到预算用尽时仍 fail-closed：错误文字、只报设置名、不建可写本地后端、不进对象缓存，均与修复前相同。该次耗时不少于预算。预算用尽后，同进程下一次解析只尝试一次，耗时回到单次尝试量级，仍 fail-closed。占用解除后，同进程再次解析恢复正常。
- [ ] AC-3: 非忙错误不重试，也不消耗预算，立即按现有规则返回。非忙错误包括：目录代替库文件、非 SQLite 内容、解密失败、schema 版本高于本二进制。现有 `storage_config_*` 用例不改不弱化。
- [ ] AC-4: 全局配置库分支、环境变量直取路径与 GlobalSchemaFuture 的行为不变。`env_resolution_may_read_global_config` 在 cloud schema-future 检查中读取 D1 等键（`cli.rs:2653-2664`、`client_storage.rs:1516`），也经 Repository 分支，同样按本卡规则重试。
- [ ] AC-5: 文本与结构化输出都不新增任何 stderr 文字，等待期间不打印。

**Verification:**

- [ ] VER-1: `source .env.test && source .env.live-test && cargo nextest run --lib -E 'test(utils::client_storage::)' --no-fail-fast --retries 0`；完整模块零失败。
  - 新增两个父用例（`#[tokio::test]`、`#[serial(cwd, env)]`）：`storage_config_busy_repository_lock_is_waited_out`（AC-1）与 `storage_config_busy_repository_lock_past_budget_fails_closed`（AC-2）。
  - 父用例只在建库时持 cwd。持锁者是父进程里的专用 OS 线程及其独立 runtime：按路径打开仓库库，显式 `PRAGMA journal_mode = DELETE` 后 `BEGIN EXCLUSIVE`。
  - 解析在子进程里进行：子进程是本测试二进制的一个 `#[ignore]` 探针 `storage_config_busy_probe`，按 `STORAGE_CONFIG_BUSY_PROBE` 选择模式，未设置时直接返回。启动参数同 `spawn_unresolved_probe`：cwd 为该仓库，HOME 隔离，`LIBRA_CONFIG_GLOBAL_DB` 指向不存在的文件，清除 `LIBRA_STORAGE_*`。
  - 这样 cwd、环境与进程级预算都随子进程隔离，同模块其它用例看不到被锁的仓库，也不会消耗该预算。
  - 父子以标记文件协调，每个等待都有 30 秒上限。
  - waited_out：子进程写 `started` 后开始计时并解析；持锁线程见到 `started` 后过 600 ms 才 `ROLLBACK`。子进程断言解析成功、耗时 ≥ 500 ms，并 put/get 一个 blob。
  - past_budget：
    - 子进程写 `started` 后，用新 base path 调 `resolve_configured_storage`。断言拒绝、文字、未建目录，耗时 ≥ 生产常量 `STORAGE_CONFIG_BUSY_BUDGET` 且 < 预算 + 3 秒。
    - 再调 `objects_storage()`。断言拒绝、不进缓存，耗时 < 1.5 秒。
    - 写 `phase1`。持锁线程见到后 `ROLLBACK` 并写 `released`。
    - 子进程等到 `released` 后再调 `objects_storage()`。断言恢复，put 成功。
  - 父用例先调 `isolate_storage_env()` 再建库；持锁线程确认 `BEGIN EXCLUSIVE` 成功后才启动子进程；子进程输出经管道捕获（`Command::output`）；子进程退出后父进程立即通知持锁线程释放并等其结束。
  - 父用例断言子进程退出 0 且输出含 `1 passed`；waited_out 另断言子进程 stderr 为空（AC-5）；失败时打印子进程 stdout/stderr。
  - 另加两例（修复后才有意义，不属 RED）：`storage_config_busy_message_classifier` 只测忙判定函数；`storage_config_busy_non_busy_failure_returns_at_once` 在进程内把仓库库换成目录，断言拒绝在 1 秒内返回（AC-3，非忙错误不重试）。
- [ ] VER-2: RED 用的源是「新用例 + 一个尚无调用者的 `STORAGE_CONFIG_BUSY_BUDGET` 常量声明，不含重试逻辑」。在其上两个父用例实际失败：前者子进程约 200 ms 后被拒；后者首次解析耗时远低于预算。修复后通过；RED 原件留存仓外。
- [ ] VER-3: `source .env.test && source .env.live-test && cargo test --lib utils::client_storage::`（进程内并发运行）与 `cargo test --test compat_serial_registry`。新用例使用带键的 `#[serial(cwd, env)]`；守卫 `src_process_global_tests_hold_matching_lanes` 只检查 census 表内已有的行，`src_serial_registry_matches_src_classifier` 只要求登记表中的函数能被分类器找到，同文件现有带键的 `storage_config_*` 用例在两份 tsv 中均无行，所以两份 tsv 不改，由该守卫确认。
- [ ] VER-4: A1 末尾以 `cargo test --test command_test --no-run` 与 `cargo build` 构建修复后的二进制并记录两者 SHA256（同 `baseline/context.txt`）。修复后用新二进制，把 `maintenance_test::gc_ten_thousand_objects_within_budget` 与 `fetch_test::test_fetch_into_mirror_updates_and_prunes_all_refs` 各单例顺序运行 20 次，0 失败。对照为修复前的 gc 4/20、fetch 11/20。worktree linked fetch 修复前 0/20，不作对照。另跑 `cargo +nightly fmt --all --check` 与 `cargo clippy --all-targets --all-features -- -D warnings`。

**A 执行记录（GREEN 后的修正）：** 第一次 GREEN 只按 `err.to_string()` 判定查询错误。A1 通过（`run-green/`），但 A2 第 12 次 gc 仍失败（`ver4-first-green/`，执行器在 gc 1/20 失败后停止 A2，`run-ver4.STOPPED.txt`）。临时诊断（`diag2/`，日志代码已按保存的原件逐字节恢复）在 60 次 gc 中记录 3 次放弃，全部是 `busy=false`、预算余 3.7–5 秒、消息 `failed to query config_kv`；另有 17 次经重试恢复。据此加入 `green2`：查询错误按 anyhow 错误链判忙，并在 `storage_config_busy_message_classifier` 中断言被 context 包裹的忙错误判为忙、`no such table` 不判为忙。A1、A2 在 green2 后完整重跑：A1 PASS（520 秒；client_storage 模块 97/97，nextest 与进程内各一次；serial 守卫 26/26；strict clippy 唯一提示为用户具名排除项），A2 PASS（2017 秒；gc 0/20、fetch mirror 0/20 失败，对照修复前 4/20、11/20），原件 `run-green2/`、`run-ver4b/`、`ver4/`。ER-03 R1–R3 关于该处「带忙文字」的前提对这一点不成立，已在此更正。

**green3（R3 全量期间发现）：** R3 的 nextest 全量在 FIX-LTR-06+07+08 树上 PASS（10836/10836，0 FLAKY、0 重试，3852 秒，`fix-ltr06/run-r3-nextest/`）。随后 blaze 以 rustc 1.99.0 编译时报 `AtomicU64::fetch_update` 已弃用（改名 `try_update`；1.98.1 尚无 `try_update`）。CI 的 clippy 用未固定的 `stable`，Issue #650 显示贡献者使用 1.95 至 1.100，因此在 1.99 及以上，`-D warnings` 会失败。执行器在 blaze 跑任何测试之前停止了它（`fix-ltr06/run-r3-blaze.STOPPED.txt`），把记账改为带单次上限的 `fetch_add`（`stages/green3.diff`，行为不变），并在新树上重跑 A1 与完整全量（nextest 加 blaze）。

**ER-05 P2 处置（`review-fix08-ER05/output.json`）：** 第 1、2、3、7 项的文字修改已在 green3 后、下一次全量之前完成；第 4、5、6 项如下登记，不改测试：
1. `docs/development/commands/fetch.md` 与 `docs/development/commands/cache.md` 已补同义句（随 green3 进入下一次全量的被测树）。
2. Commit split 的核对基准改为 `21f8e4c`；DEP-QUAL-SOURCE 措辞改为「不改本计划工作区中的任何已改文件（plan-status.md 改动在不同行，同步无冲突）」。
3. 行号：本卡与计划注记中的 `:288`…`:917`、`:1305`、`:1555` 指插卡前的计划；插卡后应以条款文字定位。FIX-LTR-05 系列的注记实际为一处集中注记，不是逐处旁注；卡片按此改述（全量后同步计划）。
4. 查询分支判忙只有构造出的单元测试与 diag2 的真实观测（3 次放弃全部在该分支），未加真实 DbErr 单测：登记为残余 RISK-QUAL-FIX08-QUERY-BUSY-TEST（P2，open）；闭合观测点为 FIX-LTR-06 的 nextest 全量与 C/D。A2 0/20 不能区分 green1 与 green2（green1 残余率约 4/80），这一限制如实记录。
5. waited_out 的「≥ 500 ms」只有约 100 ms 余量，只会误失败、不会误通过；若在全量中 FLAKY，按新 flake 处理，不改测试。
6. 进程内运行（`cargo test`、blaze）时进程级预算由同进程用例共用，`storage_config_busy_non_busy_failure_returns_at_once` 只在 nextest 下有区分力；依 DEC-QUAL-FIX08-NEXTEST-RULE，进程内运行不是本卡的观测点。
7. 文档「at most 5 seconds / 最多 5 秒」的实际上限是预算加最后一次尝试（至少 200 ms）：已改为「about 5 seconds / 约 5 秒」（随 green3 进入下一次全量的被测树）。源码注释「cannot block on foreground writers」指每次尝试的 200 ms，保留不改（不在被测树之后改源码）。diag2 的诊断构建与 60 次 gc 不在执行器合约内，02:46:41Z 通知未逐项列出；代码已按哈希恢复，对被测树无影响，如实记录。

**After-fix failure disposition（依 DEC-QUAL-FIX08-NEXTEST-RULE 修订：只以 VER-4、FIX-LTR-06 A 的 nextest 全量与 C/D 为依据；进程内 `cargo test`/blaze 中出现的该文字失败归 DEFER-QUAL-INPROCESS-ISOLATION，R4 与 cur-2 两次进程内运行各有 3 例带该文字、合计 4 例（`rejected_reachability_production_client_rejects_fifo_without_blocking`、`unchanged_replay_verifies_ref_and_all_catalog_objects`、`client_storage_reads_pack_sha256`、`client_storage_reads_pack_sha1`），发布代码上 4 例同文字；依据比用户原文「nextest and C/D only」多列 VER-4，只会更严）：** VER-4 或 FIX-LTR-06 A 的 nextest 全量中若仍出现失败：
- 带存储配置拒绝文字：本卡未完成，回到本卡诊断（例如预算或判忙漏点），不进入 C。
- 其它文字：按 ER-13 T-6 处理，另立 FIX 或 DEFER，FIX-LTR-06 的 A 等待其处置。
- 3 个未证用例（merge restart、am --continue、diff_rename_limit）同样以 FIX-LTR-06 A 的重跑为观测点；`--retries 2` 下出现 FLAKY 也算复现。「merge restart 的 exit 128 = 存储拒绝」只是假设。

**Full-suite trigger:** T-1（`src/utils/client_storage.rs` 是跨命令共享存储核心）。
- 完整默认套件由 FIX-LTR-06 A 的合并树 nextest 全量覆盖（blaze 两次未能启动，R4 改跑的进程内 `cargo test --all` 依 DEC-QUAL-INPROCESS-DEFER 归 DEFER-QUAL-INPROCESS-ISOLATION），本卡改动包含在被测树中。中间提交的树不单独测试。
- 合入本卡后，FIX-LTR-07 AC-1 的「不依赖重试」不能再在该树上单独观察，FIX-LTR-07 卡加注。

**Dependencies:**
- `DEP-QUAL-SOURCE`：main 于 2026-10-29 由 d0a5c28 前进到 `21f8e4c`（PR #651，只增 `docs/development/plan/issues/650.md` 与 `plan-status.md` 两行，不碰本卡与 FIX-LTR-06/07 的任何文件）；本卡 RED/GREEN 以 21f8e4c 为基（同步原件 `sync-21f8e4c/`）；2026-10-29T05:5xZ main 再前进到 `ba38e30`（PR #653，只改 `issues/650.md` 与 `plan-status.md` 中 issue 650 的两行），本计划 `plan-status.md` 行的改动以补丁方式重放（`sync-ba38e30/plan-status.patch`），其余 17 个文件逐字节核对；green3 之后的 A1 与 R4 全量以 ba38e30 为基。此前的诊断、对照与 R2 全量在 d0a5c28 上，两者产品源相同。
- `DEP-QUAL-CARGO-WINDOW`：root 窗口已于 2026-10-28T14:48:41Z 归还、未重新授予。执行依据是用户指令 DEC-QUAL-FIX-FIRST 与 DEC-QUAL-FIX08-RELEASE，加上每次 Cargo/测试前发给 root 的通知（协调目录）。
- 新增依赖边 `FIX-LTR-08 -> FIX-LTR-06`：FIX-LTR-06 的 A 全量依赖本卡。

**Deliverables:** N/A（代码卡）。

**Implementation write set:**
- 代码：`src/utils/client_storage.rs`。改动包括：`read_config_env_value` 拆分及其上方注释；预算常量、退避常量与进程级原子量；忙判定函数；两个父用例、一个 `#[ignore]` 探针、一个专用持锁线程 helper 与子进程启动 helper。
- 文档：
  - `docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md`、`docs/commands/cache.md`、`docs/commands/zh-CN/cache.md` 各在「仓库库不可读即失败」一句后补一句：仅被其他写者占用时，进程内共计先重试约 5 秒；`docs/development/commands/fetch.md` 与 `cache.md` 的同类句同样补充。
  - `COMPATIBILITY.md:223` fetch 行同步补同义短语。
  - 网站两页（`../libra-backend/.../fetch.en.md`、`cache.en.md`）不含「仓库库不可读即失败」句（网站滞后于主文档），本卡不改网站：N/A。
- 治理：本计划、`docs/development/plan/plan-status.md` 本计划行。
- 两份 SRC_SERIAL tsv 不改（见 VER-3）。

**Commit split:** 与 FIX-LTR-06 文件重叠：`client_storage.rs` 中是不同函数；`fetch.md`、`cache.md`、`COMPATIBILITY.md` 在同一段内，按 3 行上下文生成的 hunk 会合并。拆分办法：
- 两份逐字节存档：
  - 实施前，已把工作区全部改动文件（FIX-LTR-06 + FIX-LTR-07 状态，即上一次 nextest 被测树，diff 哈希 dc65d2a7…）存于 `fix-ltr08/pre-impl-snapshot/tree/`，哈希见 `files.sha256`。
  - FIX-LTR-06 A 重跑认领时，同样把含 FIX-LTR-08 的被测树全部改动文件存于 `fix-ltr08/tested-tree-snapshot-r4/`、最终门 R5 存于 `fix-ltr08/tested-tree-snapshot-r5/`，记录逐文件 SHA256；`commit.py` 绑定 R5 快照。`libra diff` 把 `client_storage.rs` 显示为 `<LargeFile>`，该标记不绑定文件内容，所以树身份另以这份逐文件哈希为准。
- 整文件写回前，先 `libra fetch origin`，再核对 HEAD 与 `origin/main` 等于最终门运行（R5）claim 的 head，否则 STOP；上游前进后（无论改的是产品代码、测试还是文档），都在同步后的树上重跑全量门并更新快照与门文件，`commit.py` 只接受与最终门 claim head 相同的 HEAD（R5 即因 PR #654 而重跑）。
- 提交顺序：
  - FIX-LTR-06：只暂存其写集文件，内容取自实施前存档。`client_storage.rs`、`status.rs` 另与 GREEN 副本 `fix-ltr06/diag-replace/*.green` 核对。
  - FIX-LTR-07：暂存 `replace.rs` 与两份 tsv 的实施前存档版本。
  - FIX-LTR-08：写回被测树存档，暂存本卡写集。
  - 治理：本计划与 plan-status 中三张卡的登记共用同一段文字、无法按卡拆出中间文本，作为一个已核哈希的状态随 FIX-LTR-08 提交。
  - v0.30.92 版本面：FIX-LTR-06 的 release write set。
- 每个提交中的文件与对应存档逐字节一致；全部提交后，工作区与被测树存档一致（版本面与治理除外）。
- 网站仓 `cf` 分支：只有 FIX-LTR-06 的句子，本卡 N/A。

**Release write set:** N/A（依用户决定 DEC-QUAL-FIX08-RELEASE，随 FIX-LTR-06 的 v0.30.92 发布，不单独 bump）。

**Docs and compatibility impact:** 外部可见：仓库库被其他写者占用时，命令改为在进程内共计先重试约 5 秒，而不是立即失败。同卡更新上列中英文 fetch/cache 文档与 `COMPATIBILITY.md` fetch 行（网站 N/A，见写集）；无新错误码，`docs/error-codes.md` N/A。

**Rollback mode:** `immutable-release`

**Migration and rollback:** 无 schema 迁移；已发布版本只能前滚。

**Security and privacy:** 不新增输出；错误文字与只报设置名的规则不变。

**Performance budget:**
- 无争用时零额外开销：第一次尝试即返回。
- 争用时：
  - 一个进程内所有存储配置读取共计最多多等约 5 秒，之后每次读取恢复为现在的单次 200 ms 尝试。
  - `cached_objects_storage` 在 `ClientStorage::init` 期间持有进程级 std Mutex（`util.rs:1384-1391`）。预算用尽前，其它线程取存储也可能被挡住至多约 5 秒（现为 200 ms）。
  - 同步的 `objects_storage()` 常在 async 代码的 runtime worker 上执行（`client_storage.rs:3429` `rx.recv()`）。阻塞时长由约 200 ms 变为最多约 5 秒，期间该 worker 上的池连接归还可能被搁住（`db.rs:236-246`），同进程其它数据库用户随之等待。
  - 以上两条登记为本卡已知后果。
- 长驻进程（如 `libra service`）用尽预算后不补充，此后行为与现在相同。
- 聚焦验证分两段：
  - A1（编译、fmt、strict clippy、RED/GREEN、VER-1、VER-3）：3600 秒。
  - A2（VER-4 重复运行，已构建二进制，不编译）：2700 秒。修复前 fetch 通过一次约 83–94 秒（最长 93.69 秒，含 60 秒排空等待），修复后若不再等待排空会更短；以修复前上限加每个 libra 进程可能多等的 5 秒估算，约 2160 秒，在 2700 秒内。
  - 超预算记 STOP，不续窗。

**Estimated scope:** `S`（1 个生产文件）

**Version increment:** `N/A`

**Release boundary:** 依用户具名决定 `DEC-QUAL-FIX08-RELEASE`（user-decisions.json；用户作答 2026-10-29T01:35:43Z），仅本卡越过 G-07。G-07 在模板中不可豁免，比照 FIX-LTR-07 先例以用户具名决定处置。
- 单独的卡与单独的提交，并入 FIX-LTR-06 的 v0.30.92 发布，共用一次全量 C/D。
- v0.30.92 成员：FIX-LTR-06、FIX-LTR-07、FIX-LTR-08。FIX-LTR-06 卡的 Release boundary 与 C/D coverage 同步注明。
- C 或 D 失败时，按成员定位，前滚修复，不拆分已提交的成员。

**C/D coverage from:** `FIX-LTR-06`

**Granularity:** `type=implementation; axis=仓库库被占用时存储配置读取的进程级有界重试; recovery=immutable-release前滚; complete=yes; self-contained=yes; AC=5/8; VER=4/8; landing=1; prod-files=1; scope=S; deps=DEP-QUAL-SOURCE,DEP-QUAL-CARGO-WINDOW(returned; user-directed notices); writeset=overlap(FIX-LTR-06: same files, separate commits via hash-checked snapshots); release=G-07 user exception (rides FIX-LTR-06 v0.30.92); split-from=FIX-LTR-06 A full (DEC-QUAL-RESTART-180 run); exception=N/A; governance=DEC-QUAL-FIX08-RELEASE`

### Task FIX-LTR-06: 结构化输出下存储配置降级警告不写原始 stderr

**Task type:** `implementation`

**Lifecycle / Acceptance:** `in-progress` / 空（2026-10-29 现状：A 的 RED0/RED1 PASS、GREEN 聚焦门 PASS；全量先因 replace 用例、再因存储配置 fail-closed 一族失败，分别由 FIX-LTR-07、FIX-LTR-08 处置；A 全量在三卡合并树上重跑，见上方 2026-10-28/29 段。原文：`blocked` / 空（ER-03 已由本地 Claude R5/R6 PASS；root 已授予窗口；待 DOC-LTR-03 落地及本卡 A 执行器准入））

**Description:** 单一行为轴：`--json`/`--machine` 等结构化输出下，对象存储配置降级警告不得以纯文本写入 stderr，改为写入进程的待渲染警告缓冲（`record_warning_message`，与 `emit_warning` 及 GlobalSchemaFuture 降级的现行通道相同）；文本模式下的输出与是否触发 warning 追踪逐字不变。现实影响：任何在全局配置库不可读时以结构化输出失败的命令，其 stderr 当前不是单一 JSON。注意该缓冲只由 `status`（preflight）和 `agent import` 渲染，通用成功信封与错误信封都不渲染它，因此其余命令在结构化输出下看不到这条文字（RISK-QUAL-ENVELOPE-WARNINGS，与 GlobalSchemaFuture 现状相同）；同时它会计入 warning 追踪，结构化输出加 `--exit-code-on-warning` 时原本退出 0 的成功命令会改为退出 9（`LBR-WARN-001`）；文本模式维持历史行为，不计入追踪、仍退出 0（RISK-QUAL-TEXT-WARN-EXIT，P2）。`status` 的 JSON 文档承诺「退出 9 必对应 `data.warnings[]` 中至少一条」（`docs/commands/status.md:317-331`），而 status 在收集前快照缓冲（`src/command/status.rs:582-586`），对象存储却在收集中懒打开（`status/scan.rs:101` → `status.rs:1029` → `util::objects_storage()` → `ClientStorage::init` → `open_init_storage`）；因此本卡同时让 status **仅在结构化输出时**、于进入各模式分发（`src/command/status.rs:575` 之前）先解析一次对象存储并丢弃返回值（成功的客户端进入既有进程缓存供扫描复用；被拒绝的配置不进缓存，也不改变 status 的退出码与 error code，之后照旧由扫描中的对象读取报告），使降级警告在快照前记录，并以既有的 `repository_preflight`/`config` 条目出现在 `data.warnings[]`。文本模式仍走原来的懒打开路径，输出逐字不变。结构化输出下，这也使无提交仓库与缓存命中的 `--cached`/`--check-dirty` 开始报告该降级，并每次多做一次存储配置解析（不访问网络；s3/r2 配置且 `libra.repoid` 缺失、为空或为 `unknown-repo` 的边缘仓库可能在本仓 `libra.db` 写入 `libra.repoid` 配置键，见 `src/utils/client_storage.rs:3709-3721`）。全局配置库不可读时 status 仍在更早的配置默认值读取处 fail-closed（`status/input.rs:706-709`，128/`LBR-IO-001`），不受本卡影响。

**Out of scope:** 历史 TRY1 根因（永久 unknown，不由本卡证明）；F2 关闭（由 `DEP-QUAL-F2-GATE` 决定的关闭审查承担）；`src/utils/util.rs:3880`、`:3889` 两处 ignore 文件告警的原始 `eprintln!`（RISK-QUAL-IGNORE-WARN，P2，尚未排期）；`src/utils/client_storage.rs:1220-1223` `refuse_rejected_presence` 的纯文本加 `exit(1)` 与 `src/cli.rs:3397` init 转换重试警告（RISK-QUAL-RAW-STDERR-OTHER，P2，尚未排期）；通用成功信封与错误信封不渲染待渲染警告（RISK-QUAL-ENVELOPE-WARNINGS，P2，尚未排期）；不改变降级本身（全局库不可读时对象存储仍为历史本地后端）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 三处原始 `eprintln!` | `src/utils/client_storage.rs:1719`（`storage_config_resolution_fallback` 非 GlobalSchemaFuture 分支）、`:1898`（空 bucket）、`:1903`（`LIBRA_STORAGE_TYPE=local`） |
| 结构化输出通道 | `src/utils/error.rs:1346` `emit_warning`；`src/utils/output.rs:504` `record_warning_message`（同时调用 `record_warning`）；`src/utils/output.rs:540` `structured_output_active`，由 `src/cli.rs:3388` 在存储解析前设置 |
| 缓冲的渲染者 | 仅 `src/command/status/input.rs:216` 与 `src/command/agent/import.rs:4382` 读取 `pending_warning_messages` |
| 退出码 | `src/cli.rs:3954`：`--exit-code-on-warning` 且已记录警告时返回 `LBR-WARN-001`，退出 9（`docs/error-codes.md`） |
| 受控复现 | 仓外 `f2/probe-{garbage,chmod000}.stderr`：exit 129、stderr 非 JSON；`f2/probe-none` 为单一 JSON |
| 现有文本模式断言 | `src/utils/client_storage.rs:7901` `storage_config_init_fallback_local_word_stays_writable`、`:7917` `storage_config_init_fallback_local_empty_bucket_keeps_bucket_warning`（子进程，结构化标志默认关闭）；`:8076` `storage_config_hold_unreadable_global_stays_local` |
| 文档描述位置 | `docs/commands/fetch.md:75-81`、`docs/commands/cache.md:29-35`、`docs/commands/zh-CN/cache.md:16`、`COMPATIBILITY.md:223` fetch 行；`docs/commands/zh-CN/fetch.md:29`（含与英文对应的全局库降级句与 `local` 警告句） |

**Acceptance criteria:**

- [ ] AC-1: 结构化输出下，全局配置库存在但不可读时，失败命令的 stderr 只包含其原有的单一 JSON 信封；退出码、`error_code` 与降级到本地对象存储的行为不变（status 在此条件下保持既有的 128/`LBR-IO-001`）。
- [ ] AC-2: 结构化输出下，`LIBRA_STORAGE_TYPE=local` 与空 bucket 两条既有警告同样不写原始 stderr，消息进入待渲染警告缓冲。
- [ ] AC-3: 结构化输出加 `--exit-code-on-warning` 时，触发降级的成功命令退出 9，stderr 为单一 `LBR-WARN-001` JSON（status 除外，见 AC-7）；此变化与文本模式仍退出 0 的不对称写入文档。
- [ ] AC-4: 文本模式下三条警告的字节、顺序与是否触发 warning 追踪均与当前一致；上表三个现有用例不修改、不弱化。
- [ ] AC-5: 原 F2 用例保留全部断言；退出码或 JSON 解析失败时输出 exit、stdout、stderr。
- [ ] AC-6: 不新增配置值或凭据输出；GlobalSchemaFuture、仓库库读取失败与确认配置失败的 fail-closed 行为不变。
- [ ] AC-7: `LIBRA_STORAGE_TYPE=local`（含 `LIBRA_STORAGE_TYPE=local` 且 `LIBRA_STORAGE_BUCKET` 为空）时，`--json --exit-code-on-warning status` 在有提交的默认模式、无提交的默认模式与缓存新鲜的 `--cached` 模式下均退出 9、stderr 为空，`data.warnings[]` 含 code `repository_preflight`、source `config` 的降级消息；文本模式下无提交仓库的 status stderr 与修复前逐字相同。`--scan`、`--check-dirty` 走同一分发前预解析，不单列用例（RISK-QUAL-STATUS-MODES，P2）。

**Verification:**

- [ ] VER-1: `source .env.test && source .env.live-test && cargo nextest run --lib -E 'test(utils::client_storage::)' --no-fail-fast --retries 0`；完整模块非零，含上表三个现有用例；本卡不在该模块新增单测，不改全局 `STRUCTURED_OUTPUT`，不新增 serial 键，不改 `tests/SERIAL_REGISTRY.tsv` 或 `.config/nextest.toml`。
- [ ] VER-2: `source .env.test && source .env.live-test && cargo nextest run --test command_test -E 'test(command::reset_test::)' --no-fail-fast --retries 0`；含新增 `reset_json_error_stays_single_document_when_global_store_is_unreadable`（全局库写入非 SQLite 内容，不用 chmod，root 身份下同样有效）与 F2 诊断，完整模块非零。
- [ ] VER-3: `source .env.test && source .env.live-test && cargo nextest run --test command_test -E 'test(command::output_flags_test::)' --no-fail-fast --retries 0`；含新增 `json_exit_code_on_warning_reports_storage_degrade`（AC-1/AC-3，cat-file，全局库写入非 SQLite 内容）、`json_init_local_storage_type_warning_leaves_stderr_json`（AC-2：stderr 无原始 `Warning:` 行；加 `--exit-code-on-warning` 时退出 9、stderr 为单一 `LBR-WARN-001`）、`text_exit_code_on_warning_keeps_storage_degrade_exit_zero`（AC-4 的追踪不变）、`json_status_exit_code_on_warning_lists_storage_degrade`（有提交）、`json_status_unborn_exit_code_on_warning_lists_storage_degrade`（无提交）、`json_status_cached_exit_code_on_warning_lists_storage_degrade`（先以文本 `status --scan` 建立新鲜缓存，再断言 JSON `freshness` 为 `cached`、`warnings[]` 含该条目且不含 `dirty_cache_stale_fallback`）与 `text_status_unborn_stderr_unchanged`（AC-7 的文本不变量）。VER-3 共七个新增用例（连同 VER-2 的一个共八个），均为子进程、只给子进程设环境变量，不使用 `#[serial]`，不改 `tests/SERIAL_REGISTRY.tsv`。VER-1 不新增单测：结构化分支只能在 CLI 设置结构化标志后观察，由 VER-2/VER-3 的子进程用例覆盖。
- [ ] VER-4: `source .env.test && source .env.live-test && cargo nextest run --test compat_global_config_schema_future --no-fail-fast --retries 0`。
- [ ] VER-5: VER-2/VER-3 的新增用例（`text_exit_code_on_warning_keeps_storage_degrade_exit_zero` 与 `text_status_unborn_stderr_unchanged` 除外，它们在修复前后都应通过）先在未修复源上实际失败（RED），再在修复后通过；RED 输出留存仓外。三个 JSON status 用例的 RED 须分别在「只改 client_storage」与「未改」两种源上记录，证明 status 预解析是必要的。

**Full-suite trigger:** T-1：`src/utils/client_storage.rs` 是跨命令共享存储核心；A、C 均执行 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`。

**Dependencies:** `FIX-LTR-03`（v0.30.91 发布树）、`DEP-QUAL-SOURCE`（当前 main `8abdcf7`，相对 29eaa97 只有文档，产品源与 29eaa97 相同；该行原锚定 b73，按其「变化先重审」本卡以 8abdcf7 为源）、`DEP-QUAL-PUBLISH`、`DEP-QUAL-CARGO-WINDOW`。不依赖 `DEP-QUAL-F2-GATE`。v0.30.92 将是第一个带上 29eaa97 生产改动（index 修复、`git-internal` 0.10.4，签名 NO_PUBKEY FD937770…）的发布；本卡 A/C 的完整默认套件即覆盖该整树，不另立该提交的发布卡。

**Deliverables:** N/A（代码卡）。

**Implementation write set:** `src/utils/client_storage.rs`、`src/command/status.rs`（仅结构化输出时在模式分发前预解析对象存储，丢弃返回值）、`tests/command/reset_test.rs`、`tests/command/output_flags_test.rs`、`docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md`、`docs/commands/cache.md`、`docs/commands/zh-CN/cache.md`、`COMPATIBILITY.md`（仅 fetch 行）、本计划、`docs/development/plan/plan-status.md` 本计划行；网站 `../libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`、`cache.en.md`（Git 仓、`cf` 分支；现有他人未跟踪文件 `memory.en.md` 保留不动）。`docs/commands/status.md`、`docs/commands/zh-CN/status.md`（`repository_preflight` 行、「命令运行前」措辞与「配置解析本身从不告警」一句（英文约 314–315 行、中文约 160 行）改为包含结构化输出下 status 扫描前的对象存储降级；英文约 317–322 行（中文约 162 行）「与 status 警告相同的投递矩阵」和英文约 328–331 行（中文约 164 行）「a warning exits 9 … in every output mode」须注明对象存储降级在文本模式下例外：输出原始 `Warning:` 行、仅当本次 status 懒打开对象存储时输出（如有提交的完整扫描、upstream ahead/behind 计数）、不触发退出 9（RISK-QUAL-TEXT-WARN-EXIT）；`src/command/status.rs:228` 同义注释同改）。`docs/commands/config.md` 及其中文版目前不描述这两条警告，N/A；网站 `status.en.md` 不列 warning code，N/A。

**Release write set:** Inherited（`Cargo.toml`、`install.sh`、`install.ps1`、工具链刷新的 `Cargo.lock`、release artifact；以 `compat_version_surface_sync` 为准）。

**Files likely touched:** 同 I/R。

**Docs and compatibility impact:** 结构化输出的 stderr 内容与 `--exit-code-on-warning` 退出码变化属于外部可见行为：同卡更新上列中英文 fetch/cache/status 文档（fetch/cache 中英文均写明结构化输出的去向、`--exit-code-on-warning` 退出 9 以及文本模式仍退出 0 的不对称）、`COMPATIBILITY.md` fetch 行与网站 fetch/cache 两页；无新错误码（沿用 `LBR-WARN-001`），`docs/error-codes.md` N/A。

**Rollback mode:** `immutable-release`

**Migration and rollback:** 无 schema 迁移；已发布版本不可撤回，回归以新 patch 前滚。需要旧行为（原始 stderr 文字）的用户可改用文本输出或固定使用 v0.30.91；使用 `--json`/`--machine` 加 `--exit-code-on-warning` 的脚本在存储降级时会从退出 0 变为退出 9。兼容窗口：本 patch 起生效，文档写明，不保留开关。

**Security and privacy:** 警告文本沿用现有内容，不新增配置值；结构化模式下不再把全局库绝对路径写进原始 stderr。

**Performance budget:** 文本模式无变化；结构化输出的每次 status 多一次存储配置解析（读本仓与全局配置库，s3/r2 配置时最多约 10 次查找，可能写入 `libra.repoid`，不访问网络），`--cached` 快路径同样承担。A、C 各候选一次 10800 秒（含编译、listing、全部门、default/keyring release 构建、安装和资源后检），基于本计划旧机器暖缓存估计；本机 claim 前须以实测工具链与缓存重核并经本地 Claude 准入，不在窗口外预编译，不续窗。

**Estimated scope:** `S`（2 个生产文件）

**Version increment:** `patch`（候选 v0.30.92，执行时现场核 tag/Release 未占用）

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=结构化输出下存储配置降级警告的输出通道; recovery=immutable-release前滚patch; complete=yes; self-contained=yes; AC=7/8; VER=5/8; landing=2; prod-files=2; scope=S; deps=FIX-LTR-03,DEP-QUAL-SOURCE,DEP-QUAL-PUBLISH,DEP-QUAL-CARGO-WINDOW; writeset=no-overlap; release=independent; split-from=DEP-QUAL-F2-ATTRIB; exception=N/A`

### Task DOC-LTR-03: 来源资格交接复核登记

**Task type:** `docs`

**Lifecycle / Acceptance:** `in-progress` / 空

**Description:** 把 2026-10-27/28 复核的事实、缺口、owner、用户决定、FIX-LTR-06 候选与待决事项写入本计划、plan-status 本计划行和 plan-20261015 的 `DEP-TR-QUAL-REPAIRS` 证据列；保留全部旧 FAIL/STOP 与历史文字。

**Out of scope:** 不实现 FIX-LTR-06；不改模板、共享协议、plan-long（本计划无长期能力编号，N/A）；不消费 root 独立治理副本；不关闭任何依赖。

**Acceptance criteria:**

- [ ] AC-1: 五组结论、未解决项、用户决定与 owner 逐项登记，未知与未运行明确标出，新写事实均有仓外原件或注明为未留原件的观察。
- [ ] AC-2: 只新增文字；plan-status 本计划行与 `DEP-TR-QUAL-REPAIRS` 证据列只加前缀，原行首标签保留。
- [ ] AC-3: 提交为 signed + DCO，且以本仓 vault 公钥实际验签。
- [ ] AC-4: 推送后该 SHA 的 base 八个 job 与 CodeQL 两个 job 实际终态逐项核验。

**Verification:**

- [ ] VER-1: 原生 diff 与逐文件哈希确认改动范围。
- [ ] VER-2: 本地 Claude 审查当前差异与证据清单，明确 `VERDICT: PASS`。
- [ ] VER-3: `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --test compat_agent_architecture_guard --no-fail-fast --retries 0`（须先取得 Cargo 窗口）。

**Full-suite trigger:** `N/A`（no-release 计划文本）。

**Dependencies:** `DEP-QUAL-SOURCE`、`DEP-QUAL-CARGO-WINDOW`（仅 VER-3）。

**Deliverables:** 本计划本段与相关表行、plan-status 本计划行、plan-20261015 `DEP-TR-QUAL-REPAIRS` 证据列。

**Implementation write set:** `docs/development/plan/plan-20261025-qualification.md`、`docs/development/plan/plan-status.md`（本计划行）、`docs/development/plan/plan-20261015.md`（`DEP-TR-QUAL-REPAIRS` 行）。

**Release write set:** `N/A`

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `N/A`

**Release boundary:** `no-release`

**C/D coverage from:** `FIX-LTR-06`（产品 C 覆盖；本卡自身文档门与 main D 另记）

**Granularity:** `type=docs; axis=来源资格交接复核登记; recovery=immutable-release前滚文档; complete=yes; self-contained=yes; AC=4/20; VER=3/20; landing=0; prod-files=0; scope=S; deps=DEP-QUAL-SOURCE,DEP-QUAL-CARGO-WINDOW; writeset=本计划三处; release=no-release; split-from=用户20261027交接; exception=N/A`

## 2026-10-26 跨机器接续入口（当前要求）

用户本轮明确要求：「请把 Libra 需要的前置任务写入 docs/development/plan/plan-20261025-qualification.md 这个文件中，把要求的任务写明细，然后单独提交，我会在其他的机器上启动一个 agent 完成」；随后指定「libra 的交接任务卡，请在 main 分支提交，我也会在 main 分支开发」。本次交接只提交本文件到 `main`，不携带程序候选、版本修改或原 scoped-traces 三份治理 WIP。接收 Agent 在本机定位真实目录，用原生 Libra 拉取并核验最新 main 后接续；不得复制旧机器绝对路径。

本文的任务定义和交接审查不等于实现准入、验收通过或新增发布豁免。用户已改为仅调用本地 `claude` 命令审查，不调用 API，不使用隔离 Codex。后文带旧日期的双审、feature branch/PR、旧版本、预算和 pending 字样按原时点保留；当前开发分支遵守本次用户 `main` 指令，质量门、签名、无 force 和实际 D 不变。远端保护若要求 PR/他人批准，记录真实阻塞并按保护规则处理，不能 admin bypass。

### 当前事实、范围与任务表

文档准备时已只读核验远端 `main=b73e98a77ecc7c3db1ea905ab55478785d76c041`，版本 `0.30.87`。这是核对时点，不是要求接收方退回的固定基线；本交接提交及其后新增提交均须保留。FIX01 的 PR635 合并为 `f33be958dcc3266df3a6ab796ed90d1766f3cd93`，v0.30.87 完整发布已核；PR636 合并为 b73，104 项项目 Rustdoc warnings 已修。`proc-macro-error2 2.0.1` 的 future-incompatibility 提示按用户要求排除，不修改第三方库。

| 前置事项 | 当前状态 | 接收 Agent 必须交付的结果 |
|---|---|---|
| FIX-LTR-01 | done/complete | 按当前来源限域消费已有 schema doctor 修复，保留原签名、C/D 与失败历史；不重做已完成卡 |
| DEP-QUAL-F2-ATTRIB / DEP-QUAL-F2-CLOSE | 归属待审 / 关闭未解决，暂定 P1 | 先作可为unknown的只读归属决定，再另核关闭证据；查明 `command::reset_test::reset_pathspec_from_file_missing_path_errors` 的真实 stderr/退出；不能根据别的用例推断为 SQLite 锁定 |
| FIX-LTR-05P | family child，已在 main `8a115ba` | 存在性探测第一切片已推送；实现审查 dc579753 PASS；不消耗 v0.30.88 |
| FIX-LTR-05Q | family child，已在 main `c0373cb` | show、maintenance、command restore、Blob::save 已推送；准入 3de3e11b，实现审查 cf7454d4 PASS；不消耗 v0.30.88 |
| FIX-LTR-05R | family child，已在 main `80ef760` | snapshot、operation store、operation restore、repack 已推送；准入 54f3e8cf，实现审查 146781d3 PASS；不消耗 v0.30.88 |
| FIX-LTR-05S | family child，已在 main `6177646` | client_storage 的索引补登记与修订后缀类型过滤已推送；准入 71cf457d，实现审查 d6849287 PASS；不消耗 v0.30.88 |
| FIX-LTR-05T | family child，已在 main `e728736` | am 的本地对象搜索已推送；准入 986c352a，实现审查 02aa713c PASS；不消耗 v0.30.88 |
| FIX-LTR-05U | family child，已在 main `1d3c8d2` | tiered 与 remote 的 search_checked 已推送；准入 c1d476b0，实现审查 bc600c5b PASS；不消耗 v0.30.88 |
| FIX-LTR-05V | family child，已在 main `835cfc5` | fetch AutoFollow 批量探测已推送；准入 95af8652，实现审查 72196c7f PASS；不消耗 v0.30.88 |
| FIX-LTR-05W | family child，已在 main `2a80a10` | `resolve_configured_storage` 与 fetch 写入前检查；ER03 `c6cf48c1` PASS；实现审查 `2ee621eb` PASS；不改 `init`；不消耗 v0.30.88 |
| FIX-LTR-05X | family child，实现审查 PASS | ER03 `8059d2f4` PASS；实现审查 `bd0e229d` PASS；已在 main `a729384`；不消耗 v0.30.88 |
| FIX-LTR-05Y | family child，已在 main `ec98926` | ER03 `db7a5d7f` PASS；实现审查 `a2a2af14` PASS；不消耗 v0.30.88 |
| FIX-LTR-05Z | family child，实现审查 PASS | ER03 `272c8d7d` PASS；实现审查 `4a42ec63` PASS；不消耗 v0.30.88；不开始 FIX04 |
| FIX-LTR-05 | done，v0.30.88 D 已观察 | 父卡 VER-1 至 VER-5 与失败关闭的缓存、fetch pack 已在 ae50414 满足。审查 d9641b0d PASS。发布 e85470a。D 于 2026-10-26T18:23:07Z 成功：base 40349034162 attempt 2、CodeQL 40349034316、release 40349204443。F2 仍 unknown。RISK-QUAL-ADD-LOOSE 未修 |
| FIX-LTR-04 | done，v0.30.89 D 已观察，F2 仍 unknown | 2026-10-26T19:12:37Z 修订：历史 TRY1 stderr 不可恢复，CLOSE 保持 unknown，P1 不关闭。实现审查 354b5170 PASS。发布 ff30d4a。D 于 2026-10-26T20:44:43Z 成功：base 40391580491、CodeQL 40391580303、release 40391698253。本卡只加了两行 cfg |
| FIX-LTR-02 | done，v0.30.90 D 已观察 | 发布 `98852b7`。D 成功：base `40406196457`、CodeQL `40406196519`、release `40406331881`。fixture `b5c92ec6-90ec-4a3e-a1c4-2faa9b2ae259`。F2 仍 unknown |
| FIX-LTR-03 | done，v0.30.91 D 已观察 | 发布 `e24c88b`。D 成功：base `40426098305`、CodeQL `40426098171`、release `40426201816`。worker 回收先核身份再结束父进程。F2 仍 unknown。四个具名 P2 未修 |
| F3 / RISK-QUAL-STDIN | 具名 P2，未修 | `subagent_content` fixture 的 BrokenPipe/时序风险单独处理；不能混入 FIX05；若升级为 P1 或阻断当前门，先另立 FIX 卡和依赖 |
| RISK-QUAL-ADD-LOOSE | 具名 P2，未修 | 操作日志在命令前用 `ClientStorage::init_local` 写快照，忽略 `LIBRA_STORAGE_TYPE`。`LIBRA_STORAGE_TYPE=gcs-canary` 时 `libra add` 以 LBR-IO-002 失败且不写 index，仍留下 loose 对象（含 blob `45b983be`）。不宣称失败配置下所有命令零对象写入。不混入本卡发布，也不开始 FIX04 |
| DOC-LTR-01 | locally-accepted | PR636 的文档修复与实际 main D 已发生，仍按原声明由 FIX03 覆盖；不因本交接晋升 |
| DOC-LTR-02 | 本次任务定义交接 | 仅本文件、main、signed/DCO、实际本地 Claude；无产品版本发布，覆盖由 FIX03 承载 |

历史时点：新增 FIX05 后，本外部计划实现卡曾记为 **1/5**；DOC01、DOC02 单独记账。当前完成数见文末「当前（2026-10-26）」段，不以这句作为现状。原 Libra scoped-traces 计划仍 **0/26**，不增加其分母；Mega2 已完成 MTR103，**23/118**，联合 **23/144**。卡片数不表示工时比例。两仓原计划、plan-status 的本轮单文件交接后置同步由 root 在既有协调写集中完成；用户本次明确只提交此文件，不能为满足索引同步而混入原工作区整份 plan-status。

### 实际失败与可移植证据

FIX04 C1 的 11 个命令机械退出 0、10761 个最终通过、21 个原 ignored，不能替代 review。该轮存在 4 个 flaky 身份、5 次失败尝试；Claude session `c7f9fbae-734f-416c-9d9d-3650ede3f824` 的四项结论均 FAIL，root 已记录 STOP。原失败版本四面已精确退出 0.30.87；未提交该失败树、未发布 0.30.88。旧 A1 的两项 flake、37 项 runtime 覆盖缺口、C1 的 F1/F2、旧 FIX3 的诊断 3/3、A/B 8/8 与 B8 失败全部保留，迁机不重置。

| 证据 | 可核事实 / SHA256 |
|---|---|
| F1 源文件 | b73 的 `src/utils/client_storage.rs` SHA256 `c5a60476681ab325cd70b41634b1fc8df29536a8f94bcc13e2b311a34a576b4d`；`create_storage_backend` 将 typed resolver 的 `Other` 错误送入 `storage_config_resolution_fallback`；后者无条件 stderr Warning 并返回 LocalStorage；repository config open 的锁超时为 200ms |
| F1 原失败观察 | fetch 的 machine-error 用例实际收到 `SqliteError code 5: database is locked` 的额外 Warning，破坏 JSON 错误输出；尚未证明历史锁 owner/SQL，也不能称所有 SQLite 错误均来自此处 |
| F2 源文件与缺口 | b73 `tests/command/reset_test.rs` SHA256 `72dbdbc3ff385198b21814cf26512fd757ddc256bc5fc99fa8213dc02f026cd3`；原 TRY1 JSON parse 失败、TRY2 PASS，原失败未输出 subprocess stderr；历史根因未知，不能靠新一次 PASS 关闭 |
| 原 C1 review | output SHA256 `b110df54dbe47f75636ef98fbbabc7a1f4e4e74c1585ae207844a4930b13cafd`；结论 FAIL，不追写成 PASS |
| 发布次序审查 | 本地 Claude `2891a30b-0b77-417a-b18e-c18591bc9236`：`SOURCE_BASELINE_VERDICT: PASS`、`RELEASE_ORDER_VERDICT: FAIL`、`VERDICT: FAIL`；manifest `724b5d4f137838722582d2689122b9fad98e6c340475ce7a411c865ced19a1bd`，output `72719067e04e448dcf9bd5c3ad47281e27f44e1cd6fb9cf1c9c59f215cbe96fe` |
| FIX04 保存候选 | ingress 基线 SHA256 `6ef6f6aa2d4dbf2374f9764039f83a981c8140838d4e4903eaa220500e48cd2c`；仅在 `from_payload` 与 `from_payload_with_hint_policy` 定义前加 `#[cfg(any(test, debug_assertions))]`，候选 SHA256 `2b7aa33698c2072d160cc6bea7801ffb8fb180a13bb0ebe78901ff888b7bb778`；不能将其提前放入 FIX05 |
| FIX04 迁机前保存像 | 原native stash `c763911e0c73699ad5f739309c203da3945a7405`包含两cfg候选、本计划旧稿SHA256 `5316f6478095374fdb33150504386105bae85de209fa2e3955bc9ea5439e8fa0`及status旧稿 `8d2d1193cea451230e84be04971ad5ea321f083eebdd0c603e730acabb3621cc`；只在当前交付机器，未推送且接收方不能假定可用。恢复只能精确取当前卡路径，不能stash-pop整份治理。 |
| FIX02 保存候选 | 原 native stash `988ded20adb6be7ebbc43ad195fd55768ed69a26` 仅在旧机器；不能假定远端含该对象。按 FIX02 卡的函数/断言重建时先核当前来源并重新审查，不把新文件当旧已测候选 |

这些摘要可以随仓库迁移，但不是原始输出本身。接收方如需 carry-forward，必须取得并核验原件/manifest/脱敏衍生件关系；只有摘要或旧 `/tmp` 路径时记录「原件不可得」，不能重造历史 PASS。原 SHA256 `217cea13519e3b9e8e23e818e669f8f73c5b5391921018f460a68a32a186484b` 的更早交接包已丢失，不是本次可获取产物。本机旧绝对路径只作历史定位线索；当前执行合同、工具链、数据库、服务、凭据与预算须在接收机器重新核验。

### 发布顺序与待决事项

合法恢复的**候选**顺序是：

`FIX01(done) → FIX05 → FIX04 → FIX02 → FIX03 → 计划最终资格 → 终态登记及受控整合 → 原 LTR10 的资格前置闭合`。

FIX05 前需要只读的 `DEP-QUAL-F2-ATTRIB` 决定与当前本地Claude ER03；ATTRIB可以明确为unknown，不代表P1关闭。真正的 `DEP-QUAL-F2-CLOSE` 在FIX05或另一个具名FIX实际C/D及关闭审查之后，且在FIX04之前；FIX05不依赖CLOSE。若F2不同根因，先登记新的FIX卡、写集与独立发布顺序；若unknown仍保留CLOSE阻塞FIX04和最终资格，不能靠FIX05全量一次PASS关闭。2026-10-26T19:12:37Z 的修订是这个规则的后续例外：CLOSE 仍是 unknown，P1 不关闭，但 FIX04 可以只加两行 cfg。该例外不把 F2 写成已修复。FIX05自己的实际新失败/flake仍按ER10 STOP，不能以F2另记为由放行。新F2卡不受FIX05唯一warning例外覆盖，必须另取得其具体发布决定。不得将不同缺陷合并成 G08 家族、移植 Mega G13，或省略各卡 patch/C/D。

**DEC-QUAL-05-RELEASE-WARNING（pending，非已批准 EX）：** 既定 release 零项目 warning 门使两种顺序均不合规：FIX05 先发布会留下 ingress 两方法的既存 dead_code；FIX04 先发布又留下未关闭的 F1/F2 P1。最小待决事项仅是：是否允许 **FIX05 这一次独立版本发布**在 default/keyring release 构建保留 b73 的 `from_payload`、`from_payload_with_hint_policy` 这一个具名 dead_code 诊断，紧接着由 FIX04 的下一 patch 消除。它不是模板 G-* waiver、不是第三方 proc-macro 提示，也不允许其它 warning、全量失败、Clippy warning 或未关闭 P1。用户本次要求写卡、main 提交和跨机开发，**未等同批准此例外**。接收 Agent 先完成可独立的只读核验及候选设计，再向用户取得具体决定、记录原话/UTC与范围，并获得真实本地 Claude 的发布次序 PASS；在此之前 FIX05 不进入实现/A/C/发布。若决定维持零警告，保持阻塞并提出新可审方案，不擅自扩大例外。

**现行 DEC-QUAL-05-RELEASE-WARNING（用户原话，2026-10-26T05:40:46Z）：** DEC-QUAL-05-RELEASE-WARNING：我批准仅 v0.30.88 这一次 FIX05 发布可以保留 CaptureIngressCommand::from_payload 与 from_payload_with_hint_policy 的 dead_code 诊断，保留到下一个 patch 标签为止，之后的版本不得保留。范围只有这两个方法。UTC 2026-10-26T05:40:46Z。

- UTC：`2026-10-26T05:40:46Z`。
- 范围：只有 `CaptureIngressCommand::from_payload` 与 `CaptureIngressCommand::from_payload_with_hint_policy`。
- 期限：保留到下一个 patch 标签为止；之后的版本不得保留这两条诊断。
- v0.30.88 这一次 FIX05 发布可否带上这两条 dead_code 诊断：可以。只此一次，只这两个方法。
- 本地 Claude session `4042079a-78eb-4af9-b86d-34e0d05ffe9c`：`RELEASE_ORDER_VERDICT: PASS`，`VERDICT: PASS`，`SCHEME: GRANT-ONE-VERSION`，`MAY_SHIP_DEAD_CODE: yes`，`DURATION: until-next-patch-tag`，`FIX05_VERSION: v0.30.88`。
- 该决定不关闭 F2，不把 cfg 放进 FIX05 提交，也不代替 FIX-LTR-05 配置失败后端的 ER03。2891a30b 的发布次序 FAIL 仍是当时的 FAIL。上文 pending 段保留为决定之前的入口。

### 接收 Agent 的第一轮动作

1. 定位原生 Libra 仓库，核 main、工作区、index、远端、提交祖先与实际写者；保留 COV/OG、SCAP 等上游成果以及未知改动。原工作区 `7541e9ba...` ahead 10 的 scoped-traces 历史与本计划修复来源分开，禁止将整条旧本地 main 推到远端。独立 clone 的 `.libra` 必须是实目录，不能采用共享 HEAD/index 的 worktree。
2. 读取当前 AGENTS、CLAUDE、plan-template 和本入口，核原生 CLI 版本、vault 签名、DCO、gh 权限及 workflow。只把 `gh auth status` 的有效 repo/workflow 权限作为能力证据，不能再误判为不能使用 `gh release`；实际动作仍核返回值与远端 ref。
3. 完成 F2-ATTRIB 的只读归属决定（允许明确unknown但不关闭P1），审计 F1 的 `ClientStorage::init` 调用者、所有 `Storage` 方法和错误映射，再冻结 FIX05 实际 I/AC/VER/规模。下面的窄实现路线不是对未实现方案的 PASS；若超出卡范围，先 ER03 拆分。
4. 取得 DEC-QUAL-05-RELEASE-WARNING 决定和本地 Claude 准入，再实现 FIX05。两份 env 文件缺失、所需 live 凭据缺失或新机器预算无依据时，先完成无依赖的准备，再报告具体缺项，不能 skip 对应必需门。
5. 每卡把源码、验收、Claude、提交、main push、版本 tag/Release、CI/D分别记账。每次 main push 前重新核远端；有新增提交先原生前滚整合并重验受影响门，不 force。用户在另一机器 main 开发不代表可以并发发布；接收方是 Libra 当前唯一实现/发布者，跨仓 root 继续 Mega2，共享协议修改须先协调。

### F2：先归属、后关闭，诊断由已准入FIX05承载

`DEP-QUAL-F2-ATTRIB`只读核原case `command::reset_test::reset_pathspec_from_file_missing_path_errors`、原TRY1退出断言已过而JSON解析失败/TRY2通过的原件缺口及当前控制流；本地Claude须给出「同一机制 / 独立缺陷 / unknown」具名决定。unknown允许完成归属登记，但不是根因证明或P1关闭。该只读阶段不得改跟踪文件或启动Cargo；不新增独立1800秒窗口。

只有FIX05本身获准后，才由FIX05在已登记 `tests/command/reset_test.rs` 中补失败时的合成stdout/stderr/exit诊断，保留原命令、参数、JSON/code/exit断言，禁止剥离Warning、过滤前缀或弱化断言。诊断属于FIX05自己的VER-4完整reset模块，单次 `source .env.test && source .env.live-test && cargo nextest run --test command_test -E 'test(command::reset_test::)' --no-fail-fast --retries 0`，编译与执行计入其已准入A预算；其全量A/C继续保留真实并发，不以单case通过替代。诊断输入、资源归属及执行器须在该卡ER03中审定，无前置跟踪写集空档。

`DEP-QUAL-F2-CLOSE`须真实证据及本地Claude明确关闭：同机制时依赖FIX05完整C/D；独立缺陷依赖另立FIX的完整C/D；未知时保持P1。依 2026-10-26T19:12:37Z 例外，FIX04 对 CLOSE=unknown 豁免，只加两行 cfg，P1 仍不关闭。该例外不是用户批准。新的受控实验只能证明当前机制，不能补造已经缺失的历史stderr，也不能把未复现/原native retry成功写成旧C1接受。归属与关闭是两个不同节点，故不存在FIX05等待自己的环。

### Task FIX-LTR-05: 存储配置读取失败时停止错误的本地降级

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / v0.30.88 D 已观察（DEC-QUAL-05 与 session 4042079a 仍有效。F2-ATTRIB 仍是 unknown。05W 至 05Z 与 ae50414 已在 main。父卡 VER-1 至 VER-5 在 ae50414 通过：93、62、51、58、54，0 failed。完整套件 df24361e：10822 passed，0 failed；`test_fetch_into_mirror_updates_and_prunes_all_refs` 重试后通过。审查 d9641b0d VERDICT PASS，RELEASE_BUMP_ALLOWED yes。本提交把三个版本面改为 0.30.88。tag v0.30.88、Release 与 D 已于 2026-10-26T18:23:07Z 观察成功：base 40349034162 attempt 2、CodeQL 40349034316、release 40349204443。RISK-QUAL-ADD-LOOSE 是未修 P2，不宣称失败配置下所有命令都零对象写入）

**Description:** 单一行为轴是对象存储配置解析失败的 fail-closed 语义。普通解析/数据库/解密错误不能自动创建可继续写入的 LocalStorage，也不能用额外 stderr Warning 破坏 machine/JSON 输出。保留明确 local-only、合法 remote 配置、配置角色隔离及 COV/OG 已有 GlobalSchemaFuture 处理策略。优先审计保持 `ClientStorage::init -> Self` 签名的私有失败后端方案，避免把现有约99处调用点改成新的 fallible API；不是只移除 eprintln。候选后端必须审计并覆写所有可能返回成功/执行副作用的方法，尤其 `exist_checked`、`object_size`、`heal`、`evict_local`、`delete_payload` 的默认实现及其batch路径；legacy bool/search 方法不能将未知持久化状态当成功或删除依据。实际调用链若无法在本卡写集内保证正确性，先拆卡，不以返回 false 或空集合掩盖错误。

**Out of scope:** ingress cfg 属 FIX04；reasoning fixture 属 FIX02；MCP worker teardown 属 FIX03；F3/BrokenPipe 不混入。F2 不同根因时另卡。无 schema 迁移、无全局 `ClientStorage::init` 签名重构、无新增依赖、无延长 SQLite busy timeout、无额外重试，也不把配置读取错误假装成配置不存在。

**Current evidence:** 上表F1及实际fetch失败可核；历史锁owner/SQL仍未知。R1本地Claude已识别以下真实源码限制，故“只加一个失败后端”的设计未获准：①env未设置时resolver仍读repository DB，锁住时无法区分local-only与remote，直接fail-closed可能把普通local操作变成另一错误码；②`src/utils/util.rs::cached_objects_storage`按路径永久缓存ClientStorage，一次busy不能永久毒化长进程；③`src/command/fetch.rs`先write_pack_and_index，再objects_storage，单后端不能保证pack/index零副作用；④Storage的exist_checked默认折叠bool，object_size默认Ok(None)，delete_payload默认Ok(())且obliteration driver可能先unlink。`GlobalSchemaFuture`另有既定策略，不能顺带改变；create_storage_backend中的builder panic也须在同一配置失败轴内移除，不能在被改函数保留生产panic。

**设计准入待办（不是已通过的粒度结论）：** 先完成VER-6，只读确定实际缓存恢复、配置busy政策与写入前检查的落点。下列I是3生产文件候选范围，不表示实现已自洽。原local-only正常读取必须继续成功；配置busy时若需要改变既有local命令错误码/可用性，必须先提交明确兼容方案与具名用户决定，更新本卡AC/双语文档/测试，不能直接改旧预期让新错误通过。所有普通配置/构造失败应有安全错误路径；不能把未知状态写成“没有remote”。若无法在本轴下完成、需要改Storage trait/其他删除调用者或突破3文件候选，先更新I/G-*并真实ER03，必要时拆卡。

**Acceptance criteria:**

- [x] AC-1: 普通配置解析错误不产生可操作的 LocalStorage 后端。
- [x] AC-2: 经ClientStorage/Storage的写入和状态变更拒绝失败配置；fetch在pack/index安装前完成同一配置检查，无错误成功及本卡调用路径的对象/index/ref副作用。全局绕过Storage的删除路径先VER-6核清，不把此条伪称覆盖所有命令。
- [x] AC-3: machine/JSON 失败输出保持原结构与退出约定，不夹入未结构化 Warning。
- [x] AC-4: 错误诊断不泄漏配置值、凭据或解密原文。
- [x] AC-5: local-only配置可正常读取时保持原可用行为；DB busy无法判定local/remote时不得静默降级，行为/错误码兼容方案先审定，未经具名决定不改变既有local命令契约。
- [x] AC-6: 合法 remote 配置保持原存储选择与持久化行为。
- [x] AC-7: COV/OG 的全局 future-schema/配置角色策略保持原契约。
- [x] AC-8: 配置瞬时失败不被路径缓存永久固定；配置恢复后同一长进程按已审定策略恢复，无自动无限重试。

**Verification:**

- [x] VER-1: `source .env.test && source .env.live-test && cargo nextest run --lib -E 'test(utils::client_storage::)' --no-fail-fast --retries 0`；完整模块非零，新增 `storage_config_failure_` 用例覆盖受控真实 SQLite 锁、失败后端全部方法、无落盘副作用、canary不泄漏、local/remote有效路径。使用同步屏障证明锁持有，保持原200ms，不用延长timeout掩盖故障。ae50414 上 93 passed，0 failed（run 7313ab4a）。登记里的同步屏障真锁用例没有落地，现有覆盖是把仓库库换成目录；这是 P2 缩窄，收据不写「真实锁已覆盖」。
- [x] VER-2: `source .env.test && source .env.live-test && cargo nextest run --test command_test -E 'test(command::fetch_test::)' --no-fail-fast --retries 0`；完整fetch模块非零，原machine/JSON、code、exit断言保持。ae50414 上 62 passed，0 failed（run f7d066a1）。
- [x] VER-3: `source .env.test && source .env.live-test && cargo nextest run --test command_test -E 'test(command::output_flags_test::)' --no-fail-fast --retries 0`；完整输出模式模块，不能靠过滤stderr使其通过。ae50414 上 51 passed，0 failed（run e8bb5e44）。
- [x] VER-4: `source .env.test && source .env.live-test && cargo nextest run --test command_test -E 'test(command::reset_test::)' --no-fail-fast --retries 0`；完整reset模块；F2诊断、归属和已有断言分别记录，未证明根因不记修复。ae50414 上 58 passed，0 failed（run a3d3dc5c）。这不关闭 F2。
- [x] VER-5: `source .env.test && source .env.live-test && cargo nextest run --test compat_global_config_schema_future --no-fail-fast --retries 0`；完整COV/OG兼容目标，保留权限、只读与future-schema断言。ae50414 上 54 passed，0 skipped（run 9941e16a）。
- [ ] VER-6: 准入前逐方法/调用者只读审计 `Storage`、`ClientStorage::init`、util缓存、fetch写入顺序、exist_checked/batch、object_size、先unlink后delete_payload、bool/search与错误输出映射；以当前源码列出可能吞错的方法、对应真实测试及残余限制。发现写集外消费者违反fail-closed即ER03，不仅审计get/put。

**Full-suite trigger:** T-1：跨命令共享存储核心；A、C均完整默认套件，沿本文双env/nextest原命令；不以focused PASS替代。

**Dependencies:** `FIX-LTR-01`、`FIX-LTR-05P`、`FIX-LTR-05Q`、`FIX-LTR-05R`、`FIX-LTR-05S`、`FIX-LTR-05T`、`FIX-LTR-05U`、`FIX-LTR-05V`、`FIX-LTR-05W`、`FIX-LTR-05X`、`FIX-LTR-05Y`、`FIX-LTR-05Z`、`DEP-QUAL-SOURCE`、`DEP-QUAL-PUBLISH`、`DEP-QUAL-F2-ATTRIB`、`DEC-QUAL-05-RELEASE-WARNING`。本卡拆出 `FIX-LTR-05P` 至 `FIX-LTR-05Z`。05V 只改 AutoFollow 的对象探测。05W 只增加可失败构造并让 fetch 使用它。05X 只让 `init` 的确认失败不再变成可操作的 `LocalStorage`，且不把该失败写入对象缓存。05Y 只让未决 TYPE 的本地客户端不进对象缓存。05Z 只让这一次未决读取不再构造可写的 `LocalStorage`。本卡 VER-1 至 VER-5 已在 ae50414 通过。本提交占用 v0.30.88。v0.30.88 的 D 已于 2026-10-26T18:23:07Z 观察成功：base 40349034162 attempt 2、CodeQL 40349034316、release 40349204443，SHA e85470a。F2 仍是 unknown。依 2026-10-26T19:12:37Z 例外，FIX04 对 CLOSE=unknown 豁免。F2-CLOSE不是本卡前驱；独立缺陷必须另外登记其FIX-ID和边；不以未来FIX04的C/D为前驱，避免环。

**Deliverables:** 单轴生产修复、完整测试、下列同卡文档与证据摘要；不是仅输出诊断报告。

**Implementation write set:** 候选 `src/utils/client_storage.rs`（配置失败路径、消除该路径panic及模块测试）、`src/utils/util.rs`（缓存失败恢复与相邻单测）、`src/command/fetch.rs`（必要的写入前同轴检查及相邻单测）；VER-6审定前不得实施；`tests/command/fetch_test.rs`、`tests/command/output_flags_test.rs`、`tests/command/reset_test.rs`（诊断/同轴回归）；`docs/commands/config.md`、`docs/commands/zh-CN/config.md`、`docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md`、`docs/development/commands/config.md`、`docs/development/commands/fetch.md`、`COMPATIBILITY.md`、`tests/INDEX.md`、本计划、`docs/development/plan/plan-status.md`仅本计划行。网站同卡范围为已定位libra-backend的 `apps/tanstack-app/content/docs/commands/config.en.md`、`fetch.en.md`（本机当前仅有英文对应页，不虚构不存在的zh文件），在真实cf分支按ER06a交付；接收机器先核实际文件/命令/部署。若受影响公开命令或错误码超出上述清单，先逐文件补I并ER03，不遗漏同卡文档。

**Release write set:** Inherited；当前 `Cargo.toml`、`install.sh`、`install.ps1` 三版本面及工具链刷新 `Cargo.lock`、artifact；以执行时 `compat_version_surface_sync` 为权威。

**Files likely touched:** 与I/R相同；不修改ingress、reasoning helper、MCP worker或模板。锁测试须防env优先级遮蔽真实DB路径：用受审隔离guard清除合成LIBRA_STORAGE控制变量、保存并恢复原环境，匹配env/cwd串行lane；如新增serial影响 `tests/SERIAL_REGISTRY.tsv` / `.config/nextest.toml`，先逐文件补I并按仓库生成脚本重生成，ER03后才能修改，不手改并发组来掩盖故障。

**Docs and compatibility impact:** 这是用户可见的错误处理变化；先中文后英文，同卡同步config/fetch、开发说明、COMPATIBILITY与网站。明确「未配置remote」与「读取remote配置失败」的区别、错误后的恢复指引及machine输出；保留GlobalSchemaFuture已有特殊策略。不预造新的错误码；若需要新增，先更新I、错误码文档及对应Display/CLI测试。网站命令/typecheck/build、Claude、签名cf提交/push和部署HTTP均实际核验，缺外部条件不能宣称文档完成。

**Rollback mode:** `immutable-release`

**Migration and rollback:** 无schema迁移；已发布提交/版本不可撤回，若回归则新patch前滚。失败配置下不得新写数据；用户修正配置后重新发起操作。不得自动降级、重试、移除数据或重置已有ref。

**Security and privacy:** 不降低远端持久化保证，不输出secret/配置值/真实token；测试用合成canary与owned隔离库，失败清理须证明所有权。`exist=false`不能授权危险删除或把未知远端状态当已确认不存在。

**Performance budget:** 维持现有200ms配置DB锁上限，不增加重试，不新增无界扫描。F2诊断归入VER-4，无额外窗口；FIX05 A/C各候选一次10800秒，包含编译、listing、全部门、release构建、安装与资源后检。这些基于旧机器暖缓存耗时，仅为待审上限；新机必须先核工具链/资源/缓存与具体命令成本，由本地Claude准入。旧claim不能复用；未预审、冷缓存超预算或失败均STOP/ER03，不在claim外预编译或到时续窗。

**Estimated scope:** `pending VER-6`；候选3生产文件（client_storage/util/fetch），随附测试/双语文档完整登记。尚未通过G-04/ER03；冻结实际落点、文件数和恢复自洽性后填正式S/M，禁止以本次文档PASS代替实施准入。

**Version increment:** `patch`；当前0.30.87之后的0.30.88只是未被占用时的候选，必须现场核main/tag/Release；不得把本交接文档提交当版本占用。

**Release boundary:** `independent`；独立完整C/D及下一patch，不合并FIX04。

**C/D coverage from:** `self`；实际C/D后置证据由FIX04重新开工时在本计划与plan-status本计划行补记，不新建未登记补记提交。

**Granularity（未准入候选，禁止作为G-* PASS消费）:** `type=implementation; axis=配置读取失败时对象存储fail-closed; recovery=immutable-release前滚同轴修复; complete=pending VER-6; self-contained=pending VER-6; AC=8/8; VER=6/8; landing=pending VER-6; prod-files=3候选; scope=pending VER-6; deps=FIX-LTR-01,DEP-QUAL-SOURCE,DEP-QUAL-PUBLISH,DEP-QUAL-F2-ATTRIB,DEC-QUAL-05-RELEASE-WARNING; writeset=串行于FIX04之前; release=independent; split-from=FIX04-C1-F1; exception=N/A`。接收方必须把pending项变成真实满足模板的正式字段并获得本地Claude ER03 PASS，才可实施。本次交接审查只评价缺口/要求登记是否准确，不准入不自洽设计。

**明确的release profile门：** A/C均运行 `source .env.test && source .env.live-test && cargo build --release` 及 `source .env.test && source .env.live-test && cargo build --release --features keyring`。项目warning默认零；只有DEC实际获批时才按其唯一FIX05版本/两方法诊断范围核对。除此之外任何项目warning均STOP，不能把候选例外写入通用模板。

### Task FIX-LTR-05P: 存在性探测改为可失败结果

**Task type:** `migration`

**Lifecycle / Acceptance:** `in-progress` / 空（已推送 main `8a115ba`；family child 无独立发布，所以不是 done。session `7ed2380b-d9f1-4b5c-917f-203734f1f478` 已准入本写集）

**Description:** 单一行为轴是：存储读取失败不能再被布尔 `exist` 的 `false` 说成“对象不存在”。本卡增加 `ClientStorage::is_object_type_checked`，并把下列已点名的 `exist()==false` 缺失路径改成 `exist_checked`。`classify_presence` 改为返回 `Result`，存储错误由 `src/command/file.rs` 原样失败，不得映射成 `Absent`、`LooseOnly` 或 `PackedOnly`。`Storage::exist -> bool`、`ClientStorage::exist`、`ClientStorage::search`、`ClientStorage::init` 和 `objects_storage()` 的签名与现有成功路径保持不变。

**Out of scope:** 不实现配置失败后端，不改 `create_storage_backend` 的本地降级，不改 fetch 的 pack 写入顺序，不改 ingress cfg，不迁移本卡五个生产文件以外的 `exist` / `search` / `is_object_type` 调用方，不 bump、不打 tag。v0.30.88 仍只留给 FIX-LTR-05。本卡完成也不准入 FIX-LTR-05。五个生产文件的落点仍是 4 个：`is_object_type_checked`、fsck 的 13 处、cat-file 的 2 处、以及 `classify_presence` 的 `Result`（`file.rs` 只消费这个结果，不另算落点）。

**Current evidence:** `src/utils/client_storage.rs:2469` 的 `exist` 返回 `bool`；`:2546` 的 `is_object_type` 在读取错误时返回 `false`；`:2476` 的 `exist_checked` 已能返回 `Result`。本卡纳入的 `exist` 调用点是 `src/command/fsck.rs` 的 467、471、481、933、1769、1779、2388、2562、2656、2671、2750、2864、3006，以及 `src/command/cat_file.rs` 的 517、1219。`src/internal/obliteration/mod.rs:266` 的 `classify_presence` 在 `false` 时返回 `Absent`；唯一调用方是 `src/command/file.rs:182`，`emit_preview` 在 `:270`。这三个文件当前都不调用 `is_object_type`，所以新方法本卡只由 VER-1 覆盖，不假装已有生产调用方。ER03 session `e52e4025-3bab-423b-ae01-a666afcb458a` 要求先拆卡。相关调用分布在 66 个生产文件，本卡只收下面 5 个。

**Acceptance criteria:**

- [ ] AC-1: `is_object_type_checked` 在对象类型读取失败时返回 `Err`，不返回 `false`。
- [ ] AC-2: fsck 上列 13 处 `exist` 缺失判断改用 `exist_checked`，存储错误不是 “missing”。
- [ ] AC-3: cat-file 的 517 与 1219 两处改用 `exist_checked`，存储错误不是缺失退出。
- [ ] AC-4: `classify_presence` 返回 `Result`；`file.rs` 对 `Err` 失败返回，稳定错误码是 `StableErrorCode::IoReadFailed`，并且不把它显示成 absent、loose 或 packed。
- [ ] AC-5: 未迁移调用方的 `exist`、`search`、`init` 行为不变，且本卡不引入配置失败后端。
- [ ] AC-6: 三个版本面保持 `0.30.87`，不创建 `v0.30.88` tag。

**Verification:**

- [ ] VER-1: `source .env.test && source .env.live-test && cargo nextest run --lib -E 'test(is_object_type_checked_returns_read_error)' --no-fail-fast --retries 0`
- [ ] VER-2: `source .env.test && source .env.live-test && cargo nextest run --test command_test -E 'test(fsck_storage_read_error_is_not_missing)' --no-fail-fast --retries 0`
- [ ] VER-3: `source .env.test && source .env.live-test && cargo nextest run --test command_test -E 'test(cat_file_storage_read_error_is_not_absent)' --no-fail-fast --retries 0`
- [ ] VER-4: `source .env.test && source .env.live-test && cargo nextest run --lib -E 'test(classify_presence_storage_read_error_is_not_absent)' --no-fail-fast --retries 0`
- [ ] VER-5: `source .env.test && source .env.live-test && cargo nextest run --test command_test -E 'test(file_obliterate_storage_read_error_is_not_absent)' --no-fail-fast --retries 0`。该测试必须断言稳定错误码 `StableErrorCode::IoReadFailed`，并且输出不是 absent、loose 或 packed。

**Full-suite trigger:** `N/A`（本卡不改共享存储失败语义，也不碰 `sql/**`、`src/cli.rs`、错误码或版本面；收口全量仍由计划最终资格执行）。

**Dependencies:** `FIX-LTR-01`、`DEP-QUAL-SOURCE`。

**Deliverables:** 上述五个生产文件中的可失败探测，以及对应测试。`file.rs` 对 `classify_presence` 的 `Err` 使用现有 `StableErrorCode::IoReadFailed`，不新增错误码。

**Implementation write set:** `src/utils/client_storage.rs`、`src/command/fsck.rs`、`src/command/cat_file.rs`、`src/internal/obliteration/mod.rs`、`src/command/file.rs`、`tests/command/fsck_test.rs`、`tests/command/cat_file_test.rs`、`tests/command/file_obliterate_test.rs`、本计划、`docs/development/plan/plan-status.md` 仅本计划行。

**Release write set:** `N/A`

**Files likely touched:** 与 I 相同。

**Docs and compatibility impact:** `N/A`。本卡不新增命令、参数、配置或稳定错误码；存储读取失败改为可见错误是内部探测的失败传播，既有缺失对象的成功路径保持原退出与文案。没有对应的网站命令页需要改写。

**Rollback mode:** `immutable-release`

**Migration and rollback:** 无 schema 迁移。已推送提交只以前滚修正。回退行为是调用方重新使用布尔 `exist`，因此不得在本卡发布配置失败后端。

**Security and privacy:** 错误文本不包含配置值、凭据或数据库路径。`exist_checked` 的 `Err` 不得被当成删除许可。

**Performance budget:** 不增加重试，不放宽现有 200ms 配置锁。本卡本地验证一次 3600 秒，含编译。

**Estimated scope:** `M`

**Version increment:** `N/A`

**Release boundary:** `family child`。不 bump、不 tag。已批准的 `v0.30.88` 仍只属于 FIX-LTR-05 那一次发布；本卡提交可以进入 main，但不得占用该版本号。

**C/D coverage from:** `FIX-LTR-05`（本卡无独立 release artifact；main 上的 fmt/clippy 与本卡 VER 另记）。

**Granularity:** `type=migration; axis=存储读取失败不再被布尔存在性探测说成对象不存在; recovery=immutable-release前滚，且不携带配置失败后端; complete=yes; self-contained=yes; AC=6/8; VER=5/8; landing=4; prod-files=5; scope=M; deps=FIX-LTR-01,DEP-QUAL-SOURCE; writeset=序列化于FIX-LTR-05之前; release=family child; split-from=FIX-LTR-05; exception=N/A`

### Task FIX-LTR-05Q: 下一批存在性探测迁移

**Task type:** `migration`

**Lifecycle / Acceptance:** `in-progress` / 空（已推送 main `c0373cb`；family child 无独立发布，所以不是 done。session `3de3e11b-98e4-46ee-807c-e903b42bcf0e` 已准入这四个生产文件）

**Description:** 把 `show`、`maintenance`、`restore` 和 `Blob::save` 里把存储读取失败当成对象不存在的探测改成可失败结果。`Ok(false)` 保持原来的缺失或非提交语义。读失败分别使用 `IoReadFailed`、`RestoreError::ReadObject` 或 `io::Error`，不得调用 `put`。

**Out of scope:** 不包含 `am.rs` 的 `search`，因为 `Storage::search` 仍返回 `Vec`。不包含 `fetch.rs` 和配置失败后端。不 bump、不打 tag。

**Current evidence:** `src/command/show.rs` 的 `resolve_existing_object_hash`、`src/command/maintenance.rs` 的 `collect_agent_run_manifest_roots`、`src/command/restore.rs` 的两处 `is_object_type`、`src/utils/object_ext.rs` 的 `BlobExt::save`。准入 session `3de3e11b-98e4-46ee-807c-e903b42bcf0e`。

**Acceptance criteria:**

- [ ] AC-1: `show` 的存储读取失败是 `LBR-IO-001`，不是 bad revision。
- [ ] AC-2: maintenance 清单里的存储读取失败是 `IoReadFailed`，不是 “not in the store”。
- [ ] AC-3: `restore` 的类型读取失败是读错误，不是 “not a commit”。
- [ ] AC-4: `Blob::save` 在读取失败时返回错误并且不写入 loose 对象。

**Verification:**

- [ ] VER-1: `source .env.test && source .env.live-test && cargo nextest run --test command_test -E 'test(show_storage_read_error_is_not_bad_revision)' --no-fail-fast --retries 0`
- [ ] VER-2: `source .env.test && source .env.live-test && cargo nextest run --lib -E 'test(maintenance_storage_read_error_is_not_repo_corrupt)' --no-fail-fast --retries 0`
- [ ] VER-3: `source .env.test && source .env.live-test && cargo nextest run --lib -E 'test(restore_storage_read_error_is_not_non_commit)' --no-fail-fast --retries 0`
- [ ] VER-4: `source .env.test && source .env.live-test && cargo nextest run --lib -E 'test(blob_save_storage_read_error_does_not_write)' --no-fail-fast --retries 0`

**Dependencies:** `FIX-LTR-05P`。

**Deliverables:** 上述四个生产文件的可失败探测和对应测试。

**Implementation write set:** `src/command/show.rs`、`src/command/maintenance.rs`、`src/command/restore.rs`、`src/utils/object_ext.rs`、`tests/command/show_test.rs`、`tests/SRC_SERIAL_REGISTRY.tsv`、`tests/SRC_SERIAL_CENSUS.tsv`、本计划、`docs/development/plan/plan-status.md` 仅本计划行。

**Release write set:** `N/A`

**Files likely touched:** 与 I 相同。

**Docs and compatibility impact:** `N/A`。本卡不新增命令、参数、配置或稳定错误码。对象缺失和非提交引用的原错误保持不变；只有存储读取失败改为已有的 `LBR-IO-001` 或读错误。没有对应的网站命令页需要改写。

**Rollback mode:** `immutable-release`

**Migration and rollback:** 未实施，无迁移。

**Security and privacy:** 未实施。

**Performance budget:** 不增加重试。本卡本地验证一次 3600 秒，含编译。

**Estimated scope:** `M`

**Version increment:** `N/A`

**Release boundary:** `family child`

**C/D coverage from:** `FIX-LTR-05`

**Granularity:** `type=migration; axis=show、maintenance、restore 和 Blob::save 不再把存储读取失败当成对象不存在; recovery=immutable-release前滚; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=4; prod-files=4; scope=M; deps=FIX-LTR-05P; writeset=序列化于FIX-LTR-05之前; release=family child; split-from=FIX-LTR-05; exception=N/A`

### Task FIX-LTR-05R: 操作日志与 repack 的存在性探测

**Task type:** `migration`

**Lifecycle / Acceptance:** `in-progress` / 空（已推送 main `80ef760`；family child 无独立发布，所以不是 done。session `54f3e8cf-2bd8-426e-8a64-193e4d2f77f3` 已准入这四个生产文件；实现审查 `146781d3-4c39-4e90-b1f2-c34678d523e4` VERDICT PASS）

**Description:** 把操作日志快照、存储、恢复和 `repack --local` 里把存储读取失败当成对象不存在的探测改成可失败结果。`Ok(false)` 保持原来的缺失、回退哈希或丢弃 alternate-only 对象的语义。读失败分别是 `ScanError::Io`、`SnapshotError::Object`、`StoreError::Object`、`RestoreError::Object` 或 `StableErrorCode::IoReadFailed`，不得写成缺失、非提交，也不得调用 `put`。

**Out of scope:** 不包含 `am.rs` 的 `search`，因为 `Storage::search` 仍返回 `Vec`。不包含 `fetch.rs`、`client_storage` 的 `ensure_existing_object_index` / `search_result`、`cloud/agent_capture.rs`、tiered/remote 的布尔 `exist`，以及配置失败后端。不删除公开的 `OperationStoreV2::is_object_type`。不 bump、不打 tag。本卡完成也不准入 FIX-LTR-05。

**Current evidence:** `try_reuse_index_oid` 与 `put_content_addressed_object` 在 `src/internal/operation/snapshot.rs`；`OperationStoreV2::is_object_type` 在 `src/internal/operation/store.rs`；两处提交类型探测在 `src/internal/operation/restore.rs`；`retain_local_objects` 在 `src/command/repack.rs`。准入 session `54f3e8cf-2bd8-426e-8a64-193e4d2f77f3`。

**Acceptance criteria:**

- [ ] AC-1: `try_reuse_index_oid` 的存储读取失败是 `ScanError::Io`，不是 `Ok(None)`。
- [ ] AC-2: `put_content_addressed_object` 的存储读取失败是 `SnapshotError::Object`，并且不调用 `put`。`Ok(false)` 仍写入。
- [ ] AC-3: operation restore 的类型读取失败是 `RestoreError::Object`，正文保留读取错误，并且不是 “does not reference a commit object”。`Ok(false)` 仍使用这句缺失文案。
- [ ] AC-4: `retain_local_objects` 的存储读取失败是 `StableErrorCode::IoReadFailed`，正文含无完整索引，并且不是 missing 或 absent。`Ok(false)` 仍丢弃该对象。

**Verification:**

- [ ] VER-1: `source .env.test && source .env.live-test && cargo nextest run --lib -E 'test(snapshot_index_reuse_read_error_does_not_fall_through) or test(snapshot_storage_read_error_does_not_write)' --no-fail-fast --retries 0`
- [ ] VER-2: `source .env.test && source .env.live-test && cargo nextest run --lib -E 'test(store_type_read_error_stays_an_object_error)' --no-fail-fast --retries 0`
- [ ] VER-3: `source .env.test && source .env.live-test && cargo nextest run --lib -E 'test(internal::operation::restore::tests::restore_storage_read_error_is_not_non_commit)' --no-fail-fast --retries 0`
- [ ] VER-4: `source .env.test && source .env.live-test && cargo nextest run --lib -E 'test(repack_storage_read_error_is_not_absent)' --no-fail-fast --retries 0`

**Full-suite trigger:** `N/A`（本卡不改 `sql/**`、`src/cli.rs`、稳定错误码或版本面；公开 `is_object_type` 保留，避免 T-3）。

**Dependencies:** `FIX-LTR-05Q`。

**Deliverables:** 上述四个生产文件的可失败探测和对应测试。

**Implementation write set:** `src/internal/operation/snapshot.rs`、`src/internal/operation/store.rs`、`src/internal/operation/restore.rs`、`src/command/repack.rs`、`tests/SRC_SERIAL_REGISTRY.tsv`、`tests/SRC_SERIAL_CENSUS.tsv`、本计划、`docs/development/plan/plan-status.md` 仅本计划行。

**Release write set:** `N/A`

**Files likely touched:** 与 I 相同。

**Docs and compatibility impact:** `N/A`。本卡不新增命令、参数、配置或稳定错误码。对象缺失仍沿用原缺失或非提交文案；只有存储读取失败改为已有读错误。没有对应的网站命令页需要改写。

**Rollback mode:** `immutable-release`

**Migration and rollback:** 无 schema 迁移。已推送提交只以前滚修正。

**Security and privacy:** 错误文本不包含配置值、凭据或数据库路径。读取失败不得被当成对象不存在而跳过或覆盖。

**Performance budget:** 不增加重试。本卡本地验证一次 3600 秒，含编译。

**Estimated scope:** `M`

**Version increment:** `N/A`

**Release boundary:** `family child`。不 bump、不 tag。已批准的 `v0.30.88` 仍只属于 FIX-LTR-05 那一次发布。

**C/D coverage from:** `FIX-LTR-05`

**Granularity:** `type=migration; axis=操作日志快照、恢复和 repack 不再把存储读取失败当成对象不存在; recovery=immutable-release前滚; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=4; prod-files=4; scope=M; deps=FIX-LTR-05Q; writeset=序列化于FIX-LTR-05之前; release=family child; split-from=FIX-LTR-05; exception=N/A`

### Task FIX-LTR-05S: 索引补登记与修订后缀的类型过滤

**Task type:** `migration`

**Lifecycle / Acceptance:** `in-progress` / 空（已推送 main `6177646`；family child 无独立发布，所以不是 done。session `71cf457d-5724-48c9-b2e4-68c4ac1a8bc4` 已准入 `client_storage.rs`；实现审查 `d6849287-9d4e-46c7-8148-a41e8e5e412d` VERDICT PASS）

**Description:** `ensure_existing_object_index` 在存储读取失败时不再说载荷已经不存在。`search_result` 在 `~` / `^` 导览里过滤提交类型时，类型读取失败返回 `Err`；对象缺失和类型不符仍丢弃该哈希。`storage.search` 仍返回 `Vec`。

**Out of scope:** 不包含 `am.rs`、`fetch.rs` 的 AutoFollow `exist`、local/tiered/remote 的布尔 `exist`、trait 的 `exist_checked` 默认实现，以及配置失败后端。不 bump、不打 tag。本卡完成也不准入 FIX-LTR-05。

**Current evidence:** `ensure_existing_object_index` 与 `search_result` 都在 `src/utils/client_storage.rs`。准入 session `71cf457d-5724-48c9-b2e4-68c4ac1a8bc4`。

**Acceptance criteria:**

- [ ] AC-1: `ensure_existing_object_index` 的存储读取失败不是 `NotFound`，正文不含 “no longer present”。`Ok(false)` 仍是这句缺失文案。
- [ ] AC-2: `search_result` 的 `~` / `^` 类型读取失败返回 `Err`，不是空列表。`ObjectNotFound` 仍得到空列表。

**Verification:**

- [ ] VER-1: `source .env.test && source .env.live-test && cargo nextest run --lib -E 'test(ensure_existing_object_index_read_error_is_not_missing)' --no-fail-fast --retries 0`
- [ ] VER-2: `source .env.test && source .env.live-test && cargo nextest run --lib -E 'test(search_result_type_read_error_is_not_empty)' --no-fail-fast --retries 0`

**Full-suite trigger:** `N/A`（本卡不改 `sql/**`、`src/cli.rs`、稳定错误码或版本面）。

**Dependencies:** `FIX-LTR-05R`。

**Deliverables:** 上述一个生产文件的两处可失败探测和同文件测试。

**Implementation write set:** `src/utils/client_storage.rs`、`tests/SRC_SERIAL_REGISTRY.tsv`、`tests/SRC_SERIAL_CENSUS.tsv`、本计划、`docs/development/plan/plan-status.md` 仅本计划行。

**Release write set:** `N/A`

**Files likely touched:** 与 I 相同。

**Docs and compatibility impact:** `N/A`。本卡不新增命令、参数、配置或稳定错误码。载荷缺失仍是原来的 NotFound 文案。没有对应的网站命令页需要改写。

**Rollback mode:** `immutable-release`

**Migration and rollback:** 无 schema 迁移。已推送提交只以前滚修正。

**Security and privacy:** 错误文本不包含配置值、凭据或数据库路径。读取失败不得被当成载荷缺失。

**Performance budget:** 不增加重试。本卡本地验证一次 3600 秒，含编译。

**Estimated scope:** `S`

**Version increment:** `N/A`

**Release boundary:** `family child`。不 bump、不 tag。已批准的 `v0.30.88` 仍只属于 FIX-LTR-05 那一次发布。

**C/D coverage from:** `FIX-LTR-05`

**Granularity:** `type=migration; axis=索引补登记和修订后缀不再把存储读取失败当成对象不存在; recovery=immutable-release前滚; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=2; prod-files=1; scope=S; deps=FIX-LTR-05R; writeset=序列化于FIX-LTR-05之前; release=family child; split-from=FIX-LTR-05; exception=N/A`

### Task FIX-LTR-05T: am 的本地对象搜索不再把不可读目录当成缺失

**Task type:** `migration`

**Lifecycle / Acceptance:** `in-progress` / 空（已推送 main `e728736`；family child 无独立发布，所以不是 done。session `986c352a-927e-4f3f-855e-f9879764e9e7` 已准入这四个生产文件；实现审查 `02aa713c-60c8-4b6a-b5c6-7e09cf2096af` VERDICT PASS。前一轮 `de5171c1` 的 Windows 测试模块 P1 已用 `cfg(all(test, unix))` 关闭）

**Description:** `Storage::search_checked` 在本地对象目录或 pack 索引读失败时返回 `Err`。对象目录不存在仍是空的 loose 结果。`am` 的三方基线解析使用这条路径。没有唯一匹配时仍留下该 id。读失败是 `StableErrorCode::IoReadFailed`，不说成缺失。`Storage::search` 的 `Vec` 行为保持不变。

**Out of scope:** 不改 `tiered.rs` 和 `remote.rs`。它们继续使用 trait 默认实现，因此 tiered 仓库的本地目录错误仍被吞掉。该项登记为 `DEFER-QUAL-05-TIERED-SEARCH`，重启条件是后续切片为 tiered 覆盖 `search_checked`。不改 `fetch.rs`，不改 `search_result` 的修订解析，不 bump、不打 tag。本卡完成也不准入 FIX-LTR-05。

**Current evidence:** `LocalStorage::search` 在 `src/utils/storage/local.rs` 跳过不可读目录项。`resolve_three_way_bases` 在 `src/command/am.rs`。准入 session `986c352a-927e-4f3f-855e-f9879764e9e7`。

**Acceptance criteria:**

- [ ] AC-1: 本地对象目录不可读时，`search_checked` 返回 `Err`。目录不存在仍是空结果。
- [ ] AC-2: `am` 把该错误显示为 `IoReadFailed`，正文不是 missing、absent 或 not found。没有唯一匹配时仍不解析该 id。

**Verification:**

- [ ] VER-1: `source .env.test && source .env.live-test && cargo nextest run --lib -E 'test(am_unreadable_object_dir_is_not_a_missing_base)' --no-fail-fast --retries 0`

**Full-suite trigger:** `N/A`（本卡不改 `sql/**`、`src/cli.rs`、稳定错误码表或版本面）。

**Dependencies:** `FIX-LTR-05S`。

**Deliverables:** 上述四个生产文件的可失败前缀搜索，以及 `am` 的同文件测试。

**Implementation write set:** `src/utils/storage/mod.rs`、`src/utils/storage/local.rs`、`src/utils/client_storage.rs`、`src/command/am.rs`、`tests/SRC_SERIAL_REGISTRY.tsv`、`tests/SRC_SERIAL_CENSUS.tsv`、本计划、`docs/development/plan/plan-status.md` 仅本计划行。

**Release write set:** `N/A`

**Files likely touched:** 与 I 相同。

**Docs and compatibility impact:** `N/A`。本卡不新增命令、参数、配置或稳定错误码。没有唯一对象匹配时的原行为保持。没有对应的网站命令页需要改写。`ClientStorage::search_checked` 只做前缀搜索，不解析 `HEAD`、分支或 `~` / `^`。

**Rollback mode:** `immutable-release`

**Migration and rollback:** 无 schema 迁移。已推送提交只以前滚修正。

**Security and privacy:** 错误文本不包含配置值、凭据或数据库路径。目录读失败不得被当成补丁基线不存在。

**Performance budget:** 不增加重试。本卡本地验证一次 3600 秒，含编译。

**Estimated scope:** `M`

**Version increment:** `N/A`

**Release boundary:** `family child`。不 bump、不 tag。已批准的 `v0.30.88` 仍只属于 FIX-LTR-05 那一次发布。

**C/D coverage from:** `FIX-LTR-05`

**Granularity:** `type=migration; axis=am 的本地对象搜索不再把不可读目录当成缺失; recovery=immutable-release前滚; complete=yes; self-contained=yes; AC=2/8; VER=1/8; landing=4; prod-files=4; scope=M; deps=FIX-LTR-05S; writeset=序列化于FIX-LTR-05之前; release=family child; split-from=FIX-LTR-05; exception=N/A`

### Task FIX-LTR-05U: tiered 与 remote 的前缀搜索不再把列举失败当成缺失

**Task type:** `migration`

**Lifecycle / Acceptance:** `in-progress` / 空（已推送 main `1d3c8d2`；family child 无独立发布，所以不是 done。session `c1d476b0-876f-46a7-bb94-ce56405a85ee` 已准入 `remote.rs` 与 `tiered.rs`；实现审查 `bc600c5b-b019-4988-a347-7f25472ba8b7` VERDICT PASS）

**Description:** `RemoteStorage::search_checked` 在对象列举失败时返回 `Err`。`NotFound` 和空列举仍是没有匹配。`TieredStorage::search_checked` 同时采用本地和远端的可失败结果。离线或 local 读策略不访问远端。两端的 `search -> Vec` 保持原样。

**Out of scope:** 不改 `fetch.rs`、`client_storage.rs`、`local.rs` 和配置失败后端。不 bump、不打 tag。本卡完成也不准入 FIX-LTR-05。`DEFER-QUAL-05-TIERED-SEARCH` 的搜索路径由本卡关闭；fetch 的 AutoFollow 存在性判断随后由 FIX-LTR-05V 承接。

**Current evidence:** `remote.rs` 的 `search` 跳过列举错误。`tiered.rs` 的 `search` 合并两个 `Vec`。准入 session `c1d476b0-876f-46a7-bb94-ce56405a85ee` 要求离线策略不把远端不可达变成硬失败。

**Acceptance criteria:**

- [ ] AC-1: 远端列举失败是 `Err`，空列举和 `NotFound` 仍是空结果。
- [ ] AC-2: tiered 在自动策略下传播任一端的读失败。离线策略不访问远端。本地目录不可读仍是 `Err`。

**Verification:**

- [ ] VER-1: `source .env.test && source .env.live-test && cargo nextest run --lib -E 'test(remote_search_checked_distinguishes_list_failure_from_absence) or test(tiered_search_checked_local_only_skips_unreachable_remote) or test(tiered_search_checked_unreadable_local_dir_is_not_empty)' --no-fail-fast --retries 0`

**Full-suite trigger:** `N/A`（本卡不改 `sql/**`、`src/cli.rs`、稳定错误码或版本面）。

**Dependencies:** `FIX-LTR-05T`。

**Deliverables:** 上述两个生产文件的 `search_checked` 和同文件测试。

**Implementation write set:** `src/utils/storage/remote.rs`、`src/utils/storage/tiered.rs`、`tests/SRC_SERIAL_CENSUS.tsv`、本计划、`docs/development/plan/plan-status.md` 仅本计划行。

**Release write set:** `N/A`

**Files likely touched:** 与 I 相同。

**Docs and compatibility impact:** `N/A`。本卡不新增命令、参数、配置或稳定错误码。没有匹配对象时的空结果保持。没有对应的网站命令页需要改写。

**Rollback mode:** `immutable-release`

**Migration and rollback:** 无 schema 迁移。已推送提交只以前滚修正。

**Security and privacy:** 错误文本不包含配置值、凭据或数据库路径。列举失败不得被当成没有对象。

**Performance budget:** 不增加重试。本卡本地验证一次 3600 秒，含编译。

**Estimated scope:** `S`

**Version increment:** `N/A`

**Release boundary:** `family child`。不 bump、不 tag。已批准的 `v0.30.88` 仍只属于 FIX-LTR-05 那一次发布。

**C/D coverage from:** `FIX-LTR-05`

**Granularity:** `type=migration; axis=tiered 与 remote 的前缀搜索不再把列举失败当成缺失; recovery=immutable-release前滚; complete=yes; self-contained=yes; AC=2/8; VER=1/8; landing=2; prod-files=2; scope=S; deps=FIX-LTR-05T; writeset=序列化于FIX-LTR-05之前; release=family child; split-from=FIX-LTR-05; exception=N/A`

### Task FIX-LTR-05V: fetch AutoFollow 不再把存储读取失败当成缺失标签

**Task type:** `migration`

**Lifecycle / Acceptance:** `in-progress` / 空（已推送 main `835cfc5`；family child 无独立发布，所以不是 done。session `95af8652-b4f3-4116-8eba-16ae3237a780` 已准入 `fetch.rs`、`pull.rs`、`clone.rs`；实现审查 `72196c7f-6172-41c8-ae03-c5d1ccb2dccf` VERDICT PASS）

**Description:** AutoFollow 用一次 `exist_checked_many` 判断标签对象是否已在本地。`Ok(true)` 保留标签。`Ok(false)` 仍丢弃。无法解析的哈希仍丢弃。读取失败或结果缺键是 `FetchError::TagObjectProbe`，稳定错误码仍是 `StableErrorCode::IoReadFailed`。不新增错误码。

**Out of scope:** 不改配置失败后端、对象缓存、`write_pack_and_index` 的顺序，也不改 `Storage::exist`。不 bump、不打 tag。本卡完成也不准入 FIX-LTR-05。

**Current evidence:** `fetch.rs` 的 AutoFollow 原先对 `storage.exist` 使用 `unwrap_or(false)`。准入 session `95af8652-b4f3-4116-8eba-16ae3237a780` 要求用批量探测，因为逐个 `exist_checked` 会把正在安装的无索引 pack 误报为失败。

**Acceptance criteria:**

- [ ] AC-1: 存储读取失败是 `TagObjectProbe` / `IoReadFailed`，Display 模板不含 missing、absent 或 not found。`Ok(false)` 仍丢弃标签。
- [ ] AC-2: `pull` 与 `clone` 把该变体映射为 `IoReadFailed`，不落到网络不可用。

**Verification:**

- [ ] VER-1: `source .env.test && source .env.live-test && cargo nextest run --lib -E 'test(autofollow_storage_read_error_is_not_a_missing_tag)' --no-fail-fast --retries 0`

**Full-suite trigger:** `N/A`（本卡不改 `sql/**`、`src/cli.rs`、稳定错误码表或版本面）。

**Dependencies:** `FIX-LTR-05U`。

**Deliverables:** AutoFollow 批量探测、`FetchError::TagObjectProbe`，以及 `pull` / `clone` 各一处映射。

**Implementation write set:** `src/command/fetch.rs`、`src/command/pull.rs`、`src/command/clone.rs`、本计划、`docs/development/plan/plan-status.md` 仅本计划行。

**Release write set:** `N/A`

**Files likely touched:** 与 I 相同。

**Docs and compatibility impact:** `N/A`。本卡不新增命令、参数、配置或稳定错误码。标签对象确实不存在时仍不写入该标签。没有对应的网站命令页需要改写。

**Rollback mode:** `immutable-release`

**Migration and rollback:** 无 schema 迁移。已推送提交只以前滚修正。探测发生在引用更新之后，与 `persist_fetched_tags` 相同，重跑 fetch 是幂等的。

**Security and privacy:** 错误文本不包含配置值或凭据。读取失败不得被当成标签对象不存在而静默丢弃。tiered 远端列举若返回非 NotFound（例如对象存储 403），AutoFollow 会失败而不是丢掉标签。

**Performance budget:** 不增加重试。一次批量探测，不按标签重复扫描 pack 目录。本卡本地验证一次 3600 秒，含编译。

**Estimated scope:** `M`

**Version increment:** `N/A`

**Release boundary:** `family child`。不 bump、不 tag。已批准的 `v0.30.88` 仍只属于 FIX-LTR-05 那一次发布。

**C/D coverage from:** `FIX-LTR-05`

**Granularity:** `type=migration; axis=fetch AutoFollow 不再把存储读取失败当成缺失标签; recovery=immutable-release前滚; complete=yes; self-contained=yes; AC=2/8; VER=1/8; landing=4; prod-files=3; scope=M; deps=FIX-LTR-05U; writeset=序列化于FIX-LTR-05之前; release=family child; split-from=FIX-LTR-05; exception=N/A`

### Task FIX-LTR-05W: fetch 在写入前拒绝读不到的对象存储配置

**Task type:** `implementation`

**Lifecycle / Acceptance:** `in-progress` / 空（ER03 `c6cf48c1` PASS。实现审查 `2ee621eb` PASS。此前 `cf38ef33` 与 `040f1008` 仍是 FAIL。不消耗 v0.30.88。FIX-LTR-05 仍 blocked。本卡无独立发布）

**Description:** 单一行为轴是：新的可失败构造在确认失败时不得变成可操作的 `LocalStorage`。新增 `ClientStorage::resolve_configured_storage` 与 `util::resolve_configured_objects_storage`。这两个名字避开已经存在的 `util::try_objects_storage`（`src/utils/util.rs:1369`，仍走缓存和 `init`）以及 plan-20260906 ADR-SC-03 预留的 `ClientStorage::try_init`。`fetch` 在非 dry-run 的引用更新和 pack/index 安装之前调用新函数，失败则返回现有稳定码 `IoReadFailed`，并且不安装 pack、index、shallow 边界或引用。不新增 `Storage` 实现。确认失败不返回 `false`、空 `Vec` 或 `Ok(())`。不改 `create_storage_backend` 的函数体，因此 `init` 的降级和 builder panic 保持原样。

**Out of scope:** 不修改 `ClientStorage::init`、`create_storage_backend`、`objects_storage`、`cached_objects_storage` 和既有 `try_objects_storage`。不把失败结果写入 `OBJECTS_STORAGE_CACHE`，也不读取该缓存。不迁移其余 `init` / `objects_storage` 调用方。不改 `Storage` trait。不改 ingress、reasoning helper、MCP worker。不 bump，不打 tag，不消耗 v0.30.88。本卡完成也不准入 FIX-LTR-05。不实现 ADR-SC-03。

**Current evidence:** `create_storage_backend` 在 `src/utils/client_storage.rs:1611`。`Ok(None)` 于 `:1617` 返回 `LocalStorage`。`Err` 于 `:1620` 进入 `storage_config_resolution_fallback`（`:1820`）。空 bucket（`:1640`）、非法 endpoint（`:1679`，消息含原值）、空 key（`:1712`）、空 secret（`:1731`）、不支持的 type（`:1761`，消息含原值）和 `builder.build()` panic（`:1754`）都属于 `init`。`get_or_create_repo_id_for_prefix`（`:3494`）在读取失败时仍可能写入新的 `libra.repoid`。`cached_objects_storage` 在 `src/utils/util.rs:1374`。既有 `try_objects_storage` 在 `:1369`。`fetch` 在 `src/command/fetch.rs:2518` 的空引用路径会 `update_references`，在 `:2612` 调用 `write_pack_and_index`，之后 `:2639` 与 `:2776` 才调用 `objects_storage`。`fetch_repository_safe` 还被 `remote.rs:1268`、`convert.rs:153` 和 `maintenance.rs:2029` 调用。`clone.rs:834` 的 `_` 会把未单独列出的 `FetchError` 映射成 `NetworkUnavailable`。05W 审查 `0ccb6203`、`22682963`、`aa6093ac` 和 `6f0a9998` 都是 FAIL。`6f0a9998` 要求 TYPE 确认为 `s3` 或 `r2` 之后的 8 个解析 `Err` 都返回 `Err`，`GlobalSchemaFuture` 的本地降级只留在 TYPE 读取，不支持的 type 单独返回 `Err`。ER03 session `e52e4025` 仍禁止说谎的失败后端。

**Acceptance criteria:**

- [ ] AC-1: `resolve_configured_storage` 在两类确认失败上返回 `Err(StorageConfigFailure)`，不构造 `LocalStorage`，不 panic，不打印 `Warning`。第一类：成功读到的 `LIBRA_STORAGE_TYPE` 不是 `s3` 或 `r2`（包括 `gcs` 和 `S3`）。这发生在确认 remote 之前，但值已经读到，所以不是未决状态。第二类：TYPE 已经确认为 `s3` 或 `r2` 之后，`resolve_env_sync_typed` 对 `LIBRA_STORAGE_BUCKET`（`:1633`）、`LIBRA_STORAGE_ENDPOINT`（`:1671`）、`LIBRA_STORAGE_REGION`（`:1691`）、`LIBRA_STORAGE_ACCESS_KEY`（`:1704`）、`LIBRA_STORAGE_SECRET_KEY`（`:1723`）、`LIBRA_STORAGE_ALLOW_HTTP`（`:1743`）、`LIBRA_STORAGE_THRESHOLD`（`:1782`）或 `LIBRA_STORAGE_CACHE_SIZE`（`:1795`）返回 `Err`，以及空 bucket、非法 endpoint、空 key、空 secret、`builder.build()` 错误。这 8 个 `Err` 今天会进 `storage_config_resolution_fallback`，新函数不得这样。`builder.build()` 没有可注入的公开输入，VER-1 不覆盖这一臂，代码也不保存 builder 的错误文本。repoid 的读取失败、写入失败、存储路径失败、数据库 stat 失败和通道关闭同样是 `Err`。只有确认 `NotFound` 才允许无前缀。读取失败不得被当成「需要新 id」。写入失败不得返回尚未持久化的 UUID，也不得建成 `RemoteStorage`。
- [ ] AC-2: `StorageConfigFailure` 只保存变量名。它的 `Debug`、`Display` 和 `Error::source` 都不能带出底层错误。`FetchError::StorageConfig` 同样只有变量名和固定句子。canary 放在 `LIBRA_STORAGE_ENDPOINT`，并且只在夹具仓库建好之后、执行 fetch、pull 或 clone 时设置。这三条命令在调用 `fetch_repository_with_result` 之前都不调用 `ClientStorage::init`。断言覆盖这三条命令该次执行的 stdout、stderr 和 `--json`。
- [ ] AC-3: `LIBRA_STORAGE_TYPE` 成功解析为 `Ok(None)` 时仍是带 alternates 的 `LocalStorage`。进程环境没有该变量，而且配置库忙、stat 失败或解析线程没有返回，因而无法证明它是 local-only 还是 remote 时，新函数保持与 `init` 相同的本地结果。这个保留只覆盖尚未确认 remote 的 TYPE 读取。`GlobalSchemaFuture` 也只在读取 TYPE 时保持现有 warning 和 `LocalStorage`。TYPE 已确认为 `s3` 或 `r2` 之后，后续变量上的 `GlobalSchemaFuture` 返回 `Err`，不降级。无法解析的 threshold 与 cache size 数值仍用现有默认值；这和读取这些变量时的 `Err` 不是同一件事。没有具名用户决定，所以本卡不改未决的 TYPE 读取。
- [ ] AC-4: `resolve_configured_objects_storage` 不写入、不读取 `OBJECTS_STORAGE_CACHE`。既有 `try_objects_storage` 的签名和 fail-open 行为不变。同一次新调用只解析一轮。
- [ ] AC-5: `fetch` 的非 dry-run 路径在 `refs.is_empty` 的 `update_references` 之前，以及 `write_pack_and_index` 之前，调用 `resolve_configured_objects_storage`。`Err` 时不写 pack、index、shallow 文件或引用。dry-run 与空广告的提前返回保持原样。
- [ ] AC-6: 同一次 fetch 后文使用这次解析得到的 `ClientStorage`，不再调用 `objects_storage()`。`fetch` 的 `From`、`pull` 和 `clone` 都把 `FetchError::StorageConfig` 映射为 `IoReadFailed`。`clone` 的新分支写在 `_` 之前。
- [ ] AC-7: 不改 `init` 的前提下，用特征测试钉住它现在能到达的降级：非法 endpoint 与不支持的 type 仍打印含原值的 `Warning` 并返回可写的 `LocalStorage`；空 bucket、空 key、空 secret 仍降级。`create_storage_backend` 里的 builder panic 行保持不动；本卡不制造一个新的 panic 输入。
- [ ] AC-8: 文档只把 `IoReadFailed` 写给经过本卡测试的 `fetch`、`pull` 和 `clone`，并且只在已经确认请求了 remote 存储之后。未设置 `LIBRA_STORAGE_TYPE` 且配置库无法读取时，这三条命令保持原来的本地 fetch，文档不得写成它们会失败。`convert` 仍是 `RepoStateInvalid`。`maintenance` prefetch 只把错误文本放进任务消息。`remote update` 与 `remote add -f` 不另作稳定码断言。仍调用 `init` 的命令保持旧降级。

**Verification:**

- [ ] VER-1: `source .env.test && source .env.live-test && cargo nextest run --lib -E 'test(storage_config_resolve_)' --no-fail-fast --retries 0`。覆盖不支持的 type、AC-1 的 8 个后续 `Err` 分支、repoid 的读失败与写失败和 stat 失败、TYPE 读取上的 `GlobalSchemaFuture`，以及 TYPE 已确认后后续变量的 `GlobalSchemaFuture`。也覆盖 AC-3 的未决 TYPE 读取和 AC-4。不覆盖 `builder.build()`。
- [ ] VER-2: `source .env.test && source .env.live-test && cargo nextest run --test command_test -E 'test(storage_config_failure_hides_values) | test(storage_config_unresolved_fetch_stays_local)' --no-fail-fast --retries 0`。前一个过滤器覆盖 AC-2、AC-5 和 AC-6，分别执行 fetch、pull 和 clone。后一个过滤器覆盖 AC-3 的已确认缺失：TYPE 未设置且全局配置库不存在时，这三条命令不产生存储配置错误。仓库配置库不可读的未决分支由 VER-1 的 `storage_config_resolve_unresolved_type_stays_local` 覆盖，因为损坏的全局库还会让 `fetch.prune` 等其它读取失败。
- [ ] VER-3: `source .env.test && source .env.live-test && cargo nextest run --lib -E 'test(storage_config_init_fallback_)' --no-fail-fast --retries 0`。覆盖 AC-7。AC-8 由本卡文档句子验收，不另设 Cargo 门。

**Full-suite trigger:** 无。本卡不改公开签名、命令注册、稳定错误码枚举、SQL、安装器或共享测试基建。`FetchError` 新增一个变体，稳定码仍是既有 `IoReadFailed`。

**Dependencies:** `FIX-LTR-05V`。

**Deliverables:** 两个新函数、固定文案的错误类型、fetch 写入前检查、pull/clone 映射、特征测试与 canary 测试，以及 fetch 命令文档。

**Implementation write set:** `src/utils/client_storage.rs`、`src/utils/util.rs`、`src/command/fetch.rs`、`src/command/pull.rs`、`src/command/clone.rs`、`tests/command/fetch_test.rs`、`tests/command/pull_test.rs`、`tests/command/clone_test.rs`、`docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md`、`docs/development/commands/fetch.md`、`COMPATIBILITY.md` 的 fetch 行、本计划、`docs/development/plan/plan-status.md` 仅本计划行。若测试新增 `#[serial]`，按仓库脚本更新对应的 serial 登记和 `.config/nextest.toml`。不改 `docs/error-codes.md`。网站只在本机存在 `apps/tanstack-app/content/docs/commands/fetch.en.md` 时更新该页。

**Release write set:** `N/A`

**Files likely touched:** 与 I 相同。

**Docs and compatibility impact:** 见 AC-8。读取 TYPE 时的 `GlobalSchemaFuture` 保持原 warning。TYPE 已经是 `s3` 或 `r2` 时，后续配置读不到就失败。文档不得把本卡写成全部命令已经 fail-closed。

**Rollback mode:** `immutable-release`

**Migration and rollback:** 无 schema 迁移。已推送提交只以前滚修正。失败的 fetch 不留 pack 或引用；用户修正配置或等配置库锁释放后重新 fetch。

**Security and privacy:** 新路径不输出 endpoint、bucket、key、secret、解密原文或底层数据库错误。`init` 的旧 Warning 仍会带原值，特征测试只记录这个既有事实，不把它扩散到新路径。

**Performance budget:** 保持 200ms 配置库锁上限。新函数每次调用只解析一轮，不新增重试。本卡本地验证一次 3600 秒，含编译。

**Estimated scope:** `M`

**Version increment:** `N/A`

**Release boundary:** `family child`。不 bump、不 tag。已批准的 `v0.30.88` 仍只属于 FIX-LTR-05。

**C/D coverage from:** `FIX-LTR-05`

**Granularity:** `type=implementation; axis=fetch 在写入前拒绝读不到的对象存储配置; recovery=immutable-release前滚; complete=yes; self-contained=yes; AC=8/8; VER=3/8; landing=4; prod-files=5; scope=M; deps=FIX-LTR-05V; writeset=序列化于FIX-LTR-05之前; release=family child; split-from=FIX-LTR-05; exception=N/A`

四个落点是：`resolve_configured_storage` 的确认失败分类（含 repoid 读取失败且不先写入）、`resolve_configured_objects_storage` 与旧缓存及旧 `try_objects_storage` 隔离、fetch 写入前检查并沿用同一次 handle、`FetchError::StorageConfig` 到 `IoReadFailed` 的映射。五个生产文件是 `client_storage.rs`、`util.rs`、`fetch.rs`、`pull.rs`、`clone.rs`。

### Task FIX-LTR-05X: init 在确认失败时不再构造本地存储

**Task type:** `implementation`

**Lifecycle / Acceptance:** `in-progress` / 空（ER03 `8059d2f4` PASS。实现审查 `bd0e229d` PASS。此前 `16425b1a` 仍是 FAIL。不消耗 v0.30.88。FIX-LTR-05 仍 blocked。本卡无独立发布）

**Description:** 单一行为轴是：`ClientStorage::init` 在配置已经确认失败时，不得再构造可写的 `LocalStorage`，也不得把这个失败结果放进 `OBJECTS_STORAGE_CACHE`。`init` 和四个正式上转点调用新的 `open_init_storage`，不调用 `resolve_configured_storage`。`Ok` 是 Ready 客户端。`Err` 变成只保存变量名的拒绝状态，不构造 `LocalStorage`，不打印 `Warning`，不保留配置值，不 panic。不新增第二种 `Storage` 类型。已经存在的 `impl Storage for ClientStorage`（`client_storage.rs:3280`）留在原类型上，但拒绝状态不能从它得到 `false`、空 `Vec`、`Ok(())` 或 `Ok(None)`。`ClientStorage::init -> ClientStorage` 以及 `exist -> bool`、`is_object_type -> bool`、`search -> Vec` 的签名都不变，避免改 snapshot fixture 和 `tiered.rs:1122`。Ready 路径把普通读取错误折成 `false` 或空 `Vec` 是既有行为，本卡不修。

**Out of scope:** 不改未决的 `LIBRA_STORAGE_TYPE` 读取。类型没读到时，包括配置库忙、stat 失败、解析线程没有返回和损坏的库，仍是现在的本地结果和 warning，并且这个成功的本地客户端仍会进缓存。没有具名用户决定，所以本卡不改这个契约。本句是 05X 当时的范围。其后的 FIX-LTR-05Y 只去掉这个缓存，不把这一次调用改成拒绝状态。不改 `resolve_cache_config` 和 `libra cache info` 的现有静态报告。不改 `try_objects_storage` 的签名。不改 fetch 的写入前检查。不改 `init_local`、`init_local_existing` 和 `init_local_existing_with_alternates`。不改 ingress、reasoning helper、MCP worker。不 bump，不打 tag，不消耗 v0.30.88。本卡完成也不准入 FIX-LTR-05。不实现 ADR-SC-03，不使用名字 `try_init`。`src/internal/ai/capture/live_pipeline_tests.rs:3724` 是测试里的 `init`，本卡不改它；它没有确认失败的配置时仍得到 Ready 客户端。

**Current evidence:** `init` 在 `src/utils/client_storage.rs:1591`，它调用 `create_storage_backend`（`:1659`）。确认失败仍会构造 `LocalStorage` 并打印含原值的 `Warning`。`builder.build()` 的 panic 在 `:1802`。`resolve_configured_storage`（`:1893`）已经对同一类确认失败返回 `Err`。`impl Storage for ClientStorage` 在 `:3280`。它的 `exist`（`:3351`）和 `search`（`:3371`）调用固有方法。trait 默认的 `exist_checked`、`heal`、`delete_payload`、`evict_local` 会把布尔失败折成 `Ok(false)` 或 `Ok(())`。`cached_objects_storage` 在 `src/utils/util.rs:1383`。审查 `2586a99a` 核对后，正式代码里的 `Arc::new(ClientStorage::init(...))` 上转只有四处：`src/internal/ai/hooks/intent.rs:629`、`src/command/agent/clean.rs:298`、`src/internal/ai/subagent_content.rs:7868`、`src/internal/ai/capture/checkpoint.rs:4885`。`history.rs:13172`、`coverage_gate.rs:5470` 与 `:6983`、`live_checkpoint.rs:7194`、`checkpoint.rs:7521`、`subagent_content.rs:11443`、`:11615`、`:12940` 和 `live_pipeline_tests.rs:3724` 都在测试模块里，本卡不改。`checkpoint.rs:4885` 位于 marker 登记之后，所在函数返回 `CheckpointStoreError::StoreFailure { stage }`，这个错误不带文本。同函数 `:4876` 在失败时先 `retire_failed_attempt_with_diagnostic`。`create_storage_backend` 的名字还出现在 `:1492`、`:1501`、`:1530`、`:1583` 和 `docs/development/commands/cache.md`。`storage_config_init_fallback_*` 在 `client_storage.rs:7474` 起，目前断言旧降级。`e52e4025` 仍禁止用 false 或空集合掩盖错误。

**Acceptance criteria:**

- [ ] AC-1: `init` 对确认失败返回拒绝状态。`put` 失败。全新的对象目录路径不被创建。stderr 没有 `Warning`，也没有 canary。覆盖 `gcs` 和 `S3`、空 bucket、空 key、空 secret、非法 endpoint，以及 TYPE 已确认为 `s3` 或 `r2` 之后那 8 个设置的 `Err`。`LIBRA_STORAGE_TYPE=local` 不在此列：它保持今天的 Warning 和可写 `LocalStorage`，因为没有具名决定，而且 `push.rs`、`git_client.rs`、`ssh_client.rs` 和 `global_config_schema_future_test.rs` 依赖这个结果。`builder.build()` 仍没有可注入输入，本卡不覆盖它，也丢弃 builder 的错误文本。`create_storage_backend` 删除，panic 行随之删除。只被它调用的 `get_or_create_repo_id_for_prefix`（`:3668`，唯一调用在 `:1818`）一并删除，并去掉 `:4177` 的文字提及，避免 `dead_code`。`:1492`、`:1501`、`:1530`、`:1583` 和 `docs/development/commands/cache.md` 里对已删除函数名的说明改到仍存在的行为。`storage_config_init_fallback_*` 改写成确认失败不再降级；`local` 和未决 TYPE 的旧结果保留。
- [ ] AC-2: `storage: Arc<dyn Storage>` 改成私有枚举 `Ready(Arc<dyn Storage>)` 或 `Rejected(&'static str)`。拒绝状态不放 `LocalStorage`，也不新增第二种 `Storage` 实现。所有现有 `self.storage` 读取都经过同一个取用函数；拒绝状态从这里返回固定错误。下面每个返回 `Result` 的方法都先走这个取用，包括空输入也不先返回成功：`get`、`get_with_limit`、`get_typed_with_limit`、`get_typed_bounded`、`get_commit_bounded`、`commit_parents_many`、`object_type_bounded_probe`、`object_types_bounded_probe`、`object_types_bounded_probe_with_budget`、`get_object_type`、`get_object_type_bounded`、`get_object_types_bounded_many`、`get_object_types_bounded_many_with_budget`、`put`、`exist_checked`、`exist_checked_many`、`exist_checked_batch`、`is_object_type_checked`、`search_checked`、`search_result`、`ensure_existing_object_index`、`delete_payload`、`heal`、`object_size`、`object_sizes`、`object_sizes_with_total_limit`、`evict_local`、`end_object_index_batch`、`flush_object_index_batch`。`commit_parents_many(&[])` 在拒绝状态下也是 `Err`，不是 `Ok(空 map)`。`exist_checked` 不得调用会结束进程的布尔 `exist`。`delete_payload` 不是 `Ok(())`。`evict_local` 不是 `Ok(None)`。`heal` 不是 `Ok(false)`。`object_size` 不是 `Ok(None)`。Ready 状态下，已经委派的方法继续委派；今天走 trait 默认的 `delete_payload`、`heal`、`evict_local` 继续走默认，不改成 tiered 的远端删除。`begin_object_index_batch` 和 `abort_object_index_batch` 在拒绝状态下都是空操作，不碰共享批深度或其它 handle 的 pending。
- [ ] AC-3: `exist -> bool`、`is_object_type -> bool`、`search -> Vec` 和 trait 的 `exist`、`search`、`exist_batch` 保持签名。拒绝状态在 `search_result` 的错误折叠和 `is_object_type` 的 `Err => false` 之前就结束。`exist_batch` 即使输入是空切片也先看拒绝状态，不返回空 `Vec`。这些方法调用同一个私有终止：向 stderr 写完整句子 `failed to read storage configuration for {variable}`，然后 `std::process::exit(1)`。这不是 `panic!`，也不写 `Warning`。Ready 路径上，普通读取错误仍折叠成 `false` 或空 `Vec`，本卡不把这个历史折叠说成已修。子进程逐一覆盖固有 `exist`、`is_object_type`、`search`，trait 的 `exist` 和 `search`，以及 `exist_batch` 的空输入和非空输入。每个子进程退出码是 1，stderr 只有该句子，没有 canary，父进程本身不退出。
- [ ] AC-4: 拒绝状态经 `Result` 返回的文本只有 `failed to read storage configuration for {variable}`。`Debug` 和 `Error::source` 不带底层错误或配置值。canary 放在 endpoint 或 type 里。断言完整句子，不用 `contains("")`。
- [ ] AC-5: 新增 `ClientStorage::open_init_storage(base_path: PathBuf) -> Result<ClientStorage, StorageConfigFailure>`，放在 `client_storage.rs`。`init` 在 `Ok` 时返回该客户端，在 `Err` 时返回拒绝状态。四个上转点也调用它。它只调用一次 `resolve_env_sync_typed("LIBRA_STORAGE_TYPE")`。`s3`/`r2` 之后的 bucket、endpoint、key、secret、region、allow-http、threshold、cache size 和 repo id 前缀不复制第二份：从 `resolve_configured_storage` 抽出私有 `open_after_type_read`，参数是已经读到的 TYPE 结果，函数体内不再读 TYPE。`resolve_configured_storage` 变成读一次 TYPE 再调用它，对外结果不变，包括 `local` 仍然是 `Err`。这是 05W 已有的 fetch、pull、clone 行为，本卡不改那三条命令，也不把它写成 `init` 的旧行为。`open_init_storage` 在读到正好 `local` 时不调用 `open_after_type_read`。它接着做 `create_storage_backend` 在 `:1677` 对 bucket 做的同一次读取：bucket 读失败用今天的解析 Warning 和 `LocalStorage`；bucket 为空用今天的空值 Warning 和 `LocalStorage`；bucket 可用时才打印今天的 unsupported-type Warning（值是单词 `local`）并返回 `LocalStorage`。不第二次读 TYPE，也不建造 S3。`Ok(None)`、未决 TYPE，以及读取 TYPE 时的 `GlobalSchemaFuture`，仍由 `open_after_type_read` 保持现有本地结果。`gcs` 和 `S3` 是 `Err`。`#[cfg(test)]` 计数器只加在这两次公开的 TYPE 读取上，不加在 `open_after_type_read` 里。VER-1 断言 `open_init_storage` 和 `resolve_configured_storage` 各自只增加 1。
- [ ] AC-6: 正式代码里的四个上转点改成调用 `open_init_storage`，成功时才放进 `Arc<dyn Storage>`。因此 `local` 在这四处仍成功并打印原来的 Warning，不失败。`intent.rs:629`、`clean.rs:298` 和 `subagent_content.rs:7868` 在确认失败时用各自函数已经返回的错误类型，文本只用 `Display`。`checkpoint.rs:4885` 在 `ConfiguredCreate` 分支里，确认失败时先调用 `retire_failed_attempt_with_diagnostic`，再返回 `CheckpointStoreError::StoreFailure { stage: ObjectWrite }`。这个错误不带句子。retire 是尽力而为：`Ok(false)` 或 `Err` 时函数只记日志，普通 marker 可能留下，但返回值仍是 `ObjectWrite`。VER-2 的成功路径断言 marker 已 retire。另用已有的 `CheckpointFaultPoint::MarkerCleanup` 证明 retire 失败时仍然返回 `ObjectWrite`。`LocalExisting` 仍用 `init_local_existing`。测试模块里的 `init` 上转不改。不新增稳定错误码，也不为这些命令新写 `LBR-IO-001` 断言。
- [ ] AC-7: `cached_objects_storage` 不插入拒绝状态。坏配置调用之后缓存里没有该路径；去掉该配置后的下一次 `objects_storage` 可以 `put`。未决 TYPE 的本地结果仍可被缓存。没有自动重试循环。
- [ ] AC-8: fetch 文档不再写“仍走 `init` 的命令保留本地降级”。`cache info` 对错误类型、空 key 或非法 endpoint 仍可以报告 `tiered=false`，文档写明这不是 `init` 造出了可写本地后端。`cache evict` 在拒绝状态下走 `evict_local` 的 `Err`，而不是“没有持久层”。除 05W 已经测试的 fetch、pull、clone 外，不把 `LBR-IO-001` 写给其它命令。

**Verification:**

- [ ] VER-1: `source .env.test && source .env.live-test && cargo nextest run --lib -E 'test(storage_config_init_) | test(storage_config_hold_)' --no-fail-fast --retries 0`。同一命令再跑一次，两次都通过。用一张表逐个调用 AC-2 列出的方法。`exist_checked_many`、`exist_checked_batch`、`object_types_bounded_probe`、`object_types_bounded_probe_with_budget`、`get_object_types_bounded_many`、`get_object_types_bounded_many_with_budget`、`object_sizes`、`object_sizes_with_total_limit`、`commit_parents_many` 和 `exist_batch` 都用空切片调用。拒绝状态都得到固定错误或进程退出，不返回空 map。Ready 状态不改今天的默认删除行为。子进程覆盖 AC-3，并另有一个 `TYPE=local` 的 `init` 子进程：退出码 0，stderr 含原来的 Warning，对象可写。不覆盖 `builder.build()`。改写后的 `storage_config_init_fallback_*` 也在这个过滤器里，不得再把 `gcs` 或非法 endpoint 断言成旧降级。
- [ ] VER-2: `source .env.test && source .env.live-test && cargo nextest run --lib -E 'test(storage_config_upcast_)' --no-fail-fast --retries 0`。这些测试可以加在 `intent.rs`、`clean.rs`、`subagent_content.rs` 和 `checkpoint.rs` 已有的测试模块里，不算新的生产文件。确认失败时，`intent`、`clean` 和 `subagent_content` 三处断言看到固定句子，并且该次调用没有执行 trait 的 `exist` 或 `search`。`checkpoint` 的成功 retire 路径断言 stage 是 `ObjectWrite` 且普通 marker 已 retire。`MarkerCleanup` 故障点断言 retire 失败时仍然是 `ObjectWrite`。这四处在 `TYPE=local` 下都成功，并打印与 `init` 相同的 Warning。确认失败的两条 checkpoint 断言不要求错误文本里有配置句子。

**Full-suite trigger:** 无。本卡不改公开签名、CLI 注册、稳定错误码、SQL、安装器或版本面。父卡 FIX-LTR-05 的全量仍留在父卡发布。

**Dependencies:** `FIX-LTR-05W`。

**Deliverables:** `open_init_storage`、`init` 的拒绝状态、`impl Storage` 的 `Result` 覆写、布尔查询的进程退出兜底、四个上转点改为调用 `open_init_storage`、缓存不插入拒绝状态、特征测试，以及 fetch 与 cache 文档。

**Implementation write set:** `src/utils/client_storage.rs`、`src/utils/util.rs`、`src/internal/ai/hooks/intent.rs`、`src/command/agent/clean.rs`、`src/internal/ai/subagent_content.rs`、`src/internal/ai/capture/checkpoint.rs`、`docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md`、`docs/development/commands/fetch.md`、`docs/commands/cache.md`、`docs/commands/zh-CN/cache.md`、`docs/development/commands/cache.md`、`COMPATIBILITY.md` 里描述该降级的 fetch 与 cache 行、`tests/command/cache_test.rs` 里把“后端会退回本地”说成事实的注释、本计划、`docs/development/plan/plan-status.md` 仅本计划行。生产代码只改四个上转点：`intent.rs:629`、`clean.rs:298`、`subagent_content.rs:7868`、`checkpoint.rs:4885` 的 `ConfiguredCreate` 分支。这四个文件已有的测试模块可以加入 `storage_config_upcast_` 测试。若新增 `#[serial]`，按仓库脚本更新登记和 `.config/nextest.toml`。不改 `docs/error-codes.md`。不改 `tiered.rs`、snapshot 测试，也不改测试模块里现有的 `init` 上转。不改把 `LIBRA_STORAGE_TYPE` 设为 `local` 的既有测试。网站只更新本机存在的 `fetch.en.md` 与 `cache.en.md`。

**Release write set:** `N/A`

**Files likely touched:** 与 I 相同。

**Docs and compatibility impact:** 见 AC-8。先中文后英文。`config.md` 没有这条降级的现成段落，本卡不新造一节。`cache info` 的 JSON 字段和退出约定不变。

**Rollback mode:** `immutable-release`

**Migration and rollback:** 无 schema 迁移。已推送提交只以前滚修正。拒绝状态不写对象。用户修正配置后，下一次未缓存的调用重新解析。

**Security and privacy:** 错误文本和退出兜底的 stderr 只有变量名。测试 canary 用合成值。`exist=false` 不得用来表示配置失败。

**Performance budget:** 保持 200ms 配置库锁上限。确认失败不插入缓存，所以下一次调用会再解析一次，这不是重试循环。进程退出兜底只发生在布尔查询碰到拒绝状态时，不新增重试。本卡本地验证一次 3600 秒，含编译。

**Estimated scope:** `M`

**Version increment:** `N/A`

**Release boundary:** `family child`。不 bump、不 tag。已批准的 `v0.30.88` 仍只属于 FIX-LTR-05。

**C/D coverage from:** `FIX-LTR-05`

**Granularity:** `type=implementation; axis=init 在确认失败时不再构造本地存储; recovery=immutable-release前滚; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=4; prod-files=6; scope=M; deps=FIX-LTR-05W; writeset=序列化于FIX-LTR-05之前; release=family child; split-from=FIX-LTR-05; exception=N/A`

四个落点是：拒绝状态与删除 `create_storage_backend`、`impl Storage` 的 `Result` 覆写和布尔查询兜底、对象缓存不插入拒绝状态、正式代码里的四个上转改走 `open_init_storage`。六个生产文件是 `client_storage.rs`、`util.rs`、`intent.rs`、`clean.rs`、`subagent_content.rs`、`checkpoint.rs`。测试模块里的上转不改。签名不变，所以 `tiered.rs:1122` 和 snapshot 的 `exist` 调用不必修改。Ready 状态不把 `delete_payload` 改成真正的远端删除。`resolve_configured_storage` 的对外结果不改，包括 fetch、pull、clone 对 `TYPE=local` 的 `Err`。它的函数体只是把 TYPE 读取之后的部分抽到 `open_after_type_read`。

### Task FIX-LTR-05Y: 未决的对象存储类型不进入对象缓存

**Task type:** `implementation`

**Lifecycle / Acceptance:** `in-progress` / 空（ER03 `db7a5d7f-e575-4535-9691-67fc3ca9186f` PASS。实现审查 `a2a2af14-9149-405c-906e-7eb8fdcba267` PASS。不消耗 v0.30.88。FIX-LTR-05 仍 blocked。本卡无独立发布）

**Description:** 单一行为轴是：`LIBRA_STORAGE_TYPE` 这一次没有读到，而且原因不是 `GlobalSchemaFuture` 时，这次调用仍得到今天的可写 `LocalStorage` 和今天的解析 Warning，但返回的 `ClientStorage` 不得写入 `OBJECTS_STORAGE_CACHE`。下一次 `objects_storage` 或 `try_objects_storage` 重新调用 `ClientStorage::init`。没有重试循环。这一次调用不改成拒绝状态。`resolve_configured_storage` 对同一种未决读取仍返回 `Ok`。

**Out of scope:** 不把未决 TYPE 改成 `Rejected`，也不对它调用 `process::exit`。没有具名决定把这一次本地命令改成新的错误。本句是 05Y 当时的范围。其后的 FIX-LTR-05Z 按 2026-10-26 的具名决定把这一次调用改成拒绝状态，不再构造可写的 `LocalStorage`。不改 `GlobalSchemaFuture` 的 Warning、本地结果或可缓存性。不改 `Ok(None)` 的纯本地结果。不改正好是 `local` 的历史 Warning，也不改 `historical_local_word` 里 bucket 读失败的可缓存本地结果，因为那时 TYPE 已经确认是 `local`。不改确认失败的拒绝状态，也不改它已经不进缓存的行为。不改 fetch、pull、clone 的写入前检查，也不把 `TYPE=local` 在这三条命令上的 `Err` 改掉。不改 `exist`、`search`、`is_object_type` 的签名，不返回 `false` 或空 `Vec` 来掩盖错误。不新增 `Storage` 实现，不使用名字 `try_init`。不改 `storage_config_resolution_fallback` 的文本。不第二次读 TYPE。不改 `resolve_cache_config` 和 `libra cache info` 的静态报告。不 bump，不打 tag，不消耗 v0.30.88。本卡完成也不准入 FIX-LTR-05。不改 ingress、reasoning helper、MCP worker。网站 `cf` 当前不能快进，且有无关脏文件，本卡不改网站，也不 force-push。

**Current evidence:** `open_after_type_read` 在 `src/utils/client_storage.rs:1751`。TYPE 的 `Err` 调用 `storage_config_resolution_fallback`（`:1699`）后 `return Ok(Self::ready(storage, base_path))`（`:1757`）。`ready`（`:1595`）是 `HeldStorage::Ready`。`configuration_variable`（`:1609`）对 Ready 返回 `None`。`cached_objects_storage` 在 `src/utils/util.rs:1383`，只有 `configuration_variable().is_some()` 时才不插入（`:1392`）。因此未决 TYPE 的可写本地客户端会留在缓存里，后面的调用不再读配置。`resolve_env_sync_typed`（`:3376`）先看进程环境变量，命中就不再读库。`break_repository_database`（`:7086`）把仓库库换成目录，用来制造未决读取。`storage_config_resolve_unresolved_type_stays_local`（`:7287`）断言 `resolve_configured_storage` 仍是 `Ok`。`storage_config_hold_cache_does_not_keep_rejection`（`:7939`）只覆盖确认失败。`storage_config_resolve_type_global_schema_stays_local`（`:7376`）覆盖 future schema 的本地结果。`try_objects_storage`（`util.rs:1368`）走同一个 `cached_objects_storage`。`resolve_configured_objects_storage`（`:1378`）不读写这份缓存。

**Acceptance criteria:**

- [ ] AC-1: `ClientStorage` 增加私有字段 `retains_objects_cache: bool`，并提供 `pub(crate) fn retains_objects_cache(&self) -> bool`。`ready`、`rejected`、`init_local`、`init_local_existing`、`init_local_existing_with_alternates`、`from_test_storage`、`historical_local_word`、`local_configured_client` 和成功的 `s3`/`r2` 臂都设为 `true`。不新增 `HeldStorage` 变体。
- [ ] AC-2: `open_after_type_read` 的 TYPE `Err` 臂仍调用原来的 `storage_config_resolution_fallback`，仍返回 `Ok`，仍打印原来的 Warning。只有错误不是 `StorageConfigResolutionError::GlobalSchemaFuture` 时，返回的客户端 `retains_objects_cache()` 为 `false`。`GlobalSchemaFuture` 仍是 `true`。这个判断使用已经拿在手里的错误，不再读 TYPE。`init`、`open_init_storage` 和 `resolve_configured_storage` 都经过这条臂，所以两边的未决 TYPE 客户端都不该被缓存。`resolve_configured_storage` 的结果类型仍是 `Ok`，不是 `Err`。
- [ ] AC-3: `cached_objects_storage` 在 `configuration_variable().is_some()` 或 `!retains_objects_cache()` 时直接返回，不插入。否则仍按路径插入。`objects_storage` 和 `try_objects_storage` 都走这里。没有重试循环，没有第二次构造之后再插入。
- [ ] AC-4: 特征测试调用真正的 `objects_storage`。未决读取使用 `isolate_storage_env`、新仓库、`break_repository_database` 和指向不存在文件的 `LIBRA_CONFIG_GLOBAL_DB`。第一次 `objects_storage` 之前把 `STORAGE_TYPE_READS` 置 0。调用后它是 1，`configuration_variable()` 是 `None`，`retains_objects_cache()` 是 `false`，`objects_storage_cache_contains(path::objects())` 是 `false`，`put` 成功。然后用 `ScopedEnvVar` 把 `LIBRA_STORAGE_TYPE` 设为 `gcs-canary`。因为环境变量先于数据库，损坏的库不能挡住这个新值。第二次 `objects_storage` 的 `configuration_variable()` 是 `Some("LIBRA_STORAGE_TYPE")`，缓存仍没有该路径，`put` 的 `Display` 正好是 `failed to read storage configuration for LIBRA_STORAGE_TYPE`，并且不含 `gcs-canary`。同一损坏仓库上、设置 canary 之前，再把计数器置 0 并调用一次 `resolve_configured_storage`：计数器是 1，结果是 `Ok`，`retains_objects_cache()` 是 `false`，这次调用也不插入 `path::objects()`。另外三例保持可缓存：可读且未设置 TYPE 的 `Ok(None)`、正好 `local`、读取 TYPE 时的 `GlobalSchemaFuture`。这三例在 `objects_storage` 之后缓存含有 `path::objects()`，`retains_objects_cache()` 是 `true`。可读未设置的那例在随后设上 `gcs-canary` 时，下一次 `objects_storage` 仍是缓存里的本地客户端，`put` 仍成功。future schema 的一次性 Warning 不作为断言。`storage_config_resolve_unresolved_type_stays_local` 仍是 `Ok`，并补上 `retains_objects_cache()` 为 `false`。
- [ ] AC-5: `docs/commands/cache.md` 在 `LIBRA_STORAGE_TYPE=local` 仍警告并保持本地的句子后写明：读不到 `LIBRA_STORAGE_TYPE` 时，这一次调用仍使用那个历史本地后端，进程内对象缓存不保留该客户端。只要类型仍然读不到，之后每一次 `objects_storage` 调用都会重新读取，包括每次经它打开对象的 `Tree::load`、`Commit::load` 和 `Blob::load`。这不是重试循环。future global schema、正好 `local`、以及可读的未设置类型仍可以缓存。已确认不能使用的设置继续不进这份缓存。`docs/commands/zh-CN/cache.md` 写对应中文。`docs/development/commands/cache.md` 的当前状态加同一句事实。`docs/commands/fetch.md` 在“未设置且配置库读不到时 fetch 仍走历史本地路径”之后写明：fetch、pull、clone 的这一次结果来自 `resolve_configured_storage`，不读取进程内对象缓存；经 `init` 打开存储的命令不把这种读不到的类型留在该缓存里。`docs/commands/zh-CN/fetch.md` 与 `docs/development/commands/fetch.md` 写同一事实。不改 `COMPATIBILITY.md`，它的 cache 行描述的是 `libra cache`，不是 `OBJECTS_STORAGE_CACHE`。不新增稳定错误码，也不把 `LBR-IO-001` 写给这条路径。

**Verification:**

- [ ] VER-1: `source .env.test && source .env.live-test && cargo nextest run --lib -E 'test(storage_config_hold_cache_) | test(storage_config_resolve_unresolved_type_stays_local)' --no-fail-fast --retries 0`。同一命令再跑一次，两次都通过。过滤器包含已有的 `storage_config_hold_cache_does_not_keep_rejection`、`storage_config_resolve_unresolved_type_stays_local`，以及 AC-4 的四例：`storage_config_hold_cache_skips_unresolved_type`、`storage_config_hold_cache_keeps_readable_unset_type`、`storage_config_hold_cache_keeps_local_word`、`storage_config_hold_cache_keeps_global_schema_future`。这些新测试放在 `client_storage.rs` 已有测试模块，使用 `#[serial(cwd, env)]`。`src/` 的 serial 行不进入 `tests/SERIAL_REGISTRY.tsv`，不手改 `.config/nextest.toml`。不把过滤器放宽到整个 `storage_config_`，以免带上与本卡无关的慢测试。

**Full-suite trigger:** 无。本卡不改公开签名、CLI 注册、稳定错误码、SQL、安装器或版本面。父卡 FIX-LTR-05 的全量仍留在父卡发布。

**Dependencies:** `FIX-LTR-05X`。

**Deliverables:** 未决 TYPE 客户端的 `retains_objects_cache == false`、对象缓存不插入该客户端、AC-4 的特征测试，以及 cache 与 fetch 文档里的对应句子。

**Implementation write set:** `src/utils/client_storage.rs`、`src/utils/util.rs`、`docs/commands/cache.md`、`docs/commands/zh-CN/cache.md`、`docs/development/commands/cache.md`、`docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md`、`docs/development/commands/fetch.md`、本计划、`docs/development/plan/plan-status.md` 仅本计划行。测试只加在 `client_storage.rs` 已有测试模块。不改 `tests/SERIAL_REGISTRY.tsv` 和 `.config/nextest.toml`。不改 `docs/error-codes.md` 和 `COMPATIBILITY.md`。不改网站。

**Release write set:** `N/A`

**Files likely touched:** 与 I 相同。

**Docs and compatibility impact:** 见 AC-5。先中文后英文。`cache info` 的 JSON 字段和退出约定不变。fetch、pull、clone 的一次调用结果不变。

**Rollback mode:** `immutable-release`

**Migration and rollback:** 无 schema 迁移。已推送提交只以前滚修正。未决 TYPE 的这一次调用仍可能写出本地对象。用户修好配置后，下一次未缓存的调用重新解析。不自动重试，不删除已经写出的对象。

**Security and privacy:** 不新增含配置值的错误。既有解析 Warning 的文本不改，它今天就会带上解析器的错误。确认失败路径的 canary 仍只出现在设置名之外的拒绝句子里。测试 canary 用合成值 `gcs-canary`。

**Performance budget:** 保持 200ms 配置库锁上限。未决 TYPE 不插入缓存。类型一直读不到时，每一次 `objects_storage` 都会再解析一次并再次打印既有 Warning，包括 `Tree::load`、`Commit::load`、`Blob::load` 各自的调用。这是每次调用读一次，不是重试循环，也不延长锁。本卡本地验证一次 3600 秒，含编译。

**Estimated scope:** `S`

**Version increment:** `N/A`

**Release boundary:** `family child`。不 bump、不 tag。已批准的 `v0.30.88` 仍只属于 FIX-LTR-05。

**C/D coverage from:** `FIX-LTR-05`

**Granularity:** `type=implementation; axis=未决的对象存储类型不进入对象缓存; recovery=immutable-release前滚; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=2; prod-files=2; scope=S; deps=FIX-LTR-05X; writeset=序列化于FIX-LTR-05之前; release=family child; split-from=FIX-LTR-05; exception=N/A`

两个落点是：未决且不是 `GlobalSchemaFuture` 的 TYPE 读取把客户端标成不进入对象缓存，以及 `cached_objects_storage` 看到这个标记后不插入。两个生产文件是 `client_storage.rs` 和 `util.rs`。签名不变。这一次调用的可写本地结果仍在，所以 FIX-LTR-05 仍 blocked，本卡不消耗 v0.30.88。

### Task FIX-LTR-05Z: 未决的对象存储类型不再构造本地存储

**Task type:** `implementation`

**Lifecycle / Acceptance:** `in-progress` / 空（ER03 `272c8d7d-255b-4e8d-9c2c-bf6129512723` PASS。实现审查 `4a42ec63-fb88-4cb8-af8e-d15fd43d487f` PASS。VER-1 两次都是 8 通过，VER-2 是 3 通过。不消耗 v0.30.88。不开始 FIX04。FIX-LTR-05 自己的 VER-1 至 VER-5、bump 和发布仍未做，所以父卡仍 blocked。本卡无独立发布）

**Description:** 单一行为轴是：`LIBRA_STORAGE_TYPE` 这一次没有读到，而且原因不是 `GlobalSchemaFuture` 时，这次调用不得构造可写的 `LocalStorage`。`open_after_type_read` 对这种错误返回 `Err(StorageConfigFailure)`，变量名只有 `LIBRA_STORAGE_TYPE`。`ClientStorage::init` 因此得到已经存在的拒绝状态。`resolve_configured_storage` 对同一种错误返回 `Err`。不打印 `Warning`，不保留底层错误或路径。`GlobalSchemaFuture`、`Ok(None)` 和正好 `local` 仍是今天的可写本地结果。

**Out of scope:** 不改 `GlobalSchemaFuture` 的 Warning、本地结果或可缓存性。不改可读的未设置类型，包括全局配置库文件不存在时的 `Ok(None)`。不改正好是 `local` 的历史 Warning，也不改 `historical_local_word` 里 bucket 读失败的本地结果。不改已经确认的 `gcs`、`S3`、空 bucket、空 key、空 secret 和非法 endpoint。不改 `exist`、`search`、`is_object_type` 的签名，不返回 `false` 或空 `Vec`。不新增 `Storage`，不使用名字 `try_init`。不改 `storage_config_resolution_fallback` 的文本。不第二次读 TYPE。不改 `resolve_cache_config` 和 `libra cache info`。不把 fetch 的检查移到发现远程之前。不给损坏的仓库库新增 fetch 命令测试，因为 `resolve_prune_mode`（`fetch.rs:1593`）早于存储检查（`fetch.rs:2524`），那条命令失败不能证明这条臂。不 bump，不打 tag，不消耗 v0.30.88，不开始 FIX04。本卡不跑父卡的完整 VER-1 至 VER-5，也不宣称父卡发布完成。不改 ingress、reasoning helper、MCP worker。网站 `cf` 不能快进且有无关脏文件，本卡不改网站，也不 force-push。

**Current evidence:** 具名决定是本轮评估指令，UTC 2026-10-26 12:51:00 UTC：未决的 `LIBRA_STORAGE_TYPE` 必须失败关闭，这一次调用不得有可操作的 `LocalStorage`。`open_after_type_read` 的 TYPE `Err` 臂在 `src/utils/client_storage.rs:1762`。它调用 `storage_config_resolution_fallback`（`:1710`），然后对非 `GlobalSchemaFuture` 把 `retains_objects_cache` 设为 `false` 并返回 `Ok`（`:1771`）。因此这一次 `put` 仍能写入。`init`（`:1646`）把 `Err` 变成拒绝状态。`cached_objects_storage` 在 `src/utils/util.rs:1393`，拒绝状态已经不插入。`resolve_env_sync_typed`（`client_storage.rs:3376`）先看进程环境变量。`break_repository_database`（`:7086`）把仓库库换成目录。`storage_config_resolve_unresolved_type_stays_local`（`:7304`）仍断言 `Ok`。`storage_config_unresolved_fetch_stays_local_fetch`（`tests/command/fetch_test.rs:3539`）以及 pull、clone 的同名测试只把 `LIBRA_CONFIG_GLOBAL_DB` 指到不存在的文件，仓库库仍可读，所以是 `Ok(None)`，不是这条 `Err`。`COMPATIBILITY.md` 的 fetch 行写着未设置且配置库读不到时仍走历史本地 fetch，并写 `init` 的旧降级不变。

**Acceptance criteria:**

- [ ] AC-1: `open_after_type_read` 的 TYPE `Err` 臂先区分 `GlobalSchemaFuture`。这种错误仍调用原来的 `storage_config_resolution_fallback`，仍返回 `Ok` 的可写本地客户端，并且 `retains_objects_cache` 不再存在后仍会被 `cached_objects_storage` 插入。其它 `Err` 不调用 fallback，不构造 `LocalStorage`，不打印 `Warning`，返回 `Err(StorageConfigFailure { variable: "LIBRA_STORAGE_TYPE" })`。判断使用已经拿在手里的错误，不再读 TYPE。`Debug`、`Display` 和 `Error::source` 不带底层错误、路径或配置值。
- [ ] AC-2: 删除 `retains_objects_cache` 字段、它的构造赋值和 `retains_objects_cache()`。`cached_objects_storage` 只在 `configuration_variable().is_some()` 时跳过插入。未决 TYPE 经 `init` 变成拒绝状态，所以本来就不会插入。没有重试循环。
- [ ] AC-3: 特征测试调用真正的 `objects_storage` 和 `resolve_configured_storage`。准备是 `isolate_storage_env`、新仓库、先保存仓库库字节、再 `break_repository_database`、以及指向不存在文件的 `LIBRA_CONFIG_GLOBAL_DB`。`objects_storage` 之前把 `STORAGE_TYPE_READS` 置 0。调用后计数是 1，`configuration_variable()` 是 `Some("LIBRA_STORAGE_TYPE")`，缓存没有 `path::objects()`，`put` 的 `Display` 正好是 `failed to read storage configuration for LIBRA_STORAGE_TYPE`，并且不含仓库路径。再把计数器置 0，对一个尚不存在的侧路径调用 `resolve_configured_storage`：计数是 1，结果是 `Err`，变量名是 `LIBRA_STORAGE_TYPE`，该路径不被创建。然后删掉充当数据库的目录，把保存的字节写回去。下一次 `objects_storage` 的 `configuration_variable()` 是 `None`，缓存含有该路径，`put` 成功。这证明拒绝状态没有被钉死。子进程覆盖没有 `Warning`：父进程准备同一个损坏仓库，子进程的工作目录是该仓库，`HOME` 和 `LIBRA_CONFIG_GLOBAL_DB` 都落在临时目录，模式 `unresolved_put` 在现有 `storage_config_hold_probe` 里于原有 `ClientStorage::init` 之前返回。子进程调用 `objects_storage().put`，只有 `Display` 等于上面的句子才退出 0。父进程断言退出码 0，stderr 不含 `Warning`，也不含 `Falling back`。`storage_config_hold_cache_skips_unresolved_type` 改名为 `storage_config_hold_unresolved_type_is_not_local`，不再断言可写，也不再读取已删除的字段。`storage_config_resolve_unresolved_type_stays_local` 改名为 `storage_config_resolve_unresolved_type_is_not_local`，改为断言 `Err` 和侧路径不存在。另有一例把全局库文件写成无法打开的内容，仓库库仍可读：`objects_storage` 同样是拒绝状态。子进程不使用 `spawn_hold_probe` 的空目录。`storage_config_hold_cache_keeps_readable_unset_type`、`storage_config_hold_cache_keeps_local_word` 和 `storage_config_hold_cache_keeps_global_schema_future` 去掉字段断言，仍断言缓存含有路径。可读未设置的那例在随后设上 `gcs-canary` 时，下一次 `put` 仍成功。
- [ ] AC-4: `docs/commands/cache.md`、`docs/commands/zh-CN/cache.md` 和 `docs/development/commands/cache.md` 改成：读不到 `LIBRA_STORAGE_TYPE` 且原因不是 future global schema 时，对象命令失败，不构造可写本地后端，只报告设置名，也不进入进程内对象缓存。这包括仓库库打不开、全局库文件存在但打不开或解密失败，以及解析线程没有返回。修好配置后的下一次调用会重新读取。future global schema、正好 `local`、以及可读的未设置类型仍是本地结果，并且仍可缓存。`docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md` 和 `docs/development/commands/fetch.md` 把“配置库读不到仍走历史本地路径”拆开：可读的未设置，包括全局库文件不存在，仍走历史本地路径；仓库库存在但打不开，或解析没有返回，则 `fetch`、`pull`、`clone` 以 `LBR-IO-001` 失败，只报告 `LIBRA_STORAGE_TYPE`，并且不安装 pack。`ClientStorage::init` 使用同一规则。文档和 `COMPATIBILITY.md` 不把这个类型写成 `libra init`。`COMPATIBILITY.md` 的 fetch 行把 “An unset `LIBRA_STORAGE_TYPE` whose configuration database cannot be read keeps the historical local fetch. Unsupported type values such as `gcs` fail the same way. `init`'s older local fallback is unchanged.” 换成与上面相同的区分，并写明 `init` 对这种未决读取不再构造可写本地后端。不改 cache 行，它描述的是 `libra cache`。不新增稳定错误码。

**Verification:**

- [ ] VER-1: `source .env.test && source .env.live-test && cargo nextest run --lib -E 'test(storage_config_hold_unresolved_) | test(storage_config_hold_cache_) | test(storage_config_resolve_unresolved_type_)' --no-fail-fast --retries 0`。同一命令再跑一次，两次都通过。过滤器包含 `storage_config_hold_unresolved_type_is_not_local`、`storage_config_hold_unresolved_type_stderr_has_no_warning`、`storage_config_resolve_unresolved_type_is_not_local`，以及三例仍可缓存的 `storage_config_hold_cache_keeps_*` 和已有的 `storage_config_hold_cache_does_not_keep_rejection`。新测试使用 `#[serial(cwd, env)]`。`src/` 的 serial 行不进入 `tests/SERIAL_REGISTRY.tsv`，不手改 `.config/nextest.toml`。
- [ ] VER-2: `source .env.test && source .env.live-test && cargo nextest run --test command_test -E 'test(storage_config_unresolved_fetch_stays_local)' --no-fail-fast --retries 0`。fetch、pull、clone 三例都通过，退出码 0，输出不含存储配置错误。它们覆盖的是全局库文件缺失，不是本卡的 `Err`。

**Full-suite trigger:** 无。本卡不改公开签名、CLI 注册、稳定错误码、SQL、安装器或版本面。父卡 FIX-LTR-05 的全量和 VER-1 至 VER-5 仍留在父卡发布。

**Dependencies:** `FIX-LTR-05Y`。

**Deliverables:** 未决且不是 `GlobalSchemaFuture` 的 TYPE 读取返回 `StorageConfigFailure`、删除只为可写降级服务的缓存标记、特征测试，以及 cache、fetch 和 `COMPATIBILITY.md` fetch 行。

**Implementation write set:** `src/utils/client_storage.rs`、`src/utils/util.rs`、`docs/commands/cache.md`、`docs/commands/zh-CN/cache.md`、`docs/development/commands/cache.md`、`docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md`、`docs/development/commands/fetch.md`、`COMPATIBILITY.md` 仅 fetch 行里上述那句、本计划、`docs/development/plan/plan-status.md` 仅本计划行。不改 `src/command/fetch.rs`、`pull.rs`、`clone.rs`。不改 `tests/SERIAL_REGISTRY.tsv` 和 `.config/nextest.toml`。不改 `docs/error-codes.md`。不改网站。

**Release write set:** `N/A`

**Files likely touched:** 与 I 相同。

**Docs and compatibility impact:** 见 AC-4。先中文后英文。`cache info` 的 JSON 字段和退出约定不变。可读的未设置类型在 fetch、pull、clone 上的成功结果不变。

**Rollback mode:** `immutable-release`

**Migration and rollback:** 无 schema 迁移。已推送提交只以前滚修正。未决 TYPE 不再写出新的本地对象。`ClientStorage::init` 的调用者，包括 commit、cat-file、show 和 hooks，在仓库库锁超时这类暂时读不到类型时，这一次调用也会失败关闭，不再改用本地后端继续写。用户修好配置后，下一次未缓存的调用重新解析。不自动重试，不删除已经存在的对象。

**Security and privacy:** 错误文本只有变量名。不把解析器的路径或底层错误写进 `Display`、`Debug` 或 stderr。测试不使用真实凭据。

**Performance budget:** 保持 200ms 配置库锁上限。拒绝状态不插入缓存，所以下一次调用会再解析一次。这不是重试循环。本卡本地验证一次 3600 秒，含编译。

**Estimated scope:** `S`

**Version increment:** `N/A`

**Release boundary:** `family child`。不 bump、不 tag。已批准的 `v0.30.88` 仍只属于 FIX-LTR-05。本卡的审查通过之后才允许提交。提交本身不发布 v0.30.88，也不开始 FIX04。

**C/D coverage from:** `FIX-LTR-05`

**Granularity:** `type=implementation; axis=未决的对象存储类型不再构造本地存储; recovery=immutable-release前滚; complete=yes; self-contained=yes; AC=4/8; VER=2/8; landing=2; prod-files=2; scope=S; deps=FIX-LTR-05Y; writeset=序列化于FIX-LTR-05之前; release=family child; split-from=FIX-LTR-05; exception=N/A`

两个落点是：非 `GlobalSchemaFuture` 的 TYPE 读取错误返回 `StorageConfigFailure` 且不构造 `LocalStorage`，以及删掉因此不再需要的 `retains_objects_cache` 标记。两个生产文件是 `client_storage.rs` 和 `util.rs`。`fetch.rs` 已把 `StorageConfigFailure` 映射为 `LBR-IO-001`，本卡不改它。签名不变。父卡的发布门仍未跑，所以 FIX-LTR-05 仍 blocked，本卡不消耗 v0.30.88。

### FIX04、FIX02、FIX03 的接续细则（覆盖旧开工快照）

- **FIX04：** Dependencies现为FIX05完整C/D、SOURCE/PUBLISH，外加 DEP-QUAL-F2-CLOSE。依 2026-10-26T19:12:37Z 例外，CLOSE=unknown 不挡住这两行 cfg，P1 不关闭。从实际已发布main重新建源；仅两cfg与本计划/status行，不携带helper5025。原A1、源B、自己ER05仅在源与覆盖未变且原件可核时限域carry；失败C1的bump/lock、全量、release构建、安装不能继承。先登记当前恢复及新预算，Claude PASS后才claim；default与keyring release必须零项目warning，之后每卡继续零warning。版本只能在FIX05实际版本上patch+1。
- **FIX02：** Dependencies仍为FIX01、FIX04。只恢复 `tests/helpers/reasoning_artifact.rs` 的索引结清方案，不能stash-pop旧两份治理/旧bump；无stash原件时按卡片重建并新审新测。它的两个真实消费者、shared fixture全量触发、原timeout/断言均保留。A2/ownR2不是新源C/D；新版本是FIX04实际版本+1。
- **FIX03：** Dependencies仍为FIX02。先按现有卡解决worker PID/身份/PGID/父进程的完整teardown，不能按名字kill或把父wait当worker退出。完整MCP、full、doc、network原3例及ownClaude/C/D不省略；新版本是FIX02实际版本+1。F3等独立风险若阻断门，先另卡，不把手工清理当产品修复。
- **计划最终资格：** 五张实现卡分别完整C/D后，在FIX03最终发布merge/main树完成独立最终fmt/clippy/full/doc/network与实际Claude资格审查，再做原登记的终态文档提交及其真实D。用户在main开发时，原文「merge树=被测树」解释为实际main提交树=最终被测树；遇上游变化先整合重验，不用文档提交掩盖代码漂移。终态不得只停在本机安装成功。
- **受控整合：** 最后将这些实际发布提交与原scoped-traces未发布历史精确整合，逐文件保留COV/OG/SCAP等上游成果、签名/DCO和无关WIP；原7541e9ba/ed566b0相关合并签名政策与当前写者交接仍须具名处置。此步骤不等于直接推送原main的整条ahead历史。

### 原 scoped-traces 主线仍需的跨仓前置

本资格链闭合只满足 `DEP-TR-QUAL-REPAIRS`（目前登记在原工作区未发布治理中，远端main原计划尚无此边；受控整合前不得假定远端已登记）。LTR10另外需要 `DEP-TR-H0`、`DEP-TR-H1`、`DEP-TR-WRITERS` 和独立的 `DEP-TR-LIVE-SERVICE`；LIVE含持续服务owner、fixture寿命和可用性，并不由一次H1 PASS替代。Mega2剩余链为 `MTR11 → MTR40 → MTR13 → MTR39 → MTR92 → MTR12/H1`；此前MTR103已真实发布v0.42.75，但不等于H1。H1要记录真实服务SHA/配置摘要/端口/认证身份和owner，提供HTTP/SSH scoped Git、两身份/target权限及持续fixture；不得用内部模块PASS或普通服务已启动代替。之后按原计划 `LTR10 → LTR11 → LTR12 → LTR01/H2`，前3卡为原REL-LTR-PUSH子卡，LTR01才是唯一发布点；本main文档交接不扩大子卡push/bump/tag权限。

### Task DOC-LTR-02: 跨机器前置任务定义交接

**Task type:** `docs`

**Lifecycle / Acceptance:** `in-progress` / 空（本次用户明确要求，仅任务定义与迁机交接；不宣称FIX05准入）

**Description:** 将当前失败、FIX05及后继卡细则和未决授权完整写入本文件，单独signed/DCO提交并推送main供另一机器接收；保留所有旧失败/STOP及已消费预算。

**Out of scope:** 不实现产品、不改变版本/lock、不批准警告例外、不修改原scoped-traces或网站文件、不取消任何原卡的C/D。

**Current evidence:** 用户2026-10-26明确单文件/main交接指令；当前来源、原件hash和真实失败见本入口。本节是提交前任务定义快照；先冻结最终正文并执行门，随后真实本地Claude ER05，实际终态/session/输入hash写入同一signed提交的message及仓外原件，保持被测/受审/提交正文逐字一致。以下pending不冒充未来验收，最终状态由后继既定同步补记。

**Acceptance criteria:**

- [ ] AC-1: 前置任务、依赖、未决授权和迁机证据缺口均明确登记。
- [ ] AC-2: 原失败、STOP及预算历史保留，不写未来PASS。
- [ ] AC-3: 提交仅本文件，main父提交是现场核验的远端，签名和DCO真实核验。
- [ ] AC-4: 接收方可从远端main取得同一文件与提交。
- [ ] AC-5: 该main SHA实际base全部8个job及CodeQL两个job的run/job/attempt终态逐项核验；失败STOP，pending不计完成，现场workflow变化重核映射。

**Verification:**

- [ ] VER-1: 原生diff/逐文件hash确认唯一文档变更，结构、任务字段、依赖无环及相对链接检查通过。
- [ ] VER-2: 本地Claude审查当前全文与差异，明确 `VERDICT: PASS`；P0/P1关闭，P2具名处置。
- [ ] VER-3: `source .env.test && source .env.live-test && cargo nextest run --test compat_agent_architecture_guard --no-fail-fast --retries 0`，完整现有目标非零；文档消费者扫描若发现其它受影响target，先补门及预算。

**强制本地门：** `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`，加VER-3完整target；编译均计入本卡一次窗口。

**Full-suite trigger:** `N/A`（no-release、仅计划文本；自己的nightly fmt/strict Clippy及实际文档消费者不可免，产品完整C由FIX03覆盖）。

**Dependencies:** `DEP-QUAL-SOURCE`；本次用户明确单文件/main交接，不依赖未来FIX05完成。

**Deliverables:** 本文件的任务定义与跨机交接。

**Implementation write set:** 仅 `docs/development/plan/plan-20261025-qualification.md`。本次单文件要求优先于通常同批status行同步；后置状态由root既有协调写集或下一张实施卡补记，不带原status全文。

**Release write set:** `N/A`

**Files likely touched:** 仅上述一文件。

**Docs and compatibility impact:** 无用户命令/配置/协议行为变化，命令双语与网站N/A。

**Rollback mode:** `immutable-release`

**Migration and rollback:** main推送后仅前滚更正文档，保留原失败，不回退远端或改tag。

**Security and privacy:** 正文仅源码锚点/摘要/hash，不含env/token/私钥；仓库不新增证据目录。

**Performance budget:** 本地一次3600秒，包含nightly fmt、strict Clippy、文档消费者target和实际结果核对；编译计入，失败STOP。缓存/工具链在claim前确认；本次是新文档门，不重置旧FIX04窗口。具体执行器及输入先审查。

**Estimated scope:** `S`（1文档，无生产落点）

**Version increment:** `N/A`

**Release boundary:** `no-release`；按用户指令main独立文档交接，无bump/tag/Release；实际main base/CodeQL仍观察，未触发工作流记N/A。

**C/D coverage from:** `FIX-LTR-03`（自身文档门/main D另记；此前最多locally-accepted，文件已提交不等于产品或计划完成）。

**Granularity:** `type=docs; axis=跨机器前置任务定义交接; recovery=immutable-release前滚文档; complete=yes; self-contained=yes; AC=5/20; VER=3/20; landing=0; prod-files=0; scope=S; deps=DEP-QUAL-SOURCE; writeset=本文件独占; release=no-release; split-from=用户20261026迁机交接; exception=N/A`

### 2026-10-26 修订登记

本轮正式登记FIX05（仍blocked）与DOC02，新增DEP-QUAL-F2-ATTRIB/CLOSE及pending决定，明确FIX05→FIX04→FIX02→FIX03条件顺序、原计划H1等独立前置、main分工与单文件提交范围。原四实现卡统计1/4保留在历史段，当前是1/5；文档卡不计入实现卡分母。旧正文的feature PR、旧来源和已消费预算为历史执行记录；本入口覆盖当前接续要求。文档自身Claude/本地门/提交/main D必须待实际取得后记账，不能预填。


## 既有任务与历史执行记录

以下保留已有卡片及全部失败历史；旧来源、feature PR、已消费窗口及旧统计按其日期理解，当前接续要求见文首2026-10-26入口。FIX04 Dependencies/Granularity已补FIX05/F2边，原字段在末尾原样保留。

> **FIX04 C1实际退出终态（2026-10-25 18:59:44 UTC）：** FIX04 C1的精确退出已实际执行：本地Claude退出R1 session ff79d09e-3485-4fdb-9316-258cd5d0b614四项PASS、50输入重核；root采纳后，原生restore及外层工具18851于2026-10-25 18:57:47 UTC实际exit0。四个版本/lock逐字等于published b73/.87，四个0664模式保持，原生stash list确认988ded20及完整列表不变；七路径失败树已在持久私有位置按SHA256/MANIFEST及模式补充保存。两cfg及其余全部tracked blob/mode、原Mega9/publisher9/原Libra4路径WIP保持，index空，仅余本计划/本计划status行/ingress三个脏路径。这是退出执行事实，当前等待退出终态增量单审；不授任何新A/C、commit/push/tag/Release。FIX04原C1机械11阶段0但native审查四FAIL、root STOP保持，Lifecycle仍blocked/locally-accepted，仅旧A1/源审限域有效；storage错误回退P1、reset实际stderr不足暂定P1与其它具名风险尚未解决，资格1/4、原scoped-traces0/26不变。失败C1已无active bump；后续独立Mega102须在本次终态增量单审及自己的具体C2合同通过后才claim。
> 下方候选的“待审/未执行”属于原时点历史，不覆盖本段实际结果；本次增量审查尚未发生。

> **FIX04 C1真实FAIL/STOP与精确退出候选（2026-10-25 18:47:47 UTC，未执行）：** FIX04原C1于2026-10-25 18:10:38 UTC结束，11阶段exit0、私有安装.88及资源后检exit0，5348.666525/10800秒；外层工具96753实际exit0。完整10761身份/21原ignored保留，4个flake/5个失败尝试后native重试成功；实际本地Claude session c7f9fbae-734f-416c-9d9d-3650ede3f824的四项判定全部FAIL，root于18:17:28 UTC记录STOP，C不接受。F1 P1为storage配置读取错误后无条件LocalStorage回退并污染machine stderr；F2暂定P1为reset missing-path失败未保留实际stderr，根因未知；F3 stdin fixture race与F4未执行accept脚本列P2。所有旧FAIL/STOP、A1两flake、37运行时覆盖缺口及21 ignored保持，历史SQLite锁owner/SQL与C2数据库根因未证。本次仅候选RB-FIX04-EXIT-C1：先保存七路径失败树及SHA清单，再用原生Libra精确恢复本卡四个未发布版本/lock到原before-bump（逐字等于b73/.87），保持两cfg和其它全部源码、原stash988ded20、两仓原WIP。经本机Claude EXIT/ER12/H0/VERDICT PASS和root采纳才执行；不reset/force/stash-pop/commit/push，不新开A/C，不授.88再次发布。退出成功仅释放本次失败C1发布占用，FIX04仍blocked/locally-accepted，资格1/4、原scoped-traces0/26不变。后续F1/F2/F3须先审定合法独立修复与依赖；不得把无G13的本仓改成聚合，也不借G08归并独立缺陷。Mega102自己的ER05已PASS，独立C2仍需退出终态与自身具体执行合同PASS。
> 下方日期段均保留原时点事实；本段只提出退出，不把未执行动作写成完成。

> **FIX04自身审查实际终态（2026-10-25T14:24:41.918607+00:00 root采纳）：** 本地Claude `e76e3d02-d544-4259-9f2b-42a937cc49e9` 于14:22:16.368832Z给出ER05/H0/VERDICT三PASS，P0/P1=0；root实算73输入并承接五项P2。源码仍仅两cfg，A1原2个native flake、37个runtime覆盖缺口和1个被断言负例原样保留，未证明baseline/历史根因。最终C执行审查、bump与全部发布C/D仍待，资格1/4、scoped-traces0/26不变。下方日期横幅和A1时点pending判断保留为历史，不覆盖本段实际终态。

> **FIX04实际A1/B与资源终态（2026-10-25 14:16:54 UTC）：** fmt/strictClippy、capture436、architecture65、default/keyring release、full10761/21原ignored已核；2个原生flake和当前runtime覆盖缺口保留，自己的ER05及最终C/D待。资格1/4，未bump/提交/推送；下文旧时点“未claim/待执行”均保留为历史。

> **FIX04实际恢复与实现入口（2026-10-25T12:28:29.801459+00:00）：** 本地Claude恢复R2 session 15ccf47d-7c29-42d1-be62-3515b59119ed ER03/RECOVERY/H0/VERDICT全部PASS，manifest 9fcb72036655861f6cf82c2300ab292ba507d8e878973b6fcca9b845e67ecc96、output e32e43f55b23e038ffc712ba84b00442151f5c58a3988015452c22fc72780121；P0/P1=0，P2 owner=root：审后实际收据入本段、D1终态/资源/自身审查采纳逐hash绑定、旧横幅保留为历史、keyring无release缓存须具体执行器审预算、18个远端job为root实际gh只读观察。按实际native命令完成自有.88四面精确退出为.87；FIX02三个WIP保存在stash 988ded20adb6be7ebbc43ad195fd55768ed69a26，逐blob核对且保留未pop/drop。新branch fix/capture-ingress-profile-20261025来自b73，仅ingress两cfg及两治理；helper恢复published b73字节。21份物理H0文件按受审候选同步。两次只读原生参数前检失败和stash后只读ref检查失败保留，未触发A/C；不重复stash/退出。当前源码已实现，A/B尚待具体执行，自己的ER05、C/D、提交/推送/Release均未完成；版本0.30.87。

> **恢复R2候选（2026-10-25T12:23:22.622751+00:00）：** 保留R1 H0 FAIL；补完整D1终态/自身审查采纳条件、精确源hash、前驱job级收据与四卡warning门。R2待实际Claude；不授新A/C。

> **FIX02 C1 STOP后新恢复候选（2026-10-25T12:03:44.446299+00:00，未采纳）：** Source审ce6fb162已证release profile根因，RECOVERY FAIL的两P1由本候选“FIX04唯一生产I特许+实际A release警告门”修订，完整ER03/H0待。当前顺序FIX01(done)→FIX04(pending)→FIX02(blocked/locally-accepted)→FIX03；四实现卡1/4，DOC-LTR01仍仅locally-accepted等FIX03覆盖，原scope-traces0/26/H1未解锁。以下旧时点PASS/FAIL/STOP与统计全保留，不倒改。


### FIX01与DOC已发生的发布证据（R2补记）

FIX01原D terminal SHA256 `4e2009fab779281e2cb2efbb09aa94ab28d0b619288c34341bca0522757c6cf9`，观察2026-10-25T05:20:51.187280Z；merge f33be958/tree90ca1d20、签名/DCO、v0.30.87和渠道二进制manifest校验均已在该原件实核。本轮重新只读取得以下job原件，仓外FIX01-job-receipts-R2.json SHA256 `c66879aa74f29da560d3fe438a4b62adb473fb3156487955e1417da94a0f8fe8`。

| workflow/run | job/id | 实际结论 | 完成UTC |
|---|---|---|---|
| main-base/40156941519 | compat-offline-core/121060210750 | success | 2026-10-25T05:05:07Z |
| main-base/40156941519 | compat-rustfmt/121060210778 | success | 2026-10-25T03:07:03Z |
| main-base/40156941519 | compat-scoped-input-windows/121060210810 | success | 2026-10-25T03:17:27Z |
| main-base/40156941519 | compat-owner-liveness-macos/121060210816 | success | 2026-10-25T03:43:09Z |
| main-base/40156941519 | compat-redundancy/121060210836 | success | 2026-10-25T03:06:11Z |
| main-base/40156941519 | opencode-export-sandbox-linux/121060210851 | success | 2026-10-25T03:08:43Z |
| main-base/40156941519 | compat-clippy/121060210918 | success | 2026-10-25T03:10:44Z |
| main-base/40156941519 | compat-network-remotes/121060210942 | success | 2026-10-25T03:30:46Z |
| main-CodeQL/40156941508 | security-codeql-actions/121060210278 | success | 2026-10-25T03:06:41Z |
| main-CodeQL/40156941508 | security-codeql-rust/121060210465 | success | 2026-10-25T03:38:17Z |
| release/40157032761 | build-and-upload (libra, x86_64-pc-windows-msvc, windows, amd64, windows-latest)/121060495046 | success | 2026-10-25T03:30:45Z |
| release/40157032761 | build-and-upload (libra, aarch64-apple-darwin, darwin, arm64, macos-latest)/121060495166 | success | 2026-10-25T03:28:24Z |
| release/40157032761 | build-and-upload (libra, aarch64-unknown-linux-gnu, linux, arm64, ubuntu-24.04-arm)/121060495174 | success | 2026-10-25T03:19:00Z |
| release/40157032761 | build-and-upload (libra, x86_64-unknown-linux-gnu, linux, amd64, ubuntu-latest)/121060495177 | success | 2026-10-25T03:22:18Z |
| release/40157032761 | update-homebrew-tap/121064822971 | success | 2026-10-25T03:30:55Z |
| release/40157032761 | upload-install-scripts/121064823276 | success | 2026-10-25T03:31:01Z |
| release/40157032761 | request-stable-manifest/121064823359 | success | 2026-10-25T03:31:02Z |
| release/40157032761 | verify-homebrew-formula/121064852609 | success | 2026-10-25T03:31:22Z |

DOC PR636原merge收据05:42:10Z为b73e98a/tree73eba0d7；main-terminal-audit于06:48:23Z核base八job与CodeQL、network原3例及10761/21原ignored/0retry，项目warning0。其自身A/B/Claude及signed/DCO已发生，仍locally-accepted等FIX03覆盖；旧未提交/待main横幅只属历史，不回改。

### 新FIX04预算依据与R1处置

FIX01真实C release-build于2026-10-24T23:09:42.190321Z至23:31:27.838239Z，耗时1305.648秒；FIX02 C1从10:09:46.616474Z claim到11:24:46.666098Z进入release已4500.050秒，release只运行约551秒即因实际项目warning被停止，不能把该部分当完整build耗时。新FIX04沿用已有target、jobs=1：估算前序门4500秒、default release保守1800秒、keyring增量保守1800秒、后检180秒，约8280秒低于候选10800秒；这些是claim前可否准入的估算，不是实测保证。具体执行器审查必须核同toolchain/target/profile缓存及两release重编译范围；依据不足先ER03，不在claim外预编译、不延长已消费窗口。

R1真实ER03/RECOVERY PASS、H0/VERDICT FAIL（sessionbb82411c-1e59-4e52-a7f1-930ab10005b8，output c9b9e57128d488b10393ff21be13ccbd1da5107231e486965780f0533c1413b1）保留。P1-H0由上面完整终态+采纳收据条件修订；D1现已实际GO/资源closed，ownClaude06c202a4 PASS，root于12:22:25Z采纳并独立重核stdout/编译诊断。G13仍pending。P2 owner=root：以candidate.json及before-R2保存像为权威、生成差异本脚本留存，不重跑旧生成器；ingress锁定精确SHA；FIX01 job收据与DOC当前状态已补；四卡措辞及全局warning门已补；预算按上述claim前实审；原Libra状态旧资格行由下一次原scope治理承载同步，其带日期旧快照不倒改。R2尚待实审，未恢复版本/stash/branch/实现/claim。

## 本轮受审恢复动作与来源边界（未授执行）

本候选以实际published b73的两个blob为底重建，不复制stash中旧WIP全文。仅选择可核的FIX01实际C/D、FIX02 A2/own/C1 STOP事实入文；FIX04是新的独立代码轴。FIX01完整收口原件root-card-closeout于05:20:51Z记done/complete，源审/C/签名/PR635/merge f33be958/v0.30.87与全部D实核；旧release warning P2作为历史保留。DOC PR636/main已实际成功，但本no-release卡完整C/D仍继承FIX03，不提前done。

计划PASS后才可执行以下精确恢复。入场先读新远端，main若不是b73或v0.30.88的tag/Release已被占用则STOP/ER09重审，不覆写。实核qualification为原生独立clone .libra实目录/no.git、HEAD/origin b73、branch fix/qualification-index-drain、index空、恰七dirty/无新untracked、源/工具身份与C1保存像一致；无他人写者。已有七路径failed-C-tree和C1 final-inputs/tree7388378均保存。

1. 逐一核C1的四个before-bump文件hash等于b73的Cargo.toml/Cargo.lock/install.sh/install.ps1 blob、当前四文件等于失败C1最终hash/mode；保存原件后只write这些文件的原始字节回各自前像并保持既有mode。Cargo.lock是完整已核原件恢复，不手改其版本行；不reset、改远端或他人source。核四面.87 parity与helper5025/两WIP文档字节不变。
2. 用原生 `libra stash push -m "FIX-LTR-02 preserved before profile-warning repair" -- tests/helpers/reasoning_artifact.rs docs/development/plan/plan-20261025-qualification.md docs/development/plan/plan-status.md` 只保这三个own WIP；先核暂存空，后核stash实际object/ref/tree恰含三路径delta且helper5025可回取、七保存像完整、工作区clean。保留stash，不pop/drop。目录、新原件与对象不视为窗口重置。
3. `libra switch -c fix/capture-ingress-profile-20261025 b73e98a77ecc7c3db1ea905ab55478785d76c041`；只应用本受审候选两个治理文件与ingress两cfg；ingress最终字节必须等于source审candidate-ingress.rs SHA256 `2b7aa33698c2072d160cc6bea7801ffb8fb180a13bb0ebe78901ff888b7bb778`。完整其它tracked blob/mode必须等于b73，尤其reasoning_artifact.rs等于published blob，不能含5025 WIP；无ed566b0或原未发布Traces治理祖先/源。无force/reset。新branch与唯一root publisher用既有独立clone/target，不创建共享Libra worktree。
4. A/C分别只能在本机现行Mega D1实际终态/资源后检后、具体执行器单审PASS/实核toolchain/env/暖cache后唯一claim。本恢复不授FIX02 C2，不把已耗5051秒视为可用余额。FIX04实际C/D后，先精确前滚整合到新上游，再只恢复helper保存像，两个重叠治理以新published字节为底三方重建；禁止pop旧文档/旧bump。之后FIX02新的ER03决定原A2/own的有限carry及新版本/新C，不预填PASS。

两原日期计划/SSOT/索引只做本地H0同步，仍由既定MTR104候选合法承载点/原LTR01覆盖，不能随FIX04推送原未发布Traces治理。当前Mega D1已于11:39:55Z真实claim，冻结的原三Mega与原Libra三治理不改；本轮九逻辑治理H0候选只能在D1协调器实际退出、资源closed，且D1自身review（原工具句柄24409）已有实际终态并完成root采纳/收据之后，按当时before hash逐项匹配采纳；任何不匹配先重建overlay并重审。A/C Cargo与发布仍root全局串行，stage/commit精确，不用commit-a/无admin bypass。新的恢复方案不放宽任何版本、测试、Review或远端D门。

> **2026-10-25 04:25:21 UTC 当前：** DOC-LTR-01源码已采纳、A/B实际通过（48命令、22组help相同、874.796秒），当前`in-progress/locally-accepted`；自己的本地Claude ER05、提交与PR/main D待，完整覆盖仍由FIX03承担。FIX01已合并f33be958并发布v0.30.87，release/CodeQL实际成功，main base仍待；三实现卡0/3。以下旧快照保留。

> **2026-10-25T03:14:21.976945+00:00 PR635实际合并/发布触发与文档修复候选：** FIX-LTR-01本地C所需门/完整默认10761、release build/install已核；签名/DCO head2852497、PR635全部13 checks成功后gh squash merge `f33be958dcc3266df3a6ab796ed90d1766f3cd93`，树90ca1d20与最终C相同、GitHub签名/DCO已核；人工Release/tag v0.30.87已触发release run40157032761，main base40156941519及CodeQL40156941508仍待，未done。原attempt1 eli-MI fontconfig失败与本地retry/资源缺口保留；用户移除该runner，attempt2在Mechaevo成功，非证明eli-MI已修。用户要求Libra下一提交只修104条项目Rustdoc warning，明确排除第三方proc-macro-error2。DOC-LTR-01候选见文末；三张实现修复仍0/3，本新增文档卡独立记账、由FIX03覆盖。以下旧时点记录均保留。

> **2026-10-24 21:42:59 UTC FIX-LTR-01本地A实际终态：** 独立clone的唯一结构oracle修复已通过nightly fmt、strict all-target/all-feature Clippy、exact1/1、完整target54/54及默认全量10761/10761的框架核验，exit均0；21原ignored不变，无失败/flake，3962.768/7200秒、2126输入无漂移。输出38个runtime skip标记已逐身份/逐文字归属，其中1个是缺interpreter的负向断言，37个是既有feature/live/工具/平台覆盖缺口，不能据此宣称所有live行为已测。唯一遗留worker归属当前status-pause无helper用例，已核PID/start identity并通过pidfd人工SIGTERM，确认exit/reap；原资源缺口保留，FIX03未修。A仅本卡locally-accepted，自己的真实Claude ER05、patch/C/D均待；版本仍0.30.86、无提交/push。三卡正式0/3，FIX02/03 pending。以下旧时点事实保留。**

> **2026-10-24 20:23:39 UTC FIX-LTR-01实施入口：** 本计划工作副本位于独立原生clone `/media/eli/data/libra-qualification-20261025`，branch `fix/qualification-schema-doctor`，基线0ca47cc。FIX11实际done/complete且两个告警fixed；恢复R5方案/治理审查PASS。现仅改既有结构字串加入Ui排除，生产CLI、runtime断言和相邻startup guard不变。A未启动、自己的代码ER05未取得；没有bump/提交/push/Release/CI新结果。外部修复仍0/3。下方原时点快照保留。

> **FIX-MTR-11实际收口（2026-10-24 20:22:25 UTC）：Dependabot23/24已由GitHub实际标fixed，fixed_at分别2026-10-24T20:16:51Z/20:16:52Z，均非dismissed；main与v0.42.70解引用均932731d9b7f274638d2fb1bfa10d571207a54a39，人工Release已发布，main Harbor40112708799及tag Docker40112738161三job全部success。自己的Claude R3 ER05/H0 PASS、21 targets/3085 PASS/0 FAIL/3原ignored的五门C与signed/DCO均已核，现done/complete；RED1 FAIL/STOP与原P2不改。原件D-alerts-fixed-terminal-20261025 SHA256 2585fecfeacde5f37a6bae1eed701898e20f8254e11d704cc08e017129a0e7f9。Mega15/112（活动15/111）、Libra0/26（活动0/25）、联合15/138（活动15/136）；外部恢复0/3。恢复方案R4及治理自身R5真实本地Claude均PASS。安全前置已解除；下一步优先在独立Libra clone核入场并实施FIX-LTR-01，MTR101亦仅解除本安全前置，不跳过各卡独立门。原Libra merge签名政策、H1/LTR10及LTR01仍未完成。**

> **2026-10-24 20:15:32 UTC 治理采纳与自身审查实际收据：** 九份文档已按受审hash采纳；真实本地Claude R5 `DOCS_ER05_VERDICT / ER03_VERDICT / H0_SYNC_VERDICT / VERDICT: PASS`，P0/P1=0，session `e72fd9de-c479-457d-9eff-71ea07a7b49d`，审查输出SHA256 `cd632543ed9722003ebdda14f6e6f32512eb84f7b8e63e0940ce13a96291d4b7`，manifest SHA256 `096eae5326d3714271eaa2c4c3ba59dfa1f7c1156b6aa3d48d8bdf5c7f0fa78a`。本行是审查之后的事实收据，不声称审查器读过其自身后产出的输出。root接受非阻断P2：例行维护收据/预算约束以下方具名表为准，执行前并入对应条款并实审；历史横幅保留，本文首收据为当前时点；终态执行器仍须生成/冻结/实审。缓存复制实际exit0，31147文件（deps11178/build4563/fingerprint15406）、19035122524字节，root已核排除第一方/测试可执行文件及硬链接；这只完成源准备，不是验收。两仓精确signed本地提交待，尚未push；原治理由既定MTR01/LTR01承载。三张修复仍pending、0/3；原14/138不变；FIX11两告警实际fixed及逐卡入场条件未满足前不实施，不复用任何旧FAIL/STOP窗口。

> **2026-10-24 20:09:49 UTC 当前审查状态：** 恢复方案R4真实本地Claude ER03/H0/VERDICT均PASS，P0/P1=0；原R1～R3 FAIL保留。本文现为P2事实修订后最终治理候选，自己的DOCS-ER05/H0审查待取得；三卡仍pending/未实施、未测试、未提交、未发布。

# Libra 2026-10-25 来源资格前滚修复

> ER-03 候选，尚未采纳、实施或开启测试窗口。依据 [plan-template](plan-template.md) 与仓库 AGENTS.md；按用户当前指令只使用真实本地 Claude CLI 审查，不运行隔离 Codex。本文当前承接下列四个修复行为轴（新增FIX04为本轮待审候选），不替换 `plan-20261015.md` scoped-traces 计划（原工作区现行未发布治理另行承载）。

## 目标与来源

恢复选定上游 `0ca47cc147b2099ffb835ade91272c4cfe1efd21` 的真实来源资格，保留新 UI 行为和全部原测试断言。原工作区 HEAD `ed566b0229dfe2805a2317f889355e449f7d4e77` 是外部合并，包含未发布治理且没有签名/DCO；本计划不改写它、不替它补造签名，也不从该 HEAD 推送修复。

原首次资格已 FAIL/STOP：coordinator exit1、native full exit100，2026-10-24T19:12:05Z；4982.973/7200秒，非超时。10761 eligible 身份全部到达，10760最终通过、1最终失败、21 source ignored、1 flaky success；实际失败尝试4次（3+1），最终报告重复的失败标题不重复计数。fmt/strict Clippy/build 已 exit0；doc/network 未开始。2125输入未漂移。原件在仓外 `libra-upstream-qualification-20261025/r2`；terminal SHA256 `7f1a5258024676525210f3cd6a48c834d8450dd209a6251d3b25348481fc02a6`，full JUnit `e9f08229091d55e028307cbe1d98f71b477b63299be7110532f564ee51a8c5fa`。现存 proc-macro-error2 future-incompatibility 提示仍是具名风险，不能称零诊断。

实际本地 Claude 失败处置审查：session `a87e189c-332a-4dd0-9db9-c412cc81e314`，2026-10-24T19:20:05.975264Z，exit0、FAILURE_VERDICT/VERDICT PASS、无漂移，输出 SHA256 `7e9531e8736c8d67b9e83ea17b954885f3694dc918ae9047b9bdd5c357ba4edf`。它只认可失败记账与三个独立修复方向，不是产品/资格 PASS，也不是本计划的 ER-03 准入。

### 独立发布与原工作区保护

- root 是唯一写者和串行发布者。使用真正独立的原生 Libra clone（不是共享 HEAD/index 的 worktree），从现场核实的 origin/main 建立短期 feature branch。先检查类型、HEAD、远端 ancestry、工作区/索引、并发写者、身份和 vault 签名；若上游变化，先审差异并重核本卡，不覆盖新增提交。
- 每卡独立 patch、A/B、实际 Claude ER-05、C、PR、merge、tag/人工 Release 和真实 D；不建立 G-08 家族或 G-13 聚合，不把测试修复塞进 LTR-01。未来卡的实现不得混入前一卡提交。
- 修复 checkout 的公开写集仅本计划、基于实际已发布上游版本的 `docs/development/plan/plan-status.md` 中本计划登记行、当前卡已登记测试文件、该卡实际版本面/工具链 lock；FIX-LTR-05的生产/测试/文档候选写集只按文首VER-6后冻结的正式I准入；FIX-LTR-04额外允许 `src/internal/ai/capture/ingress.rs` 两个方法定义前各加一行 `#[cfg(any(test, debug_assertions))]`，不改其方法体或生产入口。与原工作区未发布 scoped-traces 治理提交做 ancestry 和逐文件 diff 守卫，禁止携带 `ed566b0`、其本地治理父链或原三份治理 WIP；plan-status 只在上游版本中新增/更新本计划行，禁止复制原工作区整份文件。本文是新的独立修复计划，不复制未发布的 scoped-traces 文本到发布分支。
- 原工作区两仓既有八份治理文件只登记事实/外部依赖，仍由原 MTR-01/LTR-01 与 Mega 后续既定 C/D 承载；其中原 scoped-traces 治理增量不属于五张实现卡的发布写集；plan-status 的新修复计划行是下述独立同步写集。新修复发布后，原工作区只通过原生前滚整合保留双方历史；若本文本地未跟踪副本与远端已发布版本碰撞，先保留快照、核同源与内容，再只处理 root 自建副本，不清理无关文件、不 reset/force。
- `ed566b0` 的签名/DCO 政策仍须在原工作区 LTR-10/LTR-01/任何 push 前明确处置。新修复不以其为祖先，不借独立发布解决或掩盖旧合并政策。

### 使用规则与共享状态保护

依模板使用规则，新建本计划必须在 `plan-status.md` 的「计划一览」登记；推进每张卡时同一变更更新该行的卡状态、实际版本/commit及UTC秒级时间。每卡 I 都包含本文件和该行。原工作区先增加候选行；独立发布 clone 从实际上游 plan-status 开始，只新增/修改该行。逐行 diff 守卫必须证明原 scoped-traces 未发布段没有被带入。后续原生整合时对同一状态行逐项合并，保留双方历史，不以整文件覆盖解决冲突。原「LTR01唯一承载」指 Traces 治理的发布，不涵盖此独立恢复计划。

### 执行预算与停止规则

用户已明确授权为完成两个计划进行必要操作；本候选把必要的前滚修复具体化，必须先获得实际本地 Claude ER-03/H0 PASS，才按各卡新来源开始。原7200秒 claim/STOP封存，不使用剩余约2217秒，不重开旧FIX3/LTR00的3/3诊断及8/8窗口，不以换目录重置旧预算。失败处置 review 自身不授执行权。

候选独立 A 窗口：FIX-LTR-01一次7200秒，FIX-LTR-02/03/04/05各一次10800秒（共享夹具真实云消费者或最终doc/network，均包含完整默认套件）；每卡最终 bump 后本地 C 各一次10800秒；FIX-LTR-03 的 C 包含完整全量、doc及原3例network。五张实现卡实际D全绿后另有一次计划最终资格窗口10800秒，执行最终发布树fmt/clippy/full/doc/network，不以C代替模板计划完成门。上限是新独立来源的命令编排预算，不修改任何测试内部 deadline、并发组、断言或原 nextest retries。依据本机已测 build+listing+full4983秒及 strict Clippy约876秒，额外预留最终 release build/网络 feature 编译；不把冷构建慢当代码原因。本地窗口从第一条Cargo命令到本地门/构建/安装结束，包含编译和listing；PR CI、merge及Release D远端等待不计入该本地执行预算，单独登记run/job/ref，观察超时不重启作业、不记PASS。任一意外失败/超时/漂移立即保留终态并STOP，不自动再开窗口；后续只读归属与另行ER-03，不因剩余时间继续命令。

所有 Cargo 门 source `.env.test` 和 `.env.live-test`，核非秘密 presence/控制项、原工具链与 Cargo 配置；缺文件或必要凭据停止相应门。产品/集成测试延续已审隔离环境、已核 CARGO_BUILD_JOBS=1、umask0022，不复制用户凭据到仓库或日志。本次DOC02仅运行源码结构守卫及fmt/Clippy，具体合同保持登录HOME原值、只设CARGO_BUILD_JOBS=1并加载双env；target主要读源码；唯一runtime守卫 `agent_kind_enum_sql_check_and_doc_roster_stay_in_sync` 自建TempDir repo/home，用CARGO_BIN_EXE_libra init后只读SQLite并显式close，实际隔离依靠测试自身HOME重定向及执行器在调用环境、双env加载后均拒绝XDG_CONFIG_HOME/LIBRA_CONFIG_GLOBAL_DB/LIBRA_HOME三个override；LIBRA_TEST_HOME在产品二进制不生效，不能充当隔离证据。通过该检查后不访问用户库/live provider。环境差异及该fixture资源终态须在准入复审确认。listing与JUnit按(target,case)精确核数、保留原ignored/重试及成功输出中的runtime skip；缺环境、缺 feature、early return不冒充所需行为通过。代码完成、自身review、提交、push、D逐项分别登记。

构建缓存：独立 clone 使用自己的 `target/`，不与原工作区共享可写 target。不携带旧日志作为新证据；确认原 target 无 Cargo/rustc 写者后可复制缓存到新 target（禁止硬链接），记录工具链/配置/来源，Cargo仍在新来源重新核fingerprint。不能完成安全复制则使用冷 target，并在claim前重新审预算；不在claim外预编译来隐藏成本。当前7200/10800秒是暖缓存依据下的候选上限，不保证冷构建能完成，不得到时自动续窗。

完整默认套件的唯一命令为 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`；nightly fmt为 `cargo +nightly fmt --all --check`，严格Clippy为 `cargo clippy --all-targets --all-features -- -D warnings`。所有门读取并绑定同一受控输入，不改变nextest并发/配置。

每卡 A 包含其 Verification、nightly fmt及strict all-target/all-feature Clippy；触发全量的卡另跑原双env默认全量。自己的 ER-05 PASS 后，C严格按模板ER-04/08顺序：版本面parity→实际patch→工具链刷新lock→最终fmt/clippy和本卡完整全量/所需focused→`cargo build --release`（五张实现卡全局C⑤要求项目warning=0，唯一排除为用户具名proc-macro-error2 future提示；FIX04/FIX05另加keyring release门）→将本卡release二进制安装到仓外、每卡独立的 `install/bin/libra` 并以绝对路径核version/hash（复制+chmod0755，hash须等于本次target/release/libra）；不覆盖原工作区使用的libra或改变全局PATH→精确`libra add`及signed+DCO提交→feature branch/PR→该head的base/CodeQL全部适用job通过→squash merge并核merge SHA/tree/DCO/签名；merge tree必须等于已测bump后tree，任何上游变化或树不同即STOP并ER-03，不tag、不借旧PASS→人工Release及版本tag→release工作流与渠道artifact真实核实。遇分支保护、必需他人review或环境approval，保留实际pending状态；不使用admin bypass、不自行批准。未完成D不得done。编译诊断按实际记录；若既有future-incompat提示违反当次发布门，先独立归属，不静默豁免。

### 被测树、逐卡证据与终止链（执行前预登记，模板v2.14）

1. **每卡C前冻结。** 先把已真实取得的A/B、自己的Claude ER-05、前一卡已发生的C/D写入本计划与plan-status本计划行；当前卡只写实际已知状态，C/D未知项明确pending，不预填成功、未来commit或run。完成patch与工具链lock更新后冻结全部输入；本次C就在这棵树上执行。C到提交/PR/merge之间不再修改任何跟踪文件，包括状态和证据文档，因此最终提交树必须逐字等于C被测树，merge tree也必须相等。任何漂移STOP/ER03；不以文档白名单免除这三个树相等的约束。
2. **C/D后置证据归属。** FIX01的真实C/D、提交/tag/version/job/UTC收据由本次FIX04恢复准入在两份既定文档内补记；FIX05后置真实C/D由FIX04重新开工时在本计划与plan-status本计划行这两个既定文档补记；FIX04后置真实C/D由FIX02重新开工时补记；FIX02由FIX03同样补记。前一卡只有实际C/D全绿后才启动后继，不以文档中尚待补记的旧快照当证据。状态推进与证据更新在同一文档变更中完成，并在后继自己的C前冻结，后继C实际覆盖这些新增字节。若后继尚未开工，真实结果保持待补记，不提前执行后继、不发布单独的未登记补记提交；若后继STOP，随该次ER-03前滚修订在本计划正文同步补记前一卡已发布的C/D事实，仍用两文档既定写集与自身文档验收，不允许事实永久只留仓外。
3. **FIX03 done与计划完成分开。** FIX03自己的AC/VER、A/B、自身ER-05和独立C/D实际全部通过后才可done/complete；计划最终资格不是其AC的递归前置。随后在FIX03实际merge SHA的冻结树运行已登记的一次10800秒计划最终资格（fmt、strict Clippy、双env全量、doc、network原3例），再取得真实本地Claude来源资格PASS。该窗口不能包含之后才写出的终态文档，也不能把已发布产品树换成新的上游树。
4. **唯一终态登记提交。** 计划最终资格通过后，在独立clone创建一笔纯文档登记提交，I精确为 `docs/development/plan/plan-20261025-qualification.md` 与 `docs/development/plan/plan-status.md` 的本计划行；R=N/A、version=N/A、无tag/Release、不改变任一产品/测试/配置/lock/版本面。该提交承接FIX03真实C/D、最终资格和五张实现卡状态，以及它自己的真实Claude文档ER-05收据；核对实际证据，登记本终态文档自身D仍pending。它是本计划预登记终止链末端，不是第六个产品修复或新版本发布点，不替代五张实现卡各自C/D。没有独立发布原工作区Traces治理的权限。
5. **终态登记验收。** 终态文档相对FIX03 merge树只允许上两路径；逐行证明plan-status只改本计划行，其它来源逐文件相同。该文档差异不能宣称被前面的全量覆盖：先在候选文档完成结构/链接/状态一致性验收，再获得真实Claude文档ER-05 PASS；仅追加该审查的实际session/输入hash/结论/P2处置/UTC收据，不能预填或修改其它内容。随后在含已登记ER-05收据的最终字节上重新做结构/链接/状态一致性、`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`，及 `source .env.test && source .env.live-test && cargo nextest run --test compat_agent_architecture_guard --no-fail-fast --retries 0`（实际非零完整target，包含读取plan-status/long的守卫）；另只读扫描全部文档消费者，若还影响其它target则执行其完整target，输入超出此两路径或超出已登记预算先ER-03。预登记本地一次3600秒，仅这些文档相关门，无新的产品全量窗口。追加审查收据以外的任何改动都必须重新审查并重跑相关本地门；不得因此自动追加3600秒窗口，超出预算先ER-03。冻结最终文档字节，精确signed+DCO提交并核签名；走feature PR，base八job/CodeQL两job全绿后squash，核提交树=merge树=最终本地门被测树（含已登记的ER-05收据）；审查原输入与唯一后置收据差异分别记录，不声称Claude读过自己后来产生的输出，再观察merge SHA的全部适用base/CodeQL。无tag则release.yml不触发，具名N/A而非success。此文档提交的产品C覆盖仅继承FIX03已发布产品树；新增文档字节由自己的本地门与真实PR/main D覆盖。
6. **消除自引用，不静默完成。** 最终登记提交自身SHA、run ID、结论和观察UTC，只能推送后取得。依模板v2.14不再为它新建本计划提交：在下一次 `plan-status.md` 例行同步中补记，搭车既有维护提交；若到下一工作日尚无例行同步，以一笔明确的计划状态例行同步补记，属于plan-status维护，不属本计划终止链、不重启终止链。例行维护提交仍须精确signed+DCO、结构/链接/nightlyfmt/strictClippy与完整compat_agent_architecture_guard target（双env、retries0），真实Claude文档审查及PR/main base/CodeQL；不因属于维护而免门，后续若暴露其它读者则按实际影响补齐。这是库内证据落点，须实际执行；末端D未绿时不报告计划完成。原两计划接收本资格时同时登记该末端ref/D以及例行补记归属，不改原卡独立门。

任何最终登记门/PR/main D失败都保留实际FAIL、STOP并前滚处置，不为截断终止链豁免失败。计划最终资格与末端文档review都必须是实际结果，不预写未来PASS。仓库内不新增证据文件/目录；摘要、SHA、计数、run/UTC与修订历史落在上述计划正文，唯一例外是第6项模板指定的plan-status例行补记。

**终态B的只读执行器：** 使用仓外 `/tmp/libra-qualification-recovery-plan-20261025/terminal-document-check.py`（在该末端执行前生成、hash冻结并纳入真实Claude输入，不属于产品源码）。执行命令为 `python3 /tmp/libra-qualification-recovery-plan-20261025/terminal-document-check.py`，以已核FIX03 merge SHA、候选树及两路径allowlist为显式冻结输入；缺输入即失败。必须以原生Libra diff逐路径证明只有本计划与plan-status本计划行变化；全其它来源hash一致；卡片/索引状态与已发生原件逐项对应；本计划全部必需章节/五张实现卡字段存在；相对链接解析到当前clone；已登记审查收据只作后置事实，不改审查结论或其它字节；保留所有旧FAIL/STOP。自动结构结果不替代人审语义和真实门结果。

## 依赖登记表

| ID | direction | 类型 | 对象 | Owner | 产物与可用性判据 | 证据 | 超时与失败策略 |
|---|---|---|---|---|---|---|---|
| DEP-QUAL-SOURCE | incoming | 上游revision | libra-tools/libra@b73e98a77ecc7c3db1ea905ab55478785d76c041（初始0ca47cc保留为历史） | root | 独立clone的实际远端来源、允许差异及签名工具链已核；变化先重审 | 本文来源与精确比较记录 | 不追写原工作区，不用旧资格冒充新树 |
| DEP-QUAL-CODEONLY-GOV | incoming（仅待闭合交付） | 治理交付前置 | plan-20261015 GOV-LTR-CO-01 | root（唯一交付者） | GOV 七文件治理卡自身文档 B、T-5 双环境完整 nextest、fmt、严格 Clippy、release build、用户仅对本 GOV 卡后续 Claude Review 的具名豁免记录、签名+DCO 与适用 PR/main D 均实际取得，才可让本计划尚未闭合的来源资格及其终态登记采用 v2.16 code-only 交付；不作为旧已发布 FIX 卡的追溯前驱，也不妨碍独立来源诊断和源码准备 | GOV 最终 tree、七文件 manifest、commit/PR/main SHA、run/job/ref 与实际结论；R4 OAuth 失败不记 PASS；当前无交付终态 | main D pending/FAIL 时保持 incoming，`DEP-QUAL-HANDOFF` 不解除；不把 GOV 的验收代替来源资格 |
| DEP-QUAL-F2-ATTRIB | incoming | 只读归属 | reset真实stderr缺口与当前源码 | Libra接收Agent | Claude审定same/separate/unknown决定；unknown不代表关闭；不写源码不跑Cargo | 当前待审 | FIX05前置，无新诊断窗口 |
| DEP-QUAL-F2-CLOSE | incoming | P1关闭 | reset失败真实机制 | Libra接收Agent | 同轴则FIX05实际C/D+关闭审查，另轴则新FIX实际C/D；unknown不关闭。依 2026-10-26T19:12:37Z 例外，FIX04 对 CLOSE=unknown 豁免，只加两行 cfg | 当前未解决 | FIX04 的关闭前提被该例外豁免；不作为FIX05前驱；新FIX不自动继承warning例外 |
| DEC-QUAL-05-RELEASE-WARNING | incoming | 用户决定 | FIX05唯一版本的两方法dead_code例外 | 用户；Libra接收Agent记录 | 用户原话已记录：UTC 2026-10-26T05:40:46Z；范围只有 from_payload 与 from_payload_with_hint_policy；期限到下一个 patch 标签；v0.30.88 可以带上这两条诊断。session 4042079a RELEASE_ORDER_VERDICT PASS 且 VERDICT PASS | 2891a30b 仍是当时的 FAIL；现行决定是 GRANT-ONE-VERSION | 不扩大例外；不把 cfg 放进 FIX05；F2 unknown 仍不关闭 |
| DEP-QUAL-SECURITY | incoming | 跨仓前置 | Mega2 plan-20261015 FIX-MTR-11 | root | 既定安全卡完整C/D及两个告警实际fixed；不改变其不自行延期条款 | FIX11已done/complete，932731d/v0.42.70；23/24实际fixed于2026-10-24 20:16:51/52 UTC，见文首真实D收据（此前open保留为历史） | 未满足不实施本计划；只做恢复治理与审查 |
| DEP-QUAL-PUBLISH | incoming | 发布前置 | 独立clone与实际GitHub工作流 | root | 发布branch不包含原未发布治理祖先/文件；vault签名与DCO、版本面、PR/Release权限与artifact门实际可用。现行未闭合 code-only 前滚只要求来源/签名/DCO、PR/main 与适用 branch D；Release/artifact 门仅用于旧版本化提交的历史归属，不为新卡建 tag | 开工逐项原生核对，不能用gh浏览器登录互代 | 分叉/缺凭据/来源变化先停止归属，不force |
| DEP-QUAL-HANDOFF | outgoing | 来源资格 | plan-20261015 LTR-10 / LTR-01 | root | 历史五张实现卡实际版本化 C/D 逐项归属且旧同 SHA base D FAIL 保留；现行需对该 FAIL 做受审归属及必要 code-only 前滚，冻结最新上游整合树后重新执行完整双 env nextest/doc/network、本机 Claude 来源资格明确 PASS（绑定实际最终树），终态登记提交实际 PR/main D 通过并具名例行补记，再核受控整合树、原工作区受控整合/受影响门通过 | 后续真实提交/远端ref/原件，当前无；历史版本 C/D 按原 SHA 单独核 | 本生产计划 direction=outgoing、状态未解除；接收计划 direction=incoming，保持阻塞。来源资格未闭合不得解除LTR-10前置；不更改H1或替代LTR各卡门 |
| DEP-QUAL-F2-GATE | incoming | 用户决定 | `完成判据与交接`「全部P0/P1关闭」与 `DEP-QUAL-F2-CLOSE`「未知时保持P1」对 F2 的适用 | 用户；Claude执行Agent记录 | 2026-10-28T06:03:22Z 用户决定已记录（文首 2026-10-27 段原文）：F2 历史根因永久 unknown；FIX-LTR-06 完整 C/D、F2 用例诊断与本地 Claude 关闭审查 PASS 后 F2 P1 关闭；仅限 F2；以 2026-10-28T06:03:22Z 当日接受 FIX04/02/03 已在 CLOSE=unknown 下发布的事实，这不是对 19:12:37Z 例外的历史批准。DEP-QUAL-HANDOFF 因此另需 FIX-LTR-06 C/D 与该关闭审查。关闭审查须绑定：FIX-LTR-06 发布 SHA、tag、Release 与 D 收据；发布树中 AC-5 诊断存在的源码与测试证据；`user-decisions.json` 的 SHA256；本段与 F2 分析原件。输出键 `F2_CLOSE: CLOSED|OPEN` 与 `VERDICT: PASS|FAIL` | 用户决定已取得；执行方案审查见 Claude review log 中标为 2026-10-28 的各行；关闭审查尚未进行 | 现行归属：FIX-LTR-06 已发生的版本化 SHA/tag/Release 与 D 逐项按原 ref 核，旧 base D failure 不被 Release success 抵消；仅同 SHA/同 ref 受控重跑实际成功可作为新 D 收据并经 Claude 归属，代码缺陷则另立前滚 FIX 卡按 code-only C/D 与最终来源资格重审，不借新卡倒补旧 tag。关闭审查 PASS 前 F2 P1 不关闭，DEP-QUAL-HANDOFF 不闭合；不扩大到其它 P0/P1 |
| DEP-QUAL-SIGNING-KEY | incoming | 公钥 | `3F5F84D5E79CEF6C414339F20005024EAF59B8E4` | 用户/另一机器 | 可核来源的公钥及对 e85470a、ff30d4a、98852b7、e24c88b、e5424c5、380c262 与四个附注 tag 的实际 VALIDSIG | 本机 NO_PUBKEY；GitHub 提交 unknown_key、tag unverified_email；keys.openpgp.org 与 keyserver.ubuntu.com 404；2026-10-28T06:03:22Z 用户确认取不到 | 保持未验证（RISK-QUAL-SIGNING-UNVERIFIED），不改写历史、不移动 tag |
| DEP-QUAL-CARGO-WINDOW | incoming | 本机共享执行环境 | `DEP-TR-ENV-DIAG-CARGO` 的 Libra 侧消费 | root | root 明确交接，前一窗口实际终态、owned 进程与资源关闭已核 | 2026-10-28T06:03:22Z 用户决定 root A3 结束后交给 Libra；root 05:59:20Z 协调收据写明尚未发放；root 于 07:49:04Z 发出授权 `coordination/root-grant-20261028T074904Z.json`（DOC-LTR-03 3600 秒、FIX-LTR-06 A/C 各 10800 秒、最终资格 10800 秒，各自先经准入；结束须由 Libra 明确归还） | 未取得不编译、不测试、不发布 |
| DEP-QUAL-REMOTE-ORIGINALS | incoming | 原件 | 另一机器上 FIX05/04/02/03 的 C、e24c88b 最终资格窗口、bde085f1 审查 | 用户/另一机器 | 原件及 SHA256 可核 | 本机不可得；2026-10-28T06:03:22Z 用户确认取不到 | 保持缺失登记（RISK-QUAL-ORIGINALS-MISSING），以最终树新窗口作新证据，不补造旧 PASS |

## 实施顺序与发布窗口

`DEP-QUAL-SECURITY + DEP-QUAL-SOURCE + DEP-QUAL-PUBLISH -> FIX-LTR-01 -> FIX-LTR-05P -> FIX-LTR-05Q -> FIX-LTR-05R -> FIX-LTR-05S -> FIX-LTR-05T -> FIX-LTR-05U -> FIX-LTR-05V -> FIX-LTR-05W -> FIX-LTR-05X -> FIX-LTR-05Y -> FIX-LTR-05Z -> FIX-LTR-05 -> FIX-LTR-04 -> FIX-LTR-02 -> FIX-LTR-03 -> DEP-QUAL-HANDOFF`；另有 `DEP-QUAL-F2-ATTRIB + DEC-QUAL-05-RELEASE-WARNING -> FIX-LTR-05`，另有 `FIX05/具名新FIX -> DEP-QUAL-F2-CLOSE -> FIX04`。`FIX-LTR-05W` 已推送 `2a80a10`，只含 `resolve_configured_storage` 与 fetch 写入前检查。`FIX-LTR-05X` 已推送 `a729384`，只含 `init` 的确认失败拒绝状态和不缓存该状态。`FIX-LTR-05Y` 已推送 `ec98926`，只含未决 TYPE 不进入对象缓存。`FIX-LTR-05Z` 只含未决 TYPE 这一次调用不再构造 `LocalStorage`。ER03 `272c8d7d` PASS，实现审查 `4a42ec63` PASS。父卡 VER-1 至 VER-5 已在 ae50414 通过。本提交占用 v0.30.88。v0.30.88 的 D 已于 2026-10-26T18:23:07Z 观察成功：base 40349034162 attempt 2、CodeQL 40349034316、release 40349204443，SHA e85470a。F2 仍是 unknown。依 2026-10-26T19:12:37Z 例外，FIX04 对 CLOSE=unknown 豁免。顺序用于共享本计划/版本面的串行发布，不表示合并发布。先修确定性全量阻断，再修共享fixture，最后完成MCP测试清理与整份修复计划的最终资格。每卡真正 C/D 闭合后才发布下一卡。`FIX-LTR-05P` 到 `FIX-LTR-05Z` 都是 family child，不占用 v0.30.88。FIX04 已发布 ff30d4a / v0.30.89，D 于 2026-10-26T20:44:43Z 观察成功：base 40391580491、CodeQL 40391580303、release 40391698253。FIX02 从该发布树接续，不把 F2 写成已修复。

**2026-10-28 追加顺序（不改上一段历史）：** `FIX-LTR-03 -> FIX-LTR-06 -> DEP-QUAL-F2-CLOSE（依 DEP-QUAL-F2-GATE 的关闭审查）-> 计划最终资格（FIX-LTR-06 发布树）-> 来源资格 Claude 审查 -> 终态登记 -> DEP-QUAL-HANDOFF`；`DOC-LTR-03` 为 no-release 文档卡，产品 C 覆盖来自 FIX-LTR-06。每个本地窗口另依 `DEP-QUAL-CARGO-WINDOW`。

**2026-10-29 追加顺序（不改上两段）：** `FIX-LTR-07 -> FIX-LTR-06`、`FIX-LTR-08 -> FIX-LTR-06`；FIX-LTR-06 的 A 全量在三卡合并树上执行，C/D 一次覆盖三卡并发布 v0.30.92；其后仍为 `DEP-QUAL-F2-CLOSE -> 计划最终资格（v0.30.92 发布树）-> 来源资格 Claude 审查 -> 终态登记 -> DEP-QUAL-HANDOFF`。root 窗口已于 2026-10-28T14:48:41Z 归还、未重新授予，此后各窗口依用户指令并先发协调通知。

**2026-10-31 现行交付前置（不改历史顺序）：** `GOV-LTR-CO-01 main D PASS -> DEP-QUAL-CODEONLY-GOV -> 本计划待闭合 code-only 交付/终态登记 -> DEP-QUAL-HANDOFF`。GOV 与来源资格是两个独立门；治理可先完成，来源诊断可并行准备，最终交接仍须满足本计划所有真实验收和旧 D 失败归属。

### 实际 D 映射与现场复核

当前 `base.yml` 为PR及main/release/** push，`codeql.yml`为main目标PR及main/release/** push，均无paths过滤；实际job/if必须在每卡开工重新读取完整workflow，绑定最终head与merge SHA，不能只看workflow标题。`release.yml`由v* tag触发，当前build-and-upload有linux amd64/arm64、darwin arm64、windows amd64四个平台并通过OIDC release API上传；完整发布job/渠道探针一并核真实成功。失败前滚，不回写版本tag，不伪造未触发workflow通过。


### 五张实现卡共同适用的逐项D清单

每卡逐项登记run/job/attempt/ref/实际终态；不得用一条workflow success代替漏项。无顶层job if的项均适用；步骤条件和后续workflow变化在开工重新核。PR被测merge-ref、PR head和main merge SHA分开记录。

| Workflow | Job | 触发事件与ref | 判据 |
|---|---|---|---|
| `base.yml` | `format / compat-rustfmt` | PR目标main（refs/pull/<n>/merge及head）和merge后push refs/heads/main；base与CodeQL均另适用release/** push | 格式零diff |
| `base.yml` | `clippy / compat-clippy` | PR目标main（refs/pull/<n>/merge及head）和merge后push refs/heads/main；base与CodeQL均另适用release/** push | all-target/all-feature lint零warning且rustdoc links通过 |
| `base.yml` | `owner-liveness-macos / compat-owner-liveness-macos` | PR目标main（refs/pull/<n>/merge及head）和merge后push refs/heads/main；base与CodeQL均另适用release/** push | 真实macOS具名owner safety用例非零且全通过 |
| `base.yml` | `scoped-input-windows / compat-scoped-input-windows` | PR目标main（refs/pull/<n>/merge及head）和merge后push refs/heads/main；base与CodeQL均另适用release/** push | 真实Windows scoped input全部具名门通过 |
| `base.yml` | `redundancy / compat-redundancy` | PR目标main（refs/pull/<n>/merge及head）和merge后push refs/heads/main；base与CodeQL均另适用release/** push | 现行冗余/生成表检查全部通过 |
| `base.yml` | `test / compat-offline-core` | PR目标main（refs/pull/<n>/merge及head）和merge后push refs/heads/main；base与CodeQL均另适用release/** push | 实际L1+已注入L2/L3与独立feature步骤全通过，记录skip/flake |
| `base.yml` | `network-remotes / compat-network-remotes` | PR目标main（refs/pull/<n>/merge及head）和merge后push refs/heads/main；base与CodeQL均另适用release/** push | 真实network-remotes具名目标通过 |
| `base.yml` | `opencode-export-sandbox-linux` | PR目标main（refs/pull/<n>/merge及head）和merge后push refs/heads/main；base与CodeQL均另适用release/** push | Ubuntu隔离工具及具名export sandbox门全通过 |
| `codeql.yml` | `analyze / security-codeql-actions、security-codeql-rust` | PR目标main（refs/pull/<n>/merge及head）和merge后push refs/heads/main；base与CodeQL均另适用release/** push | 两个matrix job分别success |
| `release.yml` | `build-and-upload × linux-amd64/linux-arm64/darwin-arm64/windows-amd64` | 人工创建版本tag/Release触发push refs/tags/v<本卡版本>，解引用为已核merge SHA | 四平台编译上传全部success，digest/size与下载artifact匹配 |
| `release.yml` | `upload-install-scripts` | 人工创建版本tag/Release触发push refs/tags/v<本卡版本>，解引用为已核merge SHA | 脚本上传成功、发布版本面与CDN脚本实际一致 |
| `release.yml` | `update-homebrew-tap` | 人工创建版本tag/Release触发push refs/tags/v<本卡版本>，解引用为已核merge SHA | 实际tap更新成功并绑定本卡版本/digest |
| `release.yml` | `verify-homebrew-formula` | 人工创建版本tag/Release触发push refs/tags/v<本卡版本>，解引用为已核merge SHA | 更新后formula验证job实际success |
| `release.yml` | `request-stable-manifest` | 人工创建版本tag/Release触发push refs/tags/v<本卡版本>，解引用为已核merge SHA | 四平台完整签名manifest发布success，stable/CDN实际version/digest对应本卡版本 |

## 任务卡

### Task FIX-LTR-01: 同步全局schema doctor的完整auto-upgrade结构守卫

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `complete`（实际f33be958/v0.30.87与完整D见本轮已核后置事实；旧A/C/CI待快照保留）

**Description:** 只更新现有完整结构字符串，使它同时保留schema_doctor、Upgrade和当前Ui排除，恢复确定性兼容测试；不改CLI行为。

**Out of scope:** fixture时序由FIX-LTR-02承接，MCP teardown由FIX-LTR-03承接；UI产品行为测试/解释由上游UI所有者承接，若需要改变生产逻辑则本卡停止并重新ER-03。

**Current evidence:** `tests/compat/global_config_schema_future_test.rs:727`在三次原生尝试均失败。该文件在2755eb7和0ca47cc的SHA均为e16a5fa6e3307b2dfe6eeb0827b3337faae4a4de18903363112a10829585e2ab；`src/cli.rs:3636`新增Ui排除，旧结构字符串未同步。前面的真实scope/role/path/canary断言已到达并通过。

**Acceptance criteria:**

- [x] 完整字符串包含原schema_doctor和Upgrade排除，并加入当前Ui排除；不得删除结构守卫或改为松散子串。
- [x] 该用例全部原runtime角色/路径/损坏canary/legacy断言与相邻startup recovery守卫逐字保留。
- [x] 完整compat_global_config_schema_future目标通过，生产CLI与其它测试字节不变。

**Verification:**

- [x] `source .env.test && source .env.live-test && cargo nextest run --test compat_global_config_schema_future -E 'test(=config_doctor_uses_global_role)' --retries 0`；必须精确1例、0失败、不可skip。
- [x] `source .env.test && source .env.live-test && cargo nextest run --test compat_global_config_schema_future --no-fail-fast --retries 0`；与运行前实际完整listing逐名一致。
- [x] 只读diff守卫：唯一测试语义改动为既有完整字串增加`|Commands::Ui(_)`；原runtime断言/CLI源码均不变。

**Full-suite trigger:** T-5：本卡修复新来源全量的确定性阻断；A与最终C均明确要求完整默认套件，不以单例PASS替代。

**Dependencies:** DEP-QUAL-SOURCE, DEP-QUAL-SECURITY, DEP-QUAL-PUBLISH；仅消费具名发布后来源与当前卡实际准入，前驱未complete不开始本卡发布。

**Deliverables:** N/A；测试修复及本计划同卡记录完整交付。

**Implementation write set:** `tests/compat/global_config_schema_future_test.rs`；本计划文件；`docs/development/plan/plan-status.md`仅本计划行（以实际发布上游文件为基线）。原scoped-traces三治理文件的未发布增量仅由协调者另行本地同步，不进入本卡发布分支。

**Release write set:** Inherited；以当前compat_version_surface_sync为权威的Cargo.toml/install.sh/install.ps1等实际版本面、工具链Cargo.lock及artifact；开工重核，不手改lock。

**Files likely touched:** 与I/R完全一致，不新增target或公共helper注册。

**Docs and compatibility impact:** 本计划及plan-status本计划行同卡更新真实状态；用户CLI/参数/输出/schema没有变化，双语命令指南、COMPATIBILITY、错误码与网站N/A。tests/INDEX既有目标名称/用途不变；若实际发生变化先扩写I并ER-03，不能漏文档。

**Rollback mode:** `immutable-release`

**Migration and rollback:** 无schema/数据迁移；未发布源码保留精确差异，已发布问题只用新签名提交/新patch前滚，不回退ref或版本tag。

**Security and privacy:** 只使用合成fixture与具名测试资源；不读取真实provider正文，不在证据中保存env值/token。进程清理必须证明归属，云资源按fixture唯一namespace，不清扫既有资源。

**Performance budget:** A一次7200秒；C一次10800秒；单测timeout/retry不变。一次claim，非预期失败STOP；具体执行器与输入manifest在执行前实审。

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`；本卡自己的完整C/D，不免bump、不合并其它修复。

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=global-schema doctor的auto-upgrade结构oracle对齐; recovery=immutable-release前滚本卡单轴测试修复; complete=yes; self-contained=yes; AC=3/8; VER=3/8; landing=1; prod-files=0; scope=S; deps=DEP-QUAL-SOURCE, DEP-QUAL-SECURITY, DEP-QUAL-PUBLISH; writeset=五张实现卡本计划与R串行; release=independent; split-from=新上游资格20261025实际FAIL/STOP; exception=N/A`

### Task FIX-LTR-04: 对齐同步 ingress 测试构造器的编译配置

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / v0.30.89 D 已观察（旧A1勾选和失败C1都不是这次C。2026-10-26T19:12:37Z：历史 TRY1 stderr 不可恢复，CLOSE 保持 unknown，P1 不关闭。审查 a929a323 VERDICT PASS，AMENDMENT_ALLOWS_FIX04 yes。实现审查 354b5170 VERDICT PASS，CFG_ONLY yes。ingress.rs sha256 2b7aa33698c2072d160cc6bea7801ffb8fb180a13bb0ebe78901ff888b7bb778。这是执行器依当前目标记录的修订，不是用户批准。本卡仍只在 `from_payload` 与 `from_payload_with_hint_policy` 前加 `#[cfg(any(test, debug_assertions))]`。若这次 C 重现 TRY1 形状，即 exit 129 之后整段 stderr 不能解析为 JSON，则 ER10 STOP，不用 nextest 重试成功当作通过。发布 `ff30d4ad28517d77d44f10e4753734b30a0b3677`，tag v0.30.89。D 于 2026-10-26T20:44:43Z 观察成功：base 40391580491、CodeQL 40391580303、release 40391698253。F2 仍 unknown，P1 不关闭）

**历史退出前Lifecycle字段：** **Lifecycle / Acceptance:** `blocked` / `locally-accepted`（仅原A1/源审限域保留；C1 native审查FAIL/root STOP，具体退出待审/待执行，未发布）

**历史Lifecycle / Acceptance（C1前）：** **Lifecycle / Acceptance:** `in-progress` / `locally-accepted`（A1/源B、root资源后检及自身本地Claude R1三PASS已核；两原生flake按具名P2限域承接，最终C/D未完成）

**Description:** `CaptureIngressCommand::from_payload` 与 `from_payload_with_hint_policy` 只供现有 debug/test lowerer 和测试使用，两个定义却留在默认 release。仅在两定义前添加与消费者相同的 `#[cfg(any(test, debug_assertions))]`，消除真实发布构建的项目 dead_code 诊断。

**Out of scope:** 不改两个方法体、可见性、生产 `from_stdin`/parse/finish、CLI、策略、测试断言或依赖；不修 FIX02 fixture/FIX03 MCP，不处理用户明确排除的第三方 proc-macro-error2 未来警告；不扩大为 release-profile integration 测试支持。

**Current evidence:** FIX02 C1 的 release-build 实际输出一项项目 dead_code、涉及 ingress.rs:453/472 两方法。ROOT在警告已违背严格门时停止已核协调进程，原terminal于2026-10-25T11:33:57.665096Z记RuntimeError/STOP、5051.049/10800秒；native build 被中断，数值退出未记录，不记成功或编造0/101。此前9阶段exit0，完整10761/21原ignored/0重试/0flake，模块15、真实云1、fmt/strictClippy保留；安装/提交/推送/Release未执行。原件在仓外fix-ltr02-C1-20261025与failure目录。source审ce6fb162 SOURCE_AUDIT PASS证明唯一非test消费者在cfg(debug/test)而方法未cfg；RECOVERY_GUIDANCE FAIL的P1必须由本次全局I特许及A release gate关闭。独立源为published b73/.87，不含FIX02 helper5025候选或原scoped-traces未发布祖先。

**Acceptance criteria:**

- [x] 默认 release 构建不再产生项目 warning；排除项仅为用户具名第三方 future-incompatibility 提示。
- [x] release 的 keyring 配置不产生项目 warning，保持实际发布 feature 的可编译性。
- [x] 原 debug/test 调用者在其既有配置中仍可使用同步构造器；具体测试证据见Verification。
- [x] 源码行为差异严格只有两行 cfg；方法体、from_stdin 生产入口及所有断言不变。

**Verification:**

- [x] `source .env.test && source .env.live-test && cargo build --release`；实际exit0，逐条诊断核项目warning0。
- [x] `source .env.test && source .env.live-test && cargo build --release --features keyring`；实际exit0，逐条诊断核项目warning0。
- [x] `source .env.test && source .env.live-test && cargo nextest run --lib -E 'test(internal::ai::capture::)' --no-fail-fast --retries 0`；完整capture模块listing与实际身份一致，0 tests/early return不得充当所需行为。
- [x] `source .env.test && source .env.live-test && cargo nextest run --test compat_agent_architecture_guard --no-fail-fast --retries 0`；完整target非零、逐身份核对。
- [x] 相对b73的源diff恰为ingress.rs两行cfg；既有lowerer消费者cfg与候选条件相等/为其子集，helper reasoning_artifact.rs等于b73 blob；精确I/R守卫。

**Full-suite trigger:** `T-4`：同步构造器承接共享lowerer，超过6个integration target复用；A和最终bump后的C均保留双env完整默认10761/原ignored身份核验。唯一命令仍为 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`，不改timeout/retry/并发。VER为附加门，不替代全量。

**Dependencies:** `FIX-LTR-01`、`FIX-LTR-05`、`DEP-QUAL-F2-CLOSE`、`DEP-QUAL-SOURCE`、`DEP-QUAL-PUBLISH`。依 2026-10-26T19:12:37Z 例外，FIX04 对 CLOSE=unknown 豁免，不删除这条边，P1 不关闭。FIX05 的实际 C/D 仍是前提，已在 e85470a / v0.30.88 观察成功。当前字段按2026-10-26跨机入口，旧字段见历史记录。

**Deliverables:** 本卡两行代码及本计划/plan-status本计划行的实际来源、审查和验收记录；原C1 STOP、旧P2接受和unknown数值exit保留。

**Implementation write set:** 仅本卡额外允许生产文件 `src/internal/ai/capture/ingress.rs` 的两个cfg属性；`docs/development/plan/plan-20261025-qualification.md`；`docs/development/plan/plan-status.md`仅本计划行（以published b73为底）。这是一项卡级精确特许，不扩大FIX01/02/03原测试写集。

**Release write set:** 权威版本面 `Cargo.toml(version)`、`install.sh`、`install.ps1`，及工具链刷新 `Cargo.lock`；禁止手改lock或加入其它依赖变化。

**Files likely touched:** 与I/R一致，最终精确7路径；FIX02 helper候选/原scoped-traces三治理不得入本卡。

**Docs and compatibility impact:** 两方法均pub(crate)，已有公开debug/test lowerer可用配置不变，无用户CLI/参数/输出/schema/协议改变；用户指南双语、COMPATIBILITY、error-codes、网站N/A。现有内部代码span注释不改，不新建仓库证据目录。

**Rollback mode:** `immutable-release`

**Migration and rollback:** 无数据迁移；未发布只撤精确自有cfg与本卡治理，保护保存像/stash。已发布只新签名patch前滚，不reset/force/rewrite/tag覆盖。原FIX02失败.88四面退出、三个WIP精确stash与从b73建本卡branch须先通过恢复ER03，不由本卡偷偷执行。FIX04实际D全绿后才三方重建后继治理并恢复helper5025，不能直接pop旧文档/版本面。

**Security and privacy:** 原始provider frame边界不变，生产入口仍为from_stdin；不加allow、不打印body/token、不复制env到提交。只清理精确own资源，旧FIX03 worker尚未修，按既有审定形状/有界pidfd收尾与未知STOP规则。

**Performance budget:** A1候选一次10800秒、命令10620秒，含fresh编译/listing、fmt/strictClippy、VER focused、完整默认、default/keyring两release profile及资源后检；C1候选一次10800秒同样含实际bump/工具链lock与最终完整门/default+keyring两release构建/私有安装。编译暖缓存来源在claim前实核，禁止claim外预编译；若实际预算无法成立先ER03，不自动续窗。两窗口必须各自具体执行器真实ClaudePASS后唯一claim，零自动重跑；不借FIX02 C1余量，不复位任何旧FIX3/LTR00窗口。

**A1/B实际结果（2026-10-25 14:16:54 UTC，自身ER05待）：** 唯一A1原件`/tmp/fix-ltr04-A1-20261025`，具体执行审查session c8ad29bf-1b9a-46ed-8f8c-d4a5c3827560四PASS后才claim。十阶段native exit0：nightlyfmt、all-target/all-feature strict Clippy、完整capture 436、architecture 65、default/keyring两release构建、完整默认10761/21原ignored及其实际listing；终态执行耗时5795.491/10800秒，资源后检及root核验在同一原窗口内。终态SHA256 `2e5cfb669bba2dfe1ebe0a04c61d63d30a9f6af9c8cd6f6248a288689e6439e0`、full JUnit `0480f5861306f2682d9014561b0072d908180e73dfe7dabbb825701ccc222db7`、root-A `74b187277a94a396df429417d0e35edec2510d4fb9135b50c154a919112c790e`。source B原件`/tmp/fix-ltr04-source-B-20261025`：相对published b73，ingress source `2b7aa33698c2072d160cc6bea7801ffb8fb180a13bb0ebe78901ff888b7bb778`仅增加两cfg，方法体/断言/from_stdin不变、所有调用者条件为同条件或子集、FIX02 helper等于published基线。源B初次误猜out-of-line模块owner的检查失败已保留，纠正为实际live_pipeline.rs后核验通过，没有产品重跑。

**历史A1限定与具名风险（2026-10-25 14:16:54 UTC，owner=root；当时须自身Claude裁定，后续真实裁定见下段）：** 原定`--retries 2`产生2个首试FAIL、次试PASS：`libra::command::agent::checkpoint::tests::fix_rg_scoped_01_fresh_error_mapping`；`libra::command_test::command::stash_test::test_stash_no_change_untracked_only_and_quiet_guards`；保留原始输出和JUnit，不记零失败尝试。fresh-error-mapping只观察到CatalogUnavailable，该错误合并多个连接/查询/关闭/期限原因；stash quiet只观察到SQLite code5及storage-config fallback的runtime Warning，具体锁持有者及原因未证。两组相关源码均与published b73和A1冻结输入字节相同，这不等于历史复现或无回归证明，不以机器负载解释根因。分别登记RISK-QUAL-FRESH-MAPPING、RISK-QUAL-STASH-QUIET，root在后续资格维护按ER03评估；自身审查若判P0/P1必须先解决，不预设可接受。runtime输出按本次日志身份/文字与当前未变源码逐项归属：{"optional Mega LFS endpoint absent": 1, "case-insensitive filesystem prerequisite": 2, "test-live-cloud feature disabled in default suite": 9, "Bun interpreter runtime coverage unavailable": 1, "asserted missing-interpreter negative scenario, not skipped test": 1, "explicit GitHub live fixture prerequisites absent": 10, "explicit Mega2 live server/write-root prerequisites absent": 12, "current build rejects staging non-UTF8 filename; status read path unproved": 1, "PowerShell interpreter runtime coverage unavailable": 1}；不能把默认suite PASS等同未启用live/platform分支覆盖。capture与architecture无runtime opt-out或flake。资源关闭只处理本次精确观测的known worker，实际owned/new-orphan/target均0；FIX03清理源码尚未修，不倒推旧历史进程归属。仅用户具名proc-macro-error2 2.0.1 future warning排除，项目编译warning0；上述runtime Warning作为失败原件保留，不称已修。

**自身ER05实际结果与限域采纳：** 本地Claude session `e76e3d02-d544-4259-9f2b-42a937cc49e9`于2026-10-25T14:22:16.368832Z真实exit0、ER05/H0/VERDICT三PASS；manifest `cafd99c544a1d718d68b50ed170a7eac0899ad2eef217506efa84a9337757902`、output `ebd11737a87762761b0da72f71684b963fbfc86242348e73c2b001468a1b73b5`。root于14:24:41.918607Z实算全部73输入后采纳，P0/P1=0；原件`/tmp/fix-ltr04-own-review-R1-20261025/`及`/tmp/fix-ltr04-A1-root-audit-20261025/root-own-review-adoption.json`。这是审查后的实际收据；不能声称Claude事先读过本段，本两治理差异须由最终C的具体执行审查绑定并由最终被测树覆盖。两cfg源码和原A1输出不变，不借本记录重跑A或取得C资格。

**五项P2处置（owner=root）：** （1）`RISK-QUAL-STASH-QUIET`：本卡C原样保留retries2并保存实际FLAKY/FAIL，最终失败STOP；资格计划完成前另按受审ER03为SQLite BUSY导致storage-config退回本地登记FIX/DEFER并取得真实baseline证据，当前锁持有者/原因未证。（2）`RISK-QUAL-FRESH-MAPPING`：同样保留native尝试和STOP规则，资格完成前为CatalogUnavailable合并原因及InternalInvariant映射登记FIX/DEFER和真实baseline，不沿用FIX02旧subagent flake结论。（3）最终C必须实际default/keyring双release、项目warning0；永久release-profile warning防回归守卫作为root后续维护，未在本卡实现。（4）本地lock/源码冻结，实际版本D四平台必须覆盖--locked/target/strip及全部渠道artifact，不能以本地两个build替代。（5）最终C包含这两份治理最新字节，C后不改tracked文件，后置事实由FIX02既有治理承载。仅用户具名proc-macro-error2 v2.0.1 future提示排除。审查所称cfg不改变debug/test语义不等于baseline复现，也不证明历史DB根因；自身审查未读的focused JUnit/listing/执行器/进程原件已由root核验，具体C审查须补充原件输入，不冒称自身审查已读。

**发布状态：** 原C1不接受且STOP；实际四版本面已退出到b73/.87，保留两cfg及7路径失败像，无active bump。退出终态增量审查待；本卡仍blocked，未授新A/C，未commit/push/PR/tag/Release。

**历史退出前发布字段：** **发布状态：** C1已执行全部11阶段及私有安装，但native新失败审查FAIL，root最终不接受C并STOP；候选.88未commit/push/PR/tag/Release，不能发布。精确退出四版本面的方案待本地Claude及root采纳；退出后仍blocked，未授任何新A/C。

**历史发布状态（C1前）：** **发布状态：** A1与源B限域验收、自身ER05/H0已取得；独立C1具体执行器审查、最终bump后完整门、私有安装、精确signed/DCO、PR/merge/tag/人工Release和全部适用D均未完成。0.30.87未bump，FIX02旧C1 STOP及stash988ded20保留；正式资格1/4、原scoped-traces0/26/H1不变。原scoped-traces三治理不进入本分支，本卡C/D后由FIX02两治理承接，不在C后修改已测提交树。

**Estimated scope:** `S`

**Version increment:** `patch`（当前base0.30.87，.30.88仅未占用时的候选；actual claim前核remote/tag/Release，变化则ER09/ER03）

**Release boundary:** `independent`；本卡自己的A/B、实际ClaudeER05、版本bump、完整C、精确signed/DCO、feature PR全部base/CodeQL、gh squash且merge tree=被测tree、人工版本Release/tag、全部渠道D。无G08/G13合并。

**C/D coverage from:** `self`；本卡后置真实C/D由FIX02重新开工时在已登记两治理中承接并由其自身最终C覆盖，不改终止链末端FIX03的全量资格/终态登记义务。

**Granularity:** `type=implementation; axis=同步ingress测试构造器的profile可用性; recovery=immutable-release; complete=yes; self-contained=yes; AC=4/8; VER=5/8; landing=1; prod-files=1; scope=S; deps=FIX-LTR-01,FIX-LTR-05,DEP-QUAL-F2-CLOSE,DEP-QUAL-SOURCE,DEP-QUAL-PUBLISH; writeset=两cfg及既定两治理+版本面; release=independent; split-from=FIX02-C1实际release警告STOP; exception=2026-10-26T19:12:37Z FIX04对CLOSE=unknown豁免，P1不关闭`

**RB-FIX04-EXIT-C1（候选）：** FIX04原C1于2026-10-25 18:10:38 UTC结束，11阶段exit0、私有安装.88及资源后检exit0，5348.666525/10800秒；外层工具96753实际exit0。完整10761身份/21原ignored保留，4个flake/5个失败尝试后native重试成功；实际本地Claude session c7f9fbae-734f-416c-9d9d-3650ede3f824的四项判定全部FAIL，root于18:17:28 UTC记录STOP，C不接受。F1 P1为storage配置读取错误后无条件LocalStorage回退并污染machine stderr；F2暂定P1为reset missing-path失败未保留实际stderr，根因未知；F3 stdin fixture race与F4未执行accept脚本列P2。所有旧FAIL/STOP、A1两flake、37运行时覆盖缺口及21 ignored保持，历史SQLite锁owner/SQL与C2数据库根因未证。本次仅候选RB-FIX04-EXIT-C1：先保存七路径失败树及SHA清单，再用原生Libra精确恢复本卡四个未发布版本/lock到原before-bump（逐字等于b73/.87），保持两cfg和其它全部源码、原stash988ded20、两仓原WIP。经本机Claude EXIT/ER12/H0/VERDICT PASS和root采纳才执行；不reset/force/stash-pop/commit/push，不新开A/C，不授.88再次发布。退出成功仅释放本次失败C1发布占用，FIX04仍blocked/locally-accepted，资格1/4、原scoped-traces0/26不变。后续F1/F2/F3须先审定合法独立修复与依赖；不得把无G13的本仓改成聚合，也不借G08归并独立缺陷。Mega102自己的ER05已PASS，独立C2仍需退出终态与自身具体执行合同PASS。

**退出执行守卫：** 唯一root；旧C outer/driver均终态、owned/new-orphan/target=0且无新Cargo/Claude；HEAD/origin/main/远端main均b73，当前分支fix/capture-ingress-profile-20261025，index为空且精确7脏路径。当前全tracked blob/mode、七路径前像、两版本前像/安装器及published对象逐字绑定；已发布main若变先STOP/ER03。无v0.30.88 tag/Release的实际HTTP404必须再核，禁止把权限错误当不存在。先创建一次退出claim，再仅执行libra restore --source b73e98a77ecc7c3db1ea905ab55478785d76c041 --worktree Cargo.toml Cargo.lock install.sh install.ps1；Cargo.lock是恢复原工具链生成前像，不手工修改依赖。随后写入受审两治理候选，核其余全部tracked文件、原两仓9+4路径及stash不变，index空、版本.87；任一步失败保留实际部分状态和STOP，无自动重入。原private .88二进制仅隔离证据，不加入PATH/不作为发布或后续测试工具。实际退出收据在同一两治理候选上追加真实UTC，并另经增量本地Claude核对后用于下个发布准入；不得宣称本次审查已读未来收据。

**实际退出证据（2026-10-25 18:59:44 UTC）：** FIX04 C1的精确退出已实际执行：本地Claude退出R1 session ff79d09e-3485-4fdb-9316-258cd5d0b614四项PASS、50输入重核；root采纳后，原生restore及外层工具18851于2026-10-25 18:57:47 UTC实际exit0。四个版本/lock逐字等于published b73/.87，四个0664模式保持，原生stash list确认988ded20及完整列表不变；七路径失败树已在持久私有位置按SHA256/MANIFEST及模式补充保存。两cfg及其余全部tracked blob/mode、原Mega9/publisher9/原Libra4路径WIP保持，index空，仅余本计划/本计划status行/ingress三个脏路径。这是退出执行事实，当前等待退出终态增量单审；不授任何新A/C、commit/push/tag/Release。FIX04原C1机械11阶段0但native审查四FAIL、root STOP保持，Lifecycle仍blocked/locally-accepted，仅旧A1/源审限域有效；storage错误回退P1、reset实际stderr不足暂定P1与其它具名风险尚未解决，资格1/4、原scoped-traces0/26不变。失败C1已无active bump；后续独立Mega102须在本次终态增量单审及自己的具体C2合同通过后才claim。 root实际核50输入/原生全stash列表/四mode/9+9+4保护；不声称曾存在Git refs/stash：首次只读probe实际exit129已保留，改用本仓原生JSON stash list。REST单tag/Release404之外，已登录gh完整分页release list也无v0.30.88（含draft）。恢复命令可写Libra内部租约等metadata；“六路径”只指tracked写集，不把metadata写成零变更。

### Task FIX-LTR-02: 让合成reasoning fixture在索引结清后进入erase

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / v0.30.90 D 已观察。FIX04 的 v0.30.89 D 已观察后按本卡重建索引结清（不 pop 旧 stash。真实云 nextest `219d5a37-340f-4a92-900d-3f1c52112e7f` 1 passed，fixture `b5c92ec6-90ec-4a3e-a1c4-2faa9b2ae259`，清理在场景断言传播前完成。导出模块 `6bbd9880-c208-4fd8-aba3-20371cce847c` 15 passed。默认全量 `58f94c1b-0af6-4909-aaec-559cc382a2ac`：10822 passed，0 failed，22 skipped；`rename_limit_warning_exit_nine_over_dirty` 第一次失败后重试通过，不是 reset TRY1。F2 仍 unknown。实现审查 `0a4e9544-9fe1-47f7-ae29-b409040a0ec3` VERDICT PASS，HELPER_ONLY yes，LIVE_NOT_SKIP yes，RELEASE_BUMP_ALLOWED yes。D 已观察成功：base `40406196457`、CodeQL `40406196519`、release `40406331881`，SHA `98852b7`）

**Description:** 在 `seed_checkpoint` 中既有writer marker退休成功后、`conn.close()`之前建立有界、可失败的索引结清前置（在调用方直接INSERT索引及CLI erase之前），保留原删除保护和所有reasoning隐私/生命周期断言；不是修改生产erase为等待或重试。

**Out of scope:** 不改HistoryManager/ClientStorage生产API，不删除/重放marker以造绿，不归因历史C2或机器负载。结构oracle由FIX-LTR-01、MCP teardown由FIX-LTR-03承接。

**Current evidence:** 现有QUAL-LTR-FLAKY在新旧来源各出现一次；本轮metadata OID689bb4bc1a6da590532651d8db132eef0b0037b3仍有durable marker，删除栅栏正确返回WouldBlock。`seed_checkpoint`使用capture_scope=None，未等待后台索引；同仓`agent_checkpoint_export_test.rs:741-747`已有有界等待先例。失败当时为何未drain尚未证明。

**Acceptance criteria:**

- [x] 使用现有有界后台任务等待，界限不超过已有同类fixture的10秒；超界明确失败，不sleep固定延迟或重试erase。
- [x] 等待返回后只读枚举本fixture `.libra/object-index-repair/`，目录不存在或为空才通过，存在任何marker或枚举错误均失败；不扩大私有常量/失败计数生产API。消费者失败留下marker时必须失败，安全诊断仅OID/类型/文件名，不打印body/secret。
- [x] 原erase及GC、reasoning私密refs/export和坏digest断言保持，生产删除栅栏与队列实现不改。
- [x] 两个直接消费者均验收：完整agent_checkpoint_export_test模块及真实云reasoning生命周期用例；真实云缺凭据不可skip通过。

**Verification:**

- [x] `source .env.test && source .env.live-test && cargo nextest run --test command_test -E 'test(command::agent_checkpoint_export_test::)' --no-fail-fast --retries 0`；按实际完整模块listing非零逐名核对。run `6bbd9880-c208-4fd8-aba3-20371cce847c`：15 passed，0 failed。
- [x] `source .env.test && source .env.live-test && cargo nextest run --features test-live-cloud --test agent_cloud_tombstone_test -E 'test(=reasoning_artifact_mirrors_and_erases_with_checkpoint)' --retries 0`；精确1例真实D1/R2，保留fixture唯一repo UUID及cleanup证据，禁止early-return冒PASS。run `219d5a37-340f-4a92-900d-3f1c52112e7f`：1 passed，141.309s，fixture `b5c92ec6-90ec-4a3e-a1c4-2faa9b2ae259`。清理在场景断言传播前 `expect` 成功，不是 early-return skip。
- [x] 检查失败路径仍fail-closed：保留marker不擦除、不放宽erase断言；只读核该helper两个直接消费者及无新增生产API。新增确定性回归如必需，须在同一行为轴/I中先登记selector并审查，不以重复跑同一flake代替因果。helper 相对 ff30d4a 只在 marker 退休后、`conn.close()` 前增加结清；残留 marker 或枚举错误使 fixture 失败，erase 实现未改。

**Full-suite trigger:** T-4/T-5：修改tests/helpers共享测试基建，A及最终C完整默认套件；真实云selector另跑，不被默认feature的PASS替代。

**Dependencies:** FIX-LTR-01、FIX-LTR-04；仅消费具名发布后来源与当前卡实际准入，前驱未complete不开始本卡发布。

**Deliverables:** N/A；测试修复及本计划同卡记录完整交付。

**Implementation write set:** `tests/helpers/reasoning_artifact.rs`；本计划文件；`docs/development/plan/plan-status.md`仅本计划行（以实际发布上游文件为基线）。原scoped-traces三治理文件的未发布增量仅由协调者另行本地同步，不进入本卡发布分支。

**Release write set:** Inherited；以当前compat_version_surface_sync为权威的Cargo.toml/install.sh/install.ps1等实际版本面、工具链Cargo.lock及artifact；开工重核，不手改lock。

**Files likely touched:** 与I/R完全一致，不新增target或公共helper注册。

**Docs and compatibility impact:** 本计划及plan-status本计划行同卡更新真实状态；用户CLI/参数/输出/schema没有变化，双语命令指南、COMPATIBILITY、错误码与网站N/A。tests/INDEX既有目标名称/用途不变；若实际发生变化先扩写I并ER-03，不能漏文档。

**Rollback mode:** `immutable-release`

**Migration and rollback:** 无schema/数据迁移；未发布源码保留精确差异，已发布问题只用新签名提交/新patch前滚，不回退ref或版本tag。

**Security and privacy:** 只使用合成fixture与具名测试资源；不读取真实provider正文，不在证据中保存env值/token。进程清理必须证明归属，云资源按fixture唯一namespace，不清扫既有资源。

**Performance budget:** 原A1 FAIL/STOP、恢复A2实际2483.782秒与C1实际5051.049秒STOP全部保留；原C1剩余不再可用，本候选不授A3/C2。只有FIX04实际C/D与原生受控整合后，新的ER03按来源/版本/行为轴审定有限carry与新具名C预算；原10秒fixture、nextest原retries2/timeout不改。

**已保存候选验收 / root（本轮事实登记）：** helper SHA5025cbd4004e47fd508f4b28efe1bd6788444ff923d638b0b9fc008aec48f5b7仍在七文件保存像；A1于2427.745秒FAIL/STOP保留。A2由实际执行R2审查2130b342准入，于08:53:46.754733Z唯一claim、09:35:10.536517Z终态，2483.782/10800秒；fmt/strictClippy、模块15、真实云1、完整10761/21原ignored实际通过。A2有1个原生retry-success：bounded_discovery_outer_cancellation_kills_pipe_holding_descendant，原因未证；38标记中37既有覆盖缺口、1缺interpreter负控，不冒称全live。自己的本地Claude R2 b5c96cc9于09:48:52Z ER05/H0/VERDICT PASS，raw output e8485ca7f464c9fd4e9dfbacc45c17b824fd68fb54dfc2e3c0c144f331c2c255。它们只保留原b73候选来源，FIX04之后不能自动当新树PASS。C1于10:09:46.616474Z唯一claim，11:33:57.665096Z STOP，5051.049/10800秒；完整10761无失败/重试/flake、21原ignored，模块15/真实云1和fmt/clippy通过；release出现一项dead_code涉及两个同步构造器。root据严格项目warning门停止正在构建的原协调器，terminal只记RuntimeError/coordinator interrupted15；native build数值exit未记录、不伪填0/101/不称已完成，安装未执行。terminal SHAe389574db8bccda8e8467763f4017612ba0c3896996ab4c5b03026e579973311。root在原窗口5350.267秒内核七失败树文件、精确pidfd清理4031071并确认gone/reaped，owned/new-orphan/target0，实际云16D1表零/R2空。未提交/推送/Release；旧余量不复用，C2未授权。

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`；本卡自己的完整C/D，不免bump、不合并其它修复。

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=合成checkpoint fixture的索引结清前置; recovery=immutable-release前滚本卡单轴测试修复; complete=yes; self-contained=yes; AC=4/8; VER=3/8; landing=1; prod-files=0; scope=S; deps=FIX-LTR-01,FIX-LTR-04; writeset=五张实现卡本计划与R串行; release=independent; split-from=新上游资格20261025实际FAIL/STOP; exception=N/A`

### Task FIX-LTR-03: 回收MCP无helper断言用例拥有的暂停worker

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / v0.30.91 D 已观察。FIX02 的 v0.30.90 D 已观察后实现 owned worker teardown（发布 `98852b791d553e72277230dde5bc23377e2ed089`。base `40406196457`、CodeQL `40406196519`、release `40406331881` 均成功。MCP 目标 `6e211dc4-14f9-4ce0-9732-ceb0dfa3e03e` 16 passed。精确用例 `fa7ba635-8b22-47ff-9af0-d0f09da6e66d` 1 passed。network `d4b03bd4-1542-4e2d-a427-8b2aee137781` 3 passed，0 skipped。doc exit 0。默认全量 `3b39b1e1-910d-4479-b1bf-2e49962a42dd`：10822 passed，0 failed，22 skipped。F2 仍 unknown。实现审查 `b11fc563-8b6f-485a-b467-30787bf750fc` VERDICT PASS，OWNED_CLEANUP yes，NO_NAME_KILL yes。未修 P2：`RISK-QUAL-MACOS-CHILD`（`proc_listallpids` 返回个数，不能再除以 4）、`RISK-QUAL-CLEANUP-SIGKILL`（只发 SIGTERM）、`RISK-QUAL-WIN-PARENT`（Windows 父 pid 不刷新且未比较创建时间）、`RISK-QUAL-HELPER-THREAD`（`descendant_pids` 仍只读主线程 children，原有行为）。D 已观察成功：base `40426098305`、CodeQL `40426098171`、release `40426201816`，SHA `e24c88bb4c6b61b228d40f4505b6096064826829`，tag v0.30.91。最终资格窗口在这棵已发布树上执行：fmt check 0，clippy `-D warnings` 0（仅 proc-macro-error2），全量 `8bb4fff2-e7b3-4f04-a8ff-70c4b179b58d` 10822 passed、0 failed、22 skipped，doc exit 0，network `990e6bd6-d0f8-4a9e-a371-83a59b870482` 3 passed、0 skipped。不解除 LTR10 incoming，不改 H1，scoped-traces 仍 0/26）

**Description:** 仅修当前MCP测试的owned worker/父进程清理，成功、断言panic及可归属的失败路径均回收；不改变产品worker生命周期。

**Out of scope:** 不加生产PDEATHSIG、不改worker deadline/内存限制/子进程禁止语义，不扩大到其它MCP功能；不能声称重建本轮未保存的PID文件。其余两个独立缺陷由FIX-LTR-01/02承接。stdout-holder fixture的KillHold早期失败缺口尚未排期，具名RISK-QUAL-HOLDER；若本轮资源审计命中则STOP并ER-03登记修复，不吞掉该失败。

**Current evidence:** 本轮full退出后遗留原session3874874的worker PID4064241，PPid1、独立PGID、deleted fixture cwd；root经pidfd核身份后SIGTERM，进程实际消失。精确原case未采集，不能以时间重合证明。静态`read_worker_status_spawns_no_helpers`在703-704行仅kill/wait父进程，而exec.rs:141让worker拥有独立进程组，pause hook永久等待，构成明确清理缺口。原遗留进程也不能排除stdout-holder fixture；其KillHold在191-204行前的失败路径无guard、kill -9不核身份是另一处已知静态风险，不声称本卡能证明历史PID来自哪个case。

**Acceptance criteria:**

- [x] 保留descendant_pids(worker).is_empty原断言；取得worker PID后先建立具有身份核验的cleanup guard，再执行可能panic的断言。
- [x] 成功/panic以及PID取得前后的失败路径均处理当前测试拥有的父进程与可证明归属的worker；不信任复用PID、不杀其它用例或系统进程。
- [x] 保留pause文件发现上限10秒，cleanup退出观察上限1秒；先向核身份的owned worker/其独立组发退出信号，再kill+wait MCP父进程，避免先杀父丢失归属；不把父进程wait成功当worker清理证明；Linux核 `/proc/<pid>/stat` 的starttime/state，区分已退出僵尸与活进程并另记是否已回收，不以kill(pid,0)独证；其它平台用实际进程身份/退出能力，不能照搬procfs或跳过原测试；现有descendant_pids在非Linux上恒为空，不能当归属证据，若当前I/现有平台API不足以建立安全身份，必须在claim前ER-03。PID文件取得前先枚举MCP父进程的真实子关系并核身份（Linux覆盖所有 `/proc/<mcp>/task/*/children`），再执行清理；不能先杀父进程再猜孤儿归属。不提高测试超时。
- [x] 完整agent_mcp_exec_test通过，原deadline、内存、slot、禁止helper断言全部保持；本卡bump后C全量/doc/network通过，实际本卡Claude ER-05通过；五张实现卡D之后的计划最终资格属于计划完成门，不倒挂本卡done条件。全量 `3b39b1e1-910d-4479-b1bf-2e49962a42dd`：10822 passed，0 failed，22 skipped。审查 `b11fc563-8b6f-485a-b467-30787bf750fc` VERDICT PASS。

**Verification:**

- [x] `source .env.test && source .env.live-test && cargo nextest run --test agent_mcp_exec_test --no-fail-fast --retries 0`；完整目标按listing逐名核验。run `6e211dc4-14f9-4ce0-9732-ceb0dfa3e03e`：16 passed，0 skipped。
- [x] `source .env.test && source .env.live-test && cargo nextest run --test agent_mcp_exec_test -E 'test(=read_worker_status_spawns_no_helpers)' --retries 0`；精确1例。成功路径核owned PID/start identity/PGID与exit；失败路径逐分支静态审查spawn/write/try_wait/读PID/parse/panic及Drop顺序，不宣称已做错误注入运行；不运行无界循环或额外重试。若当前I不足以确定安全归属，先ER-03停止，不能退化为按进程名批量kill。run `fa7ba635-8b22-47ff-9af0-d0f09da6e66d`：1 passed。成功路径要求 worker 是已观察子进程、starttime 未变、且在 Linux 上 pgrp 等于自身 pid；父进程 wait 之后再次读取 worker 身份。失败路径由 `Drop` 回收，没有做错误注入运行。
- [x] `source .env.test && source .env.live-test && cargo test --doc`；原doc目录/计数/ignored与实际退出核实。exit 0：2 passed / 3 ignored，以及 10 passed。
- [x] `source .env.test && source .env.live-test && cargo nextest run --features test-network --test network_remotes_test --no-fail-fast --retries 0`；精确原3例，无skip/过滤。其oracle分别是refs、有限PACK标头、LFS batch JSON，不冒称完整对象下载。run `d4b03bd4-1542-4e2d-a427-8b2aee137781`：3 passed，0 skipped。

**Full-suite trigger:** T-5：本计划最后一个独立修复，A及最终C完整默认套件并完成doc/network；不是把前三卡聚合为一次发布。

**Dependencies:** FIX-LTR-02；仅消费具名发布后来源与当前卡实际准入，前驱未complete不开始本卡发布。

**Deliverables:** N/A；测试修复及本计划同卡记录完整交付。

**Implementation write set:** `tests/agent_mcp_exec_test.rs`；本计划文件；`docs/development/plan/plan-status.md`仅本计划行（以实际发布上游文件为基线）。原scoped-traces三治理文件的未发布增量仅由协调者另行本地同步，不进入本卡发布分支。

**Release write set:** Inherited；以当前compat_version_surface_sync为权威的Cargo.toml/install.sh/install.ps1等实际版本面、工具链Cargo.lock及artifact；开工重核，不手改lock。

**Files likely touched:** 与I/R完全一致，不新增target或公共helper注册。

**Docs and compatibility impact:** 本计划及plan-status本计划行同卡更新真实状态；用户CLI/参数/输出/schema没有变化，双语命令指南、COMPATIBILITY、错误码与网站N/A。tests/INDEX既有目标名称/用途不变；若实际发生变化先扩写I并ER-03，不能漏文档。

**Rollback mode:** `immutable-release`

**Migration and rollback:** 无schema/数据迁移；未发布源码保留精确差异，已发布问题只用新签名提交/新patch前滚，不回退ref或版本tag。

**Security and privacy:** 只使用合成fixture与具名测试资源；不读取真实provider正文，不在证据中保存env值/token。进程清理必须证明归属，云资源按fixture唯一namespace，不清扫既有资源。

**Performance budget:** A一次10800秒（含最终全量/doc/network）；C一次10800秒（含bump后完整全量/doc/network）；不提高原测试timeout或retries。一次claim，非预期失败STOP；具体执行器与输入manifest在执行前实审。

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`；本卡自己的完整C/D，不免bump、不合并其它修复。

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=MCP read-worker测试夹具完整teardown; recovery=immutable-release前滚本卡单轴测试修复; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=0; scope=S; deps=FIX-LTR-02; writeset=五张实现卡本计划与R串行; release=independent; split-from=新上游资格20261025实际FAIL/STOP; exception=N/A`

## 测试入口、追溯与里程碑（本轮恢复）

| 任务/里程碑 | 当前来源与入口 | 覆盖与失败边界 |
|---|---|---|
| FIX05 | F1源缺陷、R1指出缓存/写入顺序/配置busy设计缺口；先VER-6与ATTRIB、用户决定和本地ER03 | 粒度/方案未准入；独立default/keyring release及完整A/C/D，不豁免未知F2关闭 |
| FIX04 | FIX02 C1 release警告→源审ce6fb162→ingress两cfg；capture完整lib模块、compat_agent_architecture_guard、default/keyring release及完整默认 | A/C各具名新窗口、原timeout/retries不改；warning仍有即STOP |
| FIX02恢复 | 已保存A2/ownR2和C1 STOP→FIX04实际C/D→新ER03 source/version/C | 不借C1余量，不盲carry b73资格；helper/两消费者保持原断言 |
| 修复里程碑 | FIX01(done)→FIX05→FIX04→FIX02→FIX03分别独立完整C/D | 发布串行，不能合并、不能以手工worker清理当FIX03修复 |
| 最终资格/HANDOFF | FIX03真实merge树10800秒fmt/clippy/full/doc/network，再终态文档及PR/main D与例行补记 | 未全部满足不解除原LTR10 incoming；H1/旧merge签名政策不豁免 |

## 完成判据与交接

> **现行 v2.16 补充判据（2026-10-31 05:54:24 UTC；R3/R4 均 FAIL，待 R5 审查采纳）：** 下段五张旧卡的版本化 C/D 要求仅用于历史已发布事实归属，不倒改原发布结果。新的来源资格须在 fetch+rebase 最新上游后的冻结最终树上，另开受审窗口完整运行双 env nextest、doc、原三例 network、nightly fmt、严格 Clippy，并取得本机 Claude 来源资格 PASS；适用 PR/main D 与旧 `release/v0.30.92` 同 SHA base D failure 的归属都须闭合。必要的新 FIX 卡只提交代码，不 bump/tag/Release；失败/STOP、F2 关闭条件与 H1 等独立前驱保持。以下 2026-10-26/28 的版本化段落按其原时点保留。

五张实现卡分别实际A/B、Claude ER-05 PASS、精确signed/DCO提交、独立patch/PR/merge/tag/人工Release、实际CI与发布artifact D完成；全部P0/P1关闭，P2具名处置。FIX-LTR-03 的C④在bump后树执行完整默认套件、`cargo test --doc`及原3例network；merge树与被测树不等即STOP。五张实现卡各自实际D完成后，在与FIX03发布merge树相同的冻结来源执行上述一次10800秒计划最终资格窗口（fmt、strict Clippy、双env完整默认套件、doc、原3例network），核全部身份/退出/ignored/flake/runtime skip与owned资源；根因未知的缺口不得改成已证。最终实际本地Claude来源资格PASS后，才把五个具体发布SHA/版本及适用覆盖交给原scoped-traces计划；其H1、原merge签名政策、LTR自身A/B/C/D仍独立。

当前（2026-10-28）：来源资格**未闭合**。F2 P1 未关闭（用户已于 2026-10-28T06:03:22Z 决定限定关闭条件，尚未满足）；FIX05–FIX03 的 C 与 e24c88b 最终资格窗口原件取不到；最终源树（现为 29eaa97 之上）已不同于 e24c88b，须重新执行最终资格窗口；没有来源资格 Claude 审查原件；六个提交与四个附注 tag 签名未验证；当前 main 的 D 尚未由本 Agent 提交取得。详见文首 2026-10-27 段。下一句为 2026-10-26 时点记录。

当前（2026-10-26）两原计划正式23/118、0/26、23/144；本外部五实现卡 5/5（FIX01、FIX05、FIX04、FIX02、FIX03 的 D 已观察），DOC01/02另记，不增加原26卡。最终资格窗口已在 FIX03 发布树 `e24c88b` 上通过。终态文档审查 `bde085f1-c67e-439c-bdac-df02fce6b97c` VERDICT PASS。终态文档 `e5424c5114331612cb2f02206b18aafe112a2afb` 的 main D 已观察成功：base `40433225564`、CodeQL `40433225610`。这是例行补记。F2 仍 unknown。不解除 LTR10 incoming，不改 H1。联合依赖按文首ATTRIB→FIX05、FIX05/新FIX→CLOSE→FIX04→FIX02→FIX03与H0/H1/LIVE/LTR10核无环；任一卡/末端证据不足均未完成。旧17/118、1/4统计保留于下方2026-10-25快照。

> 历史初稿统计：原Mega14/112、Libra0/26、联合14/138；外部0/3，保留其原时点。

## 修订记录

| 日期 | 触发 | 变更 | 实际状态 |
|---|---|---|---|
| 2026-10-31 07:54:50 UTC | GOV R4 对 608efc3 旧输入准入 PASS/P0P1=0/P2×7 | 本计划 07:43 行已依 R4 P2-6 补 07:30 行修正事实与 R3 原 diff SHA；新 GOV P2 字节待最终验收/Claude | 来源交接和 H1 仍 blocked；旧 D FAIL 保留，无 C/提交/push |
| 2026-10-31 07:43:04 UTC | GOV R3 对 cfa 旧输入准入 PASS/P0P1=0/P2×9；远端前滚 608efc3 | 七文件仓外备份、原生 fetch/rebase、plan-status 三向保留上游 IFP-01 与本地治理；P2 前滚修订待新输入审查；本计划 07:30 行按 R3 P2-4 更正“DEP 已于 07:15 登记”，原字节由 R3 输入 diff SHA256 `22ee7528009d7efa2ab1716284768cf1527055753d5e56cbadc09d35806c3415` 保全 | DEP-QUAL-CODEONLY-GOV/HANDOFF 均 incoming，旧 D FAIL/H1 保留，无 C/提交/push |
| 2026-10-31 07:30:54 UTC | GOV 准入 R2 实际 FAIL/P0=0/P1=1/P2=9；R1 FAIL/P0=1/P1=4 保留 | GOV 本轮把 VER 拆为 17 门、AC 修为 17 项、scope=M，保持 07:15 已登记的 DEP-QUAL-CODEONLY-GOV 为 incoming，并处置 R2 P2；七治理文件待 R3 准入，资格原门不减 | DEP-QUAL-CODEONLY-GOV 与 DEP-QUAL-HANDOFF 均未解除；旧 D FAIL/H1 保留，无提交/推送 |
| 2026-10-31 07:15:00 UTC | GOV 准入 R1 实际 FAIL/P0=1/P1=4 | 撤回未准入额外 cargo test 必过门，保留 T-5 双环境 nextest 与旧 DEFER；登记本计划 code-only 交付 DEP | 来源资格未取得，旧 D FAIL/H1 不变 |
| 2026-10-31 07:06:18 UTC | GOV 执行门 R1 实际 FAIL/P1=1 | 起草独立治理卡，旧 R9 PASS 只覆盖其原输入，后续全 C/Review/D 待 | 无提交/推送，不解除交接 |
| 2026-10-31 06:19:29 UTC | 本机 Claude R6 实际 FAIL/P1×1：status/long 的 05:54 旧行被混入 06:09 改号说明 | 两旧行精确恢复 R5 字节，改号与 R6 失败仅由本次新时点前滚登记；05:20 初稿行标清 05:22 版本更名、06:09 原因补记两次动作。R3～R6 失败原件保留，R7 待审 | `DEP-QUAL-HANDOFF` producer outgoing 未解除、consumer incoming 保持，无新验收/提交/发布 |
| 2026-10-31 06:09:09 UTC | 本机 Claude R5 实际 FAIL/P1×1（06:08:06 UTC），初稿版本更名时间线缺口 | 05:20:08 初稿误用 v2.15、原生 rebase cfa 后 05:22:01 UTC 更名 v2.16 的回填时间已明示；producer `DEP-QUAL-HANDOFF` outgoing 未解除、consumer incoming 保持，旧五卡版本化 C/D 与 v0.30.92 base D FAIL 仍按原 SHA 归属；05:54:24 为上一轮登记时刻、R5 manifest 05:54:56 绑定其后终态，现行前滚 R6 待审 | 未开新 A/B/C/D 窗口、未提交或发布，LTR-10 仍阻塞 |
| 2026-10-31 05:54:24 UTC | 本机 Claude R3 实际 FAIL/P1×3/P2×14（终态 05:42:17 UTC）；R4 实际 FAIL/P1×2（终态 05:53:09 UTC）后的资格合同前滚 | R3 三项 P1 已在 R4 实核关闭；补现行 DEP-QUAL-HANDOFF、PUBLISH/F2-GATE 与最新上游冻结最终树判据，明确 producer outgoing 未解除 / consumer incoming 保持、旧同 SHA D FAIL 不被 Release success 覆盖；旧版本化完成判据保留历史原文，R4 新 P1 时间线已前滚登记。R5 尚待审 | `DEP-QUAL-HANDOFF` 未解除、LTR-10 不准入；无新修复卡验收/提交/发布 |
| 2026-10-31 05:20:08 UTC（初稿时间；v2.16 字样于 05:22:01 UTC 更名后回填，撞号原因于 06:09:09 UTC 补记） | 用户指定后续 Libra 仅代码提交；重核 v0.30.92 同 SHA release-branch base D failure | 初稿误以 v2.15 编号，与上游已生效的 v2.15 migration waiver 撞号；原生 rebase cfa86fa 后于 05:22:01 UTC 更名 v2.16。旧已发布卡历史合同及结果不倒改；未闭合来源资格按 v2.16 code-only 前滚、实际 PR/main D，原最终全量/doc/network 与本机 Claude 门保留；失败原因待受控归属 | `DEP-QUAL-HANDOFF` incoming，LTR-10 不准入，未开新验收窗口或修复卡 |
| 2026-10-29 UTC（DOCS R3 后） | FIX06-ER05 R3：DOCS FAIL（P1×1，带存储配置文字的进程内病例实为 4 例） | 4 例改正并补 `client_storage_reads_pack_sha1` 及证据行；注明用户作答时依据的是 3 例；DEFER 行措辞、1e5310c 重叠说明、摘要与 plan-status 前提、卡内观测点与快照路径改正；`commit.py` 去除剩余 assert 并防止重跑覆盖恢复副本 | 待 DOCS 复审后提交 |
| 2026-10-29 UTC（R5 后） | DEC-QUAL-R4-BASELINE、DEC-QUAL-INPROCESS-DEFER、上游 1e5310c、R5、FIX06-ER05 R2 FAIL、DEC-QUAL-FIX08-NEXTEST-RULE | 记录发布代码对照与 DEFER-QUAL-INPROCESS-ISOLATION（含 4 例存储配置文字与 4 例仅最终树失败）、1e5310c 同步（plan-status 补丁重放）与 R5 结果、FIX-LTR-08 失败处置与关闭条件改为只以 nextest 与 C/D 为依据、提交与网站步骤；措辞与时间改正 | FIX-LTR-06/07/08 in-progress；待 DOCS 复审后提交 |
| 2026-10-29 UTC（R4 后） | FIX-LTR-08 ER-05 PASS、green3、R3/R4 全量、上游 ba38e30、FIX06-ER05 R1 FAIL | 记录 green3（`fetch_update` 改 `fetch_add`）、R3 与 R4 结果、第七项用户决定 DEC-QUAL-R4-CARGO-TEST、ba38e30 同步（plan-status 补丁重放）；补齐审查日志十行与 RISK-QUAL-FIX08-QUERY-BUSY-TEST；FIX-LTR-06 卡两处 R6 措辞就地改正；R2 归属、doctest、Mega2 证据与时间措辞改正 | FIX-LTR-06/07/08 in-progress；待 FIX06-ER05 复审后提交 |
| 2026-10-29 UTC | FIX-LTR-06 A 全量两次失败、用户决定与上游 21f8e4c | 登记 2026-10-28/29 执行记录、六项用户决定、blaze 使用说明、FIX-LTR-06 更正、FIX-LTR-07/08 两卡、依赖边、四个 RISK、审查记录与 FIX-LTR-05 系列注记 | FIX-LTR-06/07/08 in-progress；A 全量待在三卡合并树重跑；依赖保持 incoming |
| 2026-10-28 09:16:58 UTC | 本地 Claude R8 FAIL（P1×1） | plan-status 本计划行最新前缀改为当前状态并改正 main 时点；squash 推断改为「squash 或 rebase」；R7、R8 入审查日志 | DOC-LTR-03 in-progress；F2 仍 unknown/P1 |
| 2026-10-28 09:10:06 UTC | 执行器自查与本地 Claude R7 FAIL（P1×2） | 改正 FIX-LTR-06 证据表与文档影响中「`docs/commands/zh-CN/fetch.md` 缺对应句」的错误说法（该文件第 29 行已有对应句；首次 R7 因此中止，无结论）；8abdcf7 时间改为提交时间 06:07:23Z 并引 PR merged_at 原件；授权、整合范围、FIX-LTR-06 源锚点与生命周期括注改为当前事实；两条索引前缀注明 main 时点 | DOC-LTR-03 in-progress；F2 仍 unknown/P1 |
| 2026-10-28 08:58:36 UTC | root 授予 Cargo 窗口（`coordination/root-grant-20261028T074904Z.json`），开工条件要求刷新 upstream | 快进到 8abdcf7（PR #649，仅文档）；三份文件的新增文字按锚点重放，与受审字节逐行相同；文首段补 8abdcf7 事实一句 | DOC-LTR-03 in-progress；F2 仍 unknown/P1 |
| 2026-10-28 07:09:02 UTC | 本地 Claude R5：PASS，P0/P1=0，P2×5 | 在 R6 前修订全部 P2：status 文本模式例外的两句文档入写集、`--cached` 用例锁定 `freshness`=`cached`、用例计数、Description 补 repoid 副作用、审查日志 R3/R4/R5 行 | DOC-LTR-03 in-progress；F2 仍 unknown/P1 |
| 2026-10-28 06:58:22 UTC | 本地 Claude R4：DOCS PASS、ER03 FAIL（P1×1） | status 预解析改为仅结构化输出并丢弃返回值；AC-7 覆盖无提交与缓存新鲜的 `--cached` 并加文本不变量；性能说明、status 文档「配置解析从不告警」句与注释、空 bucket 措辞、VER-1 说明、命令出处与审查日志行序改正 | DOC-LTR-03 in-progress；F2 仍 unknown/P1 |
| 2026-10-28 06:49:07 UTC | 本地 Claude R3：DOCS FAIL、ER03 FAIL（P1×2），R2 的 P1 已关闭 | FIX-LTR-06 把 status 列为无条件写集（模式分发前预解析对象存储），AC-7 改用 `LIBRA_STORAGE_TYPE=local` 并覆盖 `--cached`，全局库不可读时 status 保持 128；用例改名与 RED 分层；清单计数改为 18；补 root 副本读取命令出处；R3 入审查日志 | DOC-LTR-03 in-progress；F2 仍 unknown/P1 |
| 2026-10-28 06:36:00 UTC | 本地 Claude R2：DOCS FAIL（P1×2）、FIX-LTR-06 ER-03 PASS、F2 门落地 PASS | 二进制对应限定到 db331cd 并说明 29eaa97；补存 GitHub 核验与 root 副本 origin/main 原件；GATE 行补当日接受发布事实与关闭审查输入/键；FIX-LTR-06 加 AC-7（status 承诺）、四个具名用例、条件写集、退出码迁移说明；新增两条 P2 | DOC-LTR-03 in-progress；F2 仍 unknown/P1 |
| 2026-10-28 06:20:00 UTC | 本地 Claude R1 FAIL（P1×5）与用户三项决定 | 保留两处原行首标签；修正重跑时间与 12 个提交的日期；补存写者、root 副本、祖先、vault、keyserver 与 F2 场景原件；FIX-LTR-06 改写警告去向与 0→9 退出码、补齐 fetch/cache 文档写集；登记 29eaa97 上游变化与用户决定 | DOC-LTR-03 in-progress；F2 仍 unknown/P1 |
| 2026-10-27 16:30:00 UTC | 用户指定 Claude 执行 Agent 补齐交接缺口 | 登记写者交接、五组复核结论、FIX-LTR-06 与 DOC-LTR-03、四个新依赖；FIX05 卡勾选不一致与 F2 当前可证缺陷入文；不改任何旧结论 | R1 FAIL，见上一行 |
| 2026-10-26 19:12:37 UTC | 历史 TRY1 stderr 不可恢复 | GitHub Actions 中该 reset 测试只有 PASS。会话 c7f9fbae 不在本机。b73 上两次自然重跑通过。诱发的 libra.db 排他锁退出 128，stderr 是 LBR-IO-001 JSON，不是 TRY1 的 129 再解析失败。关闭审查 a73f768c 与 3d7f3b15 都是 CLOSE unknown、FIX04_ALLOWED no、VERDICT PASS。用户未选择提供 stderr、新卡或修订。本次修订只允许 FIX04 加两行 cfg，同时 CLOSE 保持 unknown，P1 不关闭。不把 FIX04 写成 F2 修复。审查 a929a323 VERDICT PASS。这是执行器依当前目标记录的修订，不是用户批准。若 FIX04 的 C 重现 TRY1 形状则 ER10 STOP | in-progress；F2 仍 unknown |
| 2026-10-26 15:16:12 UTC | 父卡 VER-1 至 VER-5 与缓存/pack 审查 d9641b0d PASS | ae50414 上 VER 计数 93、62、51、58、54，均 0 failed。完整套件 df24361e：10822 passed，0 failed；fetch mirror 用例重试后通过。fmt 与 clippy 通过。default 与 keyring release 只有 from_payload 与 from_payload_with_hint_policy 两条 dead_code，外加 proc-macro-error2。确认失败不进对象缓存。fetch 在安装 pack 前拒绝。精确 local 只在 init 保持历史本地；fetch、pull、clone 仍拒绝精确 local。VER-1 没有同步屏障真锁，覆盖是把仓库库换成目录，记为 P2 缩窄。RISK-QUAL-ADD-LOOSE：操作日志用 init_local 写快照，gcs-canary 的 add 仍留下 loose blob 45b983be，index 未写；未修，不宣称所有命令零对象写入。F2 仍 unknown。本提交占用 v0.30.88。不开始 FIX04 | in-progress/本地 C 已过；tag 与 D 未观察 |
| 2026-10-26 14:23:00 UTC | 完整套件被未决 TYPE 挡住对象读取 | 仓库库打不开仍失败关闭。全局库存在但打不开时，对象存储回到历史本地后端。解密失败仍失败关闭。审查 9e386060 与 ce4eb432 PASS。完整套件 run db0184d9：10822 passed，0 failed。不消耗 v0.30.88。不开始 FIX04 | in-progress/空；发布 D 未观察 |
| 2026-10-26 13:08:00 UTC | 05Z 实现审查 4a42ec63 PASS | 未决 TYPE 不再构造 LocalStorage。VER-1 两次 8/8，VER-2 3/3。fmt 与 clippy 通过。不消耗 v0.30.88。不开始 FIX04。FIX-LTR-05 仍 blocked | in-progress/空；实现审查 PASS |
| 2026-10-26 12:51:00 UTC | 登记 FIX-LTR-05Z 草案 | 具名决定要求未决 TYPE 这一次调用不再构造 LocalStorage。GlobalSchemaFuture、Ok(None) 和正好 local 保持。源码未改。不消耗 v0.30.88。不开始 FIX04 | blocked/空；待 ER03 |
| 2026-10-26 12:48:00 UTC | 05Y 实现审查 a2a2af14 PASS | 未决 TYPE 不进入对象缓存。VER-1 两次都是 6 通过。fmt 与 clippy 通过。不消耗 v0.30.88。FIX-LTR-05 仍 blocked | in-progress/空；实现审查 PASS |
| 2026-10-26 12:35:00 UTC | 05Y ER03 db7a5d7f PASS | 准入不缓存未决 TYPE。补上 TYPE 只读一次，以及 VER-1 包含 storage_config_resolve_unresolved_type_stays_local。类型一直读不到时，每次 objects_storage 都重新解析。不消耗 v0.30.88。FIX-LTR-05 仍 blocked | in-progress/空；实现待审查 |
| 2026-10-26 12:29:00 UTC | 登记 FIX-LTR-05Y 草案 | 未决且不是 GlobalSchemaFuture 的 TYPE 读取仍返回今天的本地结果，但不得写入对象缓存。下一次 objects_storage 重新解析。源码未改。不消耗 v0.30.88。FIX-LTR-05 仍 blocked | blocked/空；待 ER03 |
| 2026-10-26 10:58:00 UTC | 05X 实现审查 bd0e229d PASS | 16425b1a 的 search_result 早退已补上。不消耗 v0.30.88。FIX-LTR-05 仍 blocked | in-progress/空；实现审查 PASS |
| 2026-10-26 10:57:00 UTC | 05X ER03 8059d2f4 PASS 后实现 | open_init_storage、拒绝状态、缓存不插入、四个正式上转。VER-1 两次 7/7，VER-2 5/5。clippy 0。不消耗 v0.30.88。实现审查待 | in-progress/空；实现审查待 |
| 2026-10-26 10:56:00 UTC | 05X 审查 25a7c300 FAIL | Description 改为 init 调用 open_init_storage。抽出不读 TYPE 的 open_after_type_read。local 仍先读 bucket。空切片补上两个 trait 批次探测。源码仍未改 | blocked/空；待复审 |
| 2026-10-26 10:55:00 UTC | 05X 审查 79404cf2 FAIL | init 与四个上转共用只读一次 TYPE 的 open_init_storage。local 仍是 Warning 加可写本地。resolve_configured_storage 不改，fetch 对 local 的 Err 仍是 05W。源码仍未改 | blocked/空；待复审 |
| 2026-10-26 10:54:00 UTC | 05X 审查 1c072454 FAIL | 布尔查询的子进程覆盖补全。Result 入口改走同一个枚举取用。TYPE=local 保持旧降级。checkpoint retire 失败仍返回 ObjectWrite。删除因此变死的 repo id 函数。源码仍未改 | blocked/空；待复审 |
| 2026-10-26 10:53:00 UTC | 05X 审查 2586a99a FAIL | checkpoint 的确认失败先 retire marker 再返回 ObjectWrite。AC-2 补全 Result 方法。Ready 的 delete_payload、heal、evict_local 保持 trait 默认。正式上转只留四处。源码仍未改 | blocked/空；待复审 |
| 2026-10-26 10:52:00 UTC | 05X 审查 cf0d2286 FAIL | 补上既有 impl Storage、trait 默认方法，以及全部已点名的 dyn Storage 上转。布尔签名保持不变，拒绝状态不返回 false 或空 Vec，改为退出兜底。源码仍未改 | blocked/空；待复审 |
| 2026-10-26 10:51:00 UTC | 登记 FIX-LTR-05X 草案 | `init` 的确认失败改为拒绝状态且不进缓存。不新增 Storage。三个旧查询改为 Result。源码未改。表内上一行是 10:50:00，本行取下一分钟，起草时本机时钟仍早于 10:50 | blocked/空；待 ER03 |
| 2026-10-26 10:50:00 UTC | 05V 准入 session 95af8652 与实现审查 72196c7f | 冻结 fetch AutoFollow 批量探测及 pull/clone 的 IoReadFailed 映射；focused 测试已通过；不占用 v0.30.88；FIX-LTR-05 仍 blocked | in-progress/空；实现审查 PASS |
| 2026-10-26 10:30:00 UTC | 05U 准入 session c1d476b0 与实现审查 bc600c5b | 冻结 remote.rs 与 tiered.rs 的 search_checked；离线策略不访问远端；focused 测试已通过；不占用 v0.30.88；FIX-LTR-05 仍 blocked | in-progress/空；实现审查 PASS |
| 2026-10-26 10:05:00 UTC | 05T 准入 session 986c352a；实现审查 de5171c1 FAIL 后 02aa713c PASS | 冻结 storage trait、local、client_storage、am；Windows 测试模块改为 cfg(all(test, unix))；不占用 v0.30.88；FIX-LTR-05 仍 blocked；tiered 搜索记为 DEFER-QUAL-05-TIERED-SEARCH | in-progress/空；实现审查 PASS |
| 2026-10-26 10:00:18 UTC | 05W ER03 session c6cf48c1 PASS；实现审查 2ee621eb PASS | 实现 resolve_configured_storage 与 fetch 写入前检查。init 未改。cf38ef33 与 040f1008 仍是 FAIL。cargo test 与 nextest e0629e44 都通过特征测试。不消耗 v0.30.88 | in-progress/空；实现审查 PASS |
| 2026-10-26 09:40:00 UTC | 05S 准入 session 71cf457d 与实现审查 d6849287 | 冻结 client_storage.rs 的索引补登记与修订后缀类型过滤；focused 测试已通过；不占用 v0.30.88；FIX-LTR-05 仍 blocked | in-progress/空；实现审查 PASS |
| 2026-10-26 09:28:36 UTC | 05W 审查 session 6f0a9998 FAIL | 补上 TYPE 确认为 s3 或 r2 之后 8 个解析 Err 必须返回 Err。GlobalSchemaFuture 的本地降级只留在 TYPE 读取。不支持的 type 单独返回 Err。未决 TYPE 读取仍有 command 测试。源码仍未改 | blocked/空；待复审 |
| 2026-10-26 09:20:00 UTC | 05R 准入 session 54f3e8cf 与实现审查 146781d3 | 冻结 snapshot.rs、store.rs、operation restore.rs、repack.rs；focused 测试已通过；不占用 v0.30.88；FIX-LTR-05 仍 blocked | in-progress/空；实现审查 PASS |
| 2026-10-26 09:08:05 UTC | 05W 审查 0ccb6203、22682963、aa6093ac 均 FAIL | 函数名为 resolve_configured_storage。只有确认请求了 remote 之后才 fail-closed。未决的配置库读取保持原来的本地 fetch，不新增用户决定。repoid 只有确认 NotFound 才无前缀。源码仍未改 | blocked/空；待复审 |
| 2026-10-26 08:56:59 UTC | 配置失败后端仍不能用说谎的 Storage；登记 05W 草案 | 新增 FIX-LTR-05W。当时使用的 try_init 名称已被后续修订替换。未 ER03 PASS，源码未改；v0.30.88 仍只属于 FIX-LTR-05 | blocked/空；待准入 |
| 2026-10-26 08:10:00 UTC | 05Q 准入 session 3de3e11b 与实现审查 cf7454d4 | 冻结 show.rs、maintenance.rs、restore.rs、object_ext.rs；四个 focused 测试通过；不占用 v0.30.88；FIX-LTR-05 仍 blocked | in-progress/空；实现审查 PASS |
| 2026-10-26 06:40:00 UTC | 准入复审 f5697874 要求补齐计数、依赖边与 file.rs 验证 | FIX-LTR-05 写明拆出 05P 与 05Q；05P 写集含 file.rs，VER-5 覆盖 obliterate 错误传播；05Q 仍未冻结不准入；现行顺序见「实施顺序与发布窗口」 | blocked/空；源码未改 |
| 2026-10-26 06:20:00 UTC | ER03 session e52e4025 要求先拆卡 | 新增 FIX-LTR-05P，只含 client_storage 可失败类型探测及 fsck/cat-file/obliteration 的 exist_checked 迁移；66 文件全迁不进本卡；v0.30.88 仍只属于 FIX-LTR-05 | blocked/空；源码未改，待本卡 Claude 准入 |
| 2026-10-25 18:59:44 UTC | 退出R1实际四PASS与精确native退出exit0 | 四版本回到.87、0664/stash/源树及9+9+4保护已核，真实C失败/STOP保留；实际终态待增量单审 | blocked/locally-accepted；无active bump或新窗口，资格1/4 |
| 2026-10-25 18:47:47 UTC | FIX04 C1全命令0但真实native审查FAIL/root STOP | 新增仅四个未发布版本面的精确退出候选，保存7路径失败树；两P1与独立恢复待，不授新A/C或聚合 | blocked/locally-accepted；退出待Claude，资格1/4 |
| 2026-10-25T14:24:41.918607+00:00 | FIX04自身实际Claude R1终态及root采纳 | ER05/H0/VERDICT三PASS、73输入、五P2具名处置；新增事实只限既有两治理，原FAIL/STOP及A1 flakes不变 | locally-accepted；最终C/D未取得，资格1/4 |
| 2026-10-25 14:16:54 UTC | FIX04唯一A1/B实际终态及root资源后检 | 两cfg源B、十阶段及10761身份核验；保留2个原生flake与runtime缺口、旧STOP | locally-accepted，自身ER05/C/D待；资格1/4 |
| 2026-10-25T12:03:44.446299+00:00 | FIX02 C1实际release项目警告STOP；source审SOURCE PASS/RECOVERY FAIL两P1 | 新增独立FIX04、精确生产I特许、A/C release profile门、原C1 STOP/保存像及新顺序/终止链 | ER03/H0候选，未授权新A/C或正式源码 |
| 2026-10-24 19:47:13 UTC | 新上游资格full实际FAIL、既有flake复现与手工清理的worker | 三个独立测试修复、隔离发布与原计划incoming交接 | 本地Claude ER-03/H0待审；没有执行或发布 |

## 任务卡粒度审计表

| 卡 | 行为轴 | I（测试+同步文档） | AC/VER | landing/prod | 依赖 | release/C-D |
|---|---|---|---|---|---|---|
| FIX-LTR-01 | doctor完整结构oracle | global_config_schema_future_test.rs、本计划、plan-status本计划行 | 3/8、3/8 | 1/0 S | DEP-QUAL-SOURCE/SECURITY/PUBLISH | independent/self |
| FIX-LTR-05P | 存储读取失败不再被布尔存在性探测说成对象不存在 | client_storage.rs、fsck.rs、cat_file.rs、obliteration/mod.rs、file.rs 及本卡测试、本计划、plan-status 本计划行 | 6/8、5/8 | 4/5 M；file.rs 只消费 classify_presence 的 Result | FIX-LTR-01 + DEP-QUAL-SOURCE | family child；不占用 v0.30.88；已推送 8a115ba；实现审查 dc579753 PASS |
| FIX-LTR-05Q | show、maintenance、restore 和 Blob::save 的读取失败不再当成对象不存在 | show.rs、maintenance.rs、restore.rs、object_ext.rs 及本卡测试、本计划、plan-status 本计划行 | 4/8、4/8 | 4/4 M | FIX-LTR-05P | family child；不占用 v0.30.88；已推送 c0373cb |
| FIX-LTR-05R | 操作日志快照、恢复和 repack 的读取失败不再当成对象不存在 | snapshot.rs、store.rs、operation restore.rs、repack.rs 及本卡测试、本计划、plan-status 本计划行 | 4/8、4/8 | 4/4 M | FIX-LTR-05Q | family child；不占用 v0.30.88；已推送 80ef760 |
| FIX-LTR-05S | 索引补登记和修订后缀不再把存储读取失败当成对象不存在 | client_storage.rs 及本卡测试、本计划、plan-status 本计划行 | 2/8、2/8 | 2/1 S | FIX-LTR-05R | family child；不占用 v0.30.88；已推送 6177646 |
| FIX-LTR-05T | am 的本地对象搜索不再把不可读目录当成缺失 | storage/mod.rs、local.rs、client_storage.rs、am.rs 及本卡测试、本计划、plan-status 本计划行 | 2/8、1/8 | 4/4 M | FIX-LTR-05S | family child；不占用 v0.30.88；已推送 e728736 |
| FIX-LTR-05U | tiered 与 remote 的前缀搜索不再把列举失败当成缺失 | remote.rs、tiered.rs 及本卡测试、本计划、plan-status 本计划行 | 2/8、1/8 | 2/2 S | FIX-LTR-05T | family child；不占用 v0.30.88；已推送 1d3c8d2 |
| FIX-LTR-05V | fetch AutoFollow 不再把存储读取失败当成缺失标签 | fetch.rs、pull.rs、clone.rs 及本卡测试、本计划、plan-status 本计划行 | 2/8、1/8 | 4/3 M | FIX-LTR-05U | family child；不占用 v0.30.88；已推送 835cfc5；session 95af8652 已准入 |
| FIX-LTR-05W | fetch 在写入前拒绝读不到的对象存储配置 | client_storage.rs、util.rs、fetch.rs、pull.rs、clone.rs、fetch/pull/clone 测试、fetch 文档、COMPATIBILITY fetch 行、本计划、plan-status 本计划行 | 8/8、3/8 | 4/5 M | FIX-LTR-05V | family child；不占用 v0.30.88；已推送 2a80a10；ER03 c6cf48c1 PASS；实现审查 2ee621eb PASS |
| FIX-LTR-05X | init 在确认失败时不再构造本地存储 | client_storage.rs、util.rs、intent.rs、clean.rs、subagent_content.rs、checkpoint.rs、fetch 与 cache 文档、本计划、plan-status 本计划行 | 8/8、2/8 | 4/6 M | FIX-LTR-05W | family child；不占用 v0.30.88；ER03 8059d2f4 PASS；实现审查 bd0e229d PASS |
| FIX-LTR-05Y | 未决的对象存储类型不进入对象缓存 | client_storage.rs、util.rs、cache 与 fetch 文档、本计划、plan-status 本计划行 | 5/8、1/8 | 2/2 S | FIX-LTR-05X | family child；不占用 v0.30.88；已推送 ec98926；ER03 db7a5d7f PASS；实现审查 a2a2af14 PASS |
| FIX-LTR-05Z | 未决的对象存储类型不再构造本地存储 | client_storage.rs、util.rs、cache 与 fetch 文档、COMPATIBILITY fetch 行、本计划、plan-status 本计划行 | 4/8、2/8 | 2/2 S | FIX-LTR-05Y | family child；不占用 v0.30.88；ER03 272c8d7d PASS；实现审查 4a42ec63 PASS |
| FIX-LTR-05 | 配置失败与对象存储fail-closed（未准入候选） | client_storage/util/fetch候选及卡内逐文件同步集 | 8/8、6/8 | 3文件候选；落点/规模pending VER-6 | FIX01 + 05P 至 05Z + SOURCE/PUBLISH + F2-ATTRIB + v0.30.88决定；05Z不足以解除阻塞 | independent/self；不得先实施 |
| FIX-LTR-04 | 同步ingress测试构造器profile可用性 | ingress.rs仅两cfg、本计划、plan-status本计划行 | 4/8、5/8 | 1/1 S | FIX-LTR-05 + F2-CLOSE + SOURCE/PUBLISH；2026-10-26T19:12:37Z 起 CLOSE=unknown 豁免 | independent/self |
| FIX-LTR-02 | reasoning fixture索引结清 | reasoning_artifact.rs、本计划、plan-status本计划行 | 4/8、3/8 | 1/0 S | FIX-LTR-01/FIX-LTR-04 | independent/self |
| FIX-LTR-03 | MCP owned worker teardown | agent_mcp_exec_test.rs、本计划、plan-status本计划行 | 4/8、4/8 | 1/0 S | FIX-LTR-02 | independent/self |
| FIX-LTR-06 | 结构化输出下存储配置降级警告的输出通道（ER-03 候选） | client_storage.rs、status.rs（仅结构化输出时在模式分发前预解析）、reset_test.rs、output_flags_test.rs、fetch/cache/status 中英文、COMPATIBILITY fetch 行、网站 fetch/cache、本计划、plan-status 本计划行 | 7/8、5/8 | 2/2 S | FIX-LTR-03 + SOURCE/PUBLISH + CARGO-WINDOW | independent/self；候选 v0.30.92 |
| FIX-LTR-07 | replace -d 用例夹具的数据库状态 | replace.rs（单个用例）、SRC_SERIAL_REGISTRY/CENSUS 各一行、本计划、plan-status 本计划行 | 3/8、4/8 | 1/0 S | DEP-QUAL-SOURCE；FIX-LTR-07 -> FIX-LTR-06 | G-07 用户例外 DEC-QUAL-FIX07-RELEASE（随 v0.30.92）/FIX-LTR-06 |
| FIX-LTR-08 | 仓库库被占用时存储配置读取的进程级有界重试 | client_storage.rs、fetch/cache 中英文文档、COMPATIBILITY fetch 行、本计划、plan-status 本计划行 | 5/8、4/8 | 1/1 S | DEP-QUAL-SOURCE、DEP-QUAL-CARGO-WINDOW；FIX-LTR-08 -> FIX-LTR-06 | G-07 用户例外 DEC-QUAL-FIX08-RELEASE（随 v0.30.92）/FIX-LTR-06 |
| DOC-LTR-03 | 来源资格交接复核登记 | 本计划、plan-status 本计划行、plan-20261015 DEP-TR-QUAL-REPAIRS 行 | 4/20、3/20 | 0/0 S | SOURCE + CARGO-WINDOW（仅 VER-3） | no-release / FIX-LTR-06 |

上表是开工时的粒度快照，其中「未准入候选／不得先实施」不是当前状态。当前完成状态以文首任务表为准：五张实现卡的 D 均已观察。

## 风险登记

**现行profile警告 / root：** P2-QUAL-RELEASE-DEADCODE的旧接受仅解释v0.30.87历史，本轮用户严格项目warning指令取代该放行，由FIX04独立关闭；只排除proc-macro-error2第三方future提示。DOC-LTR01旧句“默认debug build如出现capture-ingress警告”保留为历史措辞，现已证明该警告只在默认release出现，DOC的debug build无该警告不是修复证据。新FIX04 A及C必须实际跑default/keyring release并对项目warning失败；不得用dev Clippy/测试PASS代替。


| ID | 事实/边界 | root处置/重启条件 | 状态 |
|---|---|---|---|
| RISK-QUAL-ORIGINALS-MISSING | FIX05/04/02/03 的 C、e24c88b 最终窗口与 bde085f1 原件在另一机器，用户确认取不到 | 不补造；以最终树新窗口与新的来源资格审查作新证据 | open，P2 具名 |
| RISK-QUAL-SIGNING-UNVERIFIED | 3F5F84D5… 与 FD937770… 签名的提交/tag 无可得公钥 | 不改写历史、不移动 tag；取得公钥后再验 | open，P2 具名 |
| RISK-QUAL-TAGGER-IDENTITY | v0.30.88–v0.30.91 附注 tag 的 tagger 为 `user <user@example.com>` | 不移动 tag；后续发布核对 tagger 身份 | open，P2 |
| RISK-QUAL-GOV-PUSHED | 12 个原工作区治理提交已以新 SHA 推送到 main，早于 LTR01 承载 | root 处置 LTR01 承载关系；不 force、不回退 | open，owner root |
| RISK-QUAL-GIT-MAINT-LOCK | `init_from_git_test` 的执行前快照可能含 Git 后台维护的 `maintenance.lock`，CI TRY1 失败、TRY2 通过 | 若阻断门则另立 FIX 卡；不放宽断言 | open，P2 |
| RISK-QUAL-ENVELOPE-WARNINGS | 待渲染警告缓冲只由 status 与 agent import 渲染，通用成功信封与错误信封不带 | 尚未排期；FIX-LTR-06 不扩大到信封契约 | open，P2 |
| RISK-QUAL-IGNORE-WARN | `src/utils/util.rs:3880`、`:3889` ignore 告警在结构化输出下写原始 stderr | 尚未排期 | open，P2 |
| RISK-QUAL-TEXT-WARN-EXIT | 文本模式下存储降级警告不计入 warning 追踪，`--exit-code-on-warning` 仍退出 0，与结构化模式（FIX-LTR-06 后退出 9）不对称 | FIX-LTR-06 保持历史文本行为并写入文档；尚未排期 | open，P2 |
| RISK-QUAL-STATUS-MODES | FIX-LTR-06 的 status 预解析位于模式分发前，`--scan`、`--check-dirty` 与已测的默认/`--cached` 共享该调用，但未单列用例 | 若后续改动分发顺序则补用例；尚未排期 | open，P2 |
| RISK-QUAL-FIX05-VER-GAP | FIX05 卡 done，但 VER-6 未勾、VER-1 同步屏障真锁未落地；VER-4 与 AC-3 的缺口由 FIX-LTR-06 AC-5/AC-1/AC-2 承接 | 不回改历史勾选；若存储配置锁故障复现则另立 FIX 卡。2026-10-29：已复现并立卡 FIX-LTR-08（真锁用例已落地），关闭条件见该卡 | open，P2，owner Claude执行Agent |
| RISK-QUAL-RAW-STDERR-OTHER | `refuse_rejected_presence`（纯文本加 exit 1）与 `src/cli.rs:3397` init 转换重试警告写原始 stderr | 尚未排期 | open，P2 |
| RISK-QUAL-INDEX-SELF-CONTENTION | 后台对象索引登记（每对象一个连接、一次写提交）与前台存储配置读取争用同一仓库库 | 后果自 FIX-LTR-08 起受进程级 5 秒重试预算约束；争用本身（批量化、事务合并、WAL）尚未排期 | open，P2 |
| RISK-QUAL-GLOBAL-BUSY-DEGRADE | 全局配置库忙时存储配置读取退回本地并告警（0.30.87 起即如此）；含 RISK-QUAL-STASH-QUIET 中可能来自全局库的「退回本地」部分 | 尚未排期；FIX-LTR-08 只处理仓库库 | open，P2 |
| RISK-QUAL-STORAGE-MIGRATE-BUSY | 需要 schema 升级的仓库库在连接时就地迁移；`run_builtin_migrations` 丢掉忙文字，该处忙仍立即 fail-closed | 尚未排期 | open，P2 |
| RISK-QUAL-HOOK-STORAGE-WAIT | 托管 hook 若经间接路径初始化存储，FIX-LTR-08 后最多多等约 5 秒；已知生产 capture 写入用 `init_local_existing` | 尚未排期；间接路径未穷举 | open，P2 |
| RISK-QUAL-FIX08-QUERY-BUSY-TEST | FIX-LTR-08 查询分支的判忙只有构造出的 anyhow 单测与 diag2 的真实观测，没有真实 DbErr 用例 | 尚未排期；闭合观测点为 FIX-LTR-06 全量与 C/D | open，P2 |
| DEFER-QUAL-INPROCESS-ISOLATION | 本机进程内 `cargo test --all` 在发布代码与三卡最终树上都失败（blaze 两次未能跑任何测试；R4 的进程内运行在 `command_test` 中途超时，其后约 100 个二进制未在进程内跑过）：发布代码两次 lib 运行合计 28 例（排除未构建 helper 的 42 例），最终树两次合计 21 例，其中 17 例在发布代码上同样失败，其余 4 例只在最终树失败（3 例为 capture/StoreFailure 文字、1 例为 workspace 忙碰撞，判为同族是推断）；fsck 模块单独运行两树都通过；两次运行各有 3 例、合计 4 例带存储配置拒绝文字（`rejected_reachability_production_client_rejects_fifo_without_blocking`、`unchanged_replay_verifies_ref_and_all_catalog_objects`、`client_storage_reads_pack_sha256`、`client_storage_reads_pack_sha1`；`run-r4-cargotest/cargo_test_all.stdout`、`baseline-ba38e30/cur-lib2.stdout`），发布代码上 4 例同文字（`baseline-ba38e30/lib.stdout`、`base2-lib.stdout`），依 DEC-QUAL-FIX08-NEXTEST-RULE 不作为 FIX-LTR-08 的失败处置依据；4 例只在最终树失败，判为同族是推断，由 DEC-QUAL-INPROCESS-DEFER 接受；对照基线因 `libra clone` 过慢改为 rsync 副本按 HEAD 重置（`baseline-ba38e30/libra-clone-aborted/ABORTED.txt`）；全部用例在 nextest（每例独立进程）下通过（R4 10836/10836）。原件 `baseline-ba38e30/COMPARISON.md`、`evidence.sha256`、`fix-ltr06/run-r4-cargotest/` | 依用户决定 DEC-QUAL-INPROCESS-DEFER 延期；A 的全量门以 nextest 为准（计划原定的全量命令；blaze 与进程内 `cargo test --all` 依用户决定作为补充，其结果已如上处置），进程内结果作为既有缺陷登记。重开条件：任何以进程内运行为验收门的窗口（含 CLAUDE.md 的 `cargo test --all`、blaze）之前，须另立 FIX 卡处理共享 cwd/env、进程级存储与 capture 状态、SQLite 争用 | open，DEFER，owner Claude 执行 Agent |
| RISK-QUAL-FUTURE | proc-macro-error2现有future-incompat提示；旧Clippy exit0不等于无诊断 | 原件保留；任何适用发布门拒绝则STOP独立归属，不allow压制 | open P2 |
| RISK-QUAL-FLAKE | marker为什么未drain尚未证；fixture边界缺等待有源码证据 | FIX02 已在 v0.30.90 加入有界等待。历史原因仍未证明，不能宣称关闭历史 C2 | open P2 |
| RISK-QUAL-ADD-LOOSE | 操作日志在命令前用 init_local 写快照，忽略 LIBRA_STORAGE_TYPE。gcs-canary 的 add 仍留下 loose 对象 | 不混入已发布修复。若要消除，另立卡 | open P2 |
| RISK-QUAL-STDIN | subagent_content fixture 的 BrokenPipe/时序风险 | 不混入 FIX05。若升级为 P1 或阻断当前门，先另立卡 | open P2 |
| RISK-QUAL-MACOS-CHILD | macOS `proc_listallpids` 返回 pid 个数，FIX03 曾按字节数截断 | 只影响未在本计划 CI 执行的 macOS 分支。另立卡前不宣称该平台已测 | open P2 |
| RISK-QUAL-CLEANUP-SIGKILL | FIX03 回收只发 SIGTERM，1 秒后仍存活就让测试失败，不升级 SIGKILL | 正常路径已由 Linux 精确用例覆盖。若 SIGTERM 被忽略，另立卡 | open P2 |
| RISK-QUAL-WIN-PARENT | Windows 子进程快照的父 pid 不刷新，也未比较创建时间 | 本计划 CI 不跑该分支。另立卡前不宣称 Windows 归属已证 | open P2 |
| RISK-QUAL-HELPER-THREAD | `descendant_pids` 仍只读主线程 children | 原有断言未改。若要覆盖非主线程 fork，另立卡 | open P2 |
| RISK-QUAL-HOLDER | 历史PID不能排除stdout-holder；KillHold早期失败缺口及裸kill -9已知 | root接受为FIX03当前范围外残余风险，尚未排期；若实际资源审计命中或当前路径失败，立即STOP并ER03新增修复，不以手工kill当自动PASS | open P2，非历史PID确定归因 |
| RISK-QUAL-CACHE | 独立路径可能使Cargo重新冷编译，暖缓存时间不保证 | 独占缓存、记录配置；安全复制不可用或需超候选预算则claim前ER03；运行超时STOP不自动续时 | open P2 |
| RISK-QUAL-MERGE | PR期间upstream/tree/版本可能变化；原unsigned merge未处理 | 比较被测tree与merge tree，不等STOP；原merge仅原工作区后续解决，不进入修复祖先 | open，硬门 |

## Claude review log

按用户指令单用本地Claude，替代模板reviewer名称，不降低实审或PASS要求。

| Round | Scope | Result | P0/P1 | P2处置 | Evidence |
|---|---|---|---|---|---|
| R1 | ER03/H0候选 | FAIL | P1-1顺序表漏门、P1-2状态登记缺失、P1-3发布树资格未绑定 | R2逐项修订；残余风险由root具名登记 | session04ce4421-a050-4bf3-a003-7662306ee05f；output SHA256 52d6588527ba5c8a16b96072e6857cf0aa60576ba7bcb28d7bcab270f962f96f |
| R2 | ER03/H0恢复候选 | FAIL | 原三P1已闭；新终态证据/被测树P1 | R3前滚修订，未执行产品窗口 | session e9f0e17d-07b4-40d8-9098-3f2a6e68c857；output 92b3745fc0f02790221e06d6db9f80ce56acab39d945378cfeeb23cab6e6c5ba；H0 PASS |
| R3 | ER03/H0终止链候选 | FAIL | 终态ER05收据无落点 | R4闭合，原FAIL不改 | session 82ca0f53-d8f6-4148-af66-13721b381b95；output 8f15fea4fd997f6157f3a82d323b286a9838b91fe8ef5e960a05be1baaa9876b；H0 PASS |
| R4 | ER03/H0最终恢复方案 | PASS | 无 | root具名逐项处置如下；最终事实字节待DOCS-ER05/H0实审 | session 67756716-0e46-4d90-bf70-fd23dff6b918；output 48f93079f925f4129fa9523b78a5cf2bae123e9aa4fab890be11b9b15dd793a2；manifest 925901de4eac79a2d4a1e12540bfff0bd24417c42158a9606875041ff7b222ed |
| 2026-10-28 R1 | DOC-LTR-03 VER-2 与 FIX-LTR-06 ER-03 | FAIL | P1×5（旧标签被替换、两处事实不符、缺原件、结构化警告去向与 0→9 未声明、文档写集不全） | R2 逐项修订 | session 69c4e992-7764-4b7a-b5c1-b8225cb77266；output SHA256 b6383160cad0ace24af8c46285aec78d2254bc573c4eca659a43e04b3b07a8a2；仓外 review-R1/ |
| 2026-10-28 R2 | 同上，另审用户 F2 门决定的落地 | DOCS FAIL；ER03 PASS；F2_GATE_EXECUTION PASS | P1×2（二进制对应未限定到 db331cd、三处事实缺原件） | R3 修订：补原件、限定说法；P2-1 GATE 行补当日接受发布事实、P2-2 补审查日志、P2-3 关闭审查输入与键、P2-4 status 承诺、P2-5 退出码迁移、P2-6 用例点名、P2-7 发布范围、P2-8 FIX05 缺口具名；P2-9 为审查后新增的仓外候选文件，不视为已准入 | session c4c62348-a826-406a-b8ee-a80314315035；output SHA256 ac5e78b1f0a7b9822dc88648850fe9e8ef0148946277250c990b9ae54ac01cfc；仓外 review-R2/ |
| 2026-10-28 R3 | 同上 | DOCS FAIL；ER03 FAIL；F2_GATE_EXECUTION PASS；R2 的 P1 2/2 关闭 | P1×2（AC-7 在全局库不可读时不可达、条件写集触发不可判定；观察清单计数 16 应为 18） | R4 修订：status 改为无条件写集并在模式分发前预解析，AC-7 改用 `LIBRA_STORAGE_TYPE=local`；计数改正；P2-1 AC-3 加 status 例外、P2-3 用例改名、P2-4 日志指针泛化、P2-5 时间改为 06:36:00、P2-6 补命令出处、P2-7 审计表与文档影响同步；P2-2 模式覆盖与 VER-1 单测说明部分处置 | session 80ddb243-566b-4f3e-baec-efeb3f9ba6ea；output SHA256 e076bfa809c34959607bdf37a9c2caaeaf06cfbf326198894c100a02a54f2a3e；仓外 review-R3/ |
| 2026-10-28 R4 | 同上 | DOCS PASS；ER03 FAIL；F2_GATE_EXECUTION PASS；R3 的 P1 2/2 关闭 | P1×1（无条件预解析改变文本模式与无提交/缓存命中路径，超出行为轴） | R5 修订：预解析只在结构化输出时进行并丢弃返回值；AC-7 覆盖无提交与缓存新鲜的 `--cached`，加文本不变量用例；性能说明改正；status「配置解析从不告警」句与注释同改；`--scan`/`--check-dirty` 具名 RISK-QUAL-STATUS-MODES；P2-3 VER-1 与 VER-3 说法统一、P2-5 命令出处改写为执行器记录、P2-6 日志行序改正、P2-7 R5 冻结清单补回 9 份原件 | session 6637130a-a4be-47d6-98ec-156a4871d7f5；output SHA256 cd1b6b19291de63383da77a53be82b4eb98b3529b04026f17541473ebb898c90；仓外 review-R4/ |
| 2026-10-28 R5 | 同上 | PASS（DOCS、ER03、F2_GATE_EXECUTION 均 PASS）；R4 的 P1 1/1 关闭 | P0/P1=0；P2×5（status 文本模式例外的两句文档、`--cached` 未锁定新鲜缓存、用例计数、repoid 写入副作用说明、审查日志补项） | P2 全部在 R6 前修订：写集补两句 status 文档、`--cached` 断言 `freshness` 为 `cached`、计数改正、Description 补 repoid、R3/R4 行补齐 | session 35c7cfc8-df4f-446e-b006-fa2c2ce54918；output SHA256 42366a851dc936d9cabee3fda6d66406551f269db9d50845996c6ae5ebb4d6f6；仓外 review-R5/ |
| 2026-10-28 R6 | R5→R6 的 P2 修订及全部既有结论 | PASS（DOCS、ER03_FIX_LTR_06、F2_GATE_EXECUTION 均 PASS；R5 P2 5/5 处置） | P0/P1=0；P2×2：写集新增句「无提交或缓存命中时不输出」过宽（配置了 upstream 的缓存命中仍会懒打开存储）、Description「写入该文件」应为本仓 `libra.db` 的配置键 | 具名接受，owner Claude执行Agent：两处措辞在 FIX-LTR-06 实施时于卡片与 status 文档改正，由 FIX-LTR-06 自身 ER-05 核对；本行是该审查之后唯一追加的收据，审查器未读过本行 | session 2a87b009-d469-4c56-b097-ed7e2466cfc6；output SHA256 e6249415ba74e5e386631f203c6e26f260dad41b4e2fa01573910532169c9c88；input manifest SHA256 81f8c114f03f22f0f7bd3f953b22606b0ec563e2136ef289007c889f05790de5；结束 2026-10-28T07:15:22.693280826Z；仓外 review-R6/ |
| 2026-10-28 R7 | DOC-LTR-03 增量重审（快进到 8abdcf7、R6 收据、8abdcf7 事实句、zh-CN fetch 说法改正） | FAIL | P1×2（8abdcf7 时间写作 06:07:24Z；授权、源锚点、整合范围等当前状态未更新） | R8 修订 | session ec3937bd-ea36-4176-aa17-e8764647a513；output SHA256 8c6d8b9cc2bcc55082f3019692b37b79dbe565c365f200962cddfe9f2323a61b；input manifest SHA256 4daa424affac2b1cdd68b371d5dfcbd357dcf8b801fa82000f7a7be5d3454b80；结束 2026-10-28T09:09:05.885947370Z；仓外 review-R7/（此前一次 R7 因执行器自查中止、无结论，见 review-R7-aborted/） |
| 2026-10-28 R8 | 同上第二轮 | FAIL；R7 的 P1 2/2 关闭 | P1×1（plan-status 索引前缀把 06:20 时的 main 写成 29eaa97，与 8abdcf7 06:07:23Z 已在远端 main 矛盾） | R9 修订：前缀改为本地 main 先后并更新为当前状态；squash 推断改为「squash 或 rebase」；R7/R8 入日志 | session 182461cc-3244-4e65-a02f-758fe23153a2；output SHA256 cee6686138de843960bf3601cadd25205cf9149b9220bdc4d081f7513fc1acfc；input manifest SHA256 fd02f371f954a2ac88338d20e8acfe1382bb60752d3b081b1c24d4edb1bec0ac；结束 2026-10-28T09:16:02.673795018Z；仓外 review-R8/ |
| 2026-10-28 R9 | 同上第三轮 | PASS（REQUAL、DOCS 均 PASS）；R8 的 P1 1/1 关闭 | P0/P1=0；P2×3：plan-status 前缀「06:07:23Z 已在远端 main」宜用 merged_at 06:07:24Z；R9 收据行的逐字节核对须留存；plan-20261015 前缀为 06:20 快照（标时，非错误） | 具名接受，owner Claude执行Agent：第 1、3 项下次改行时处理，第 2 项已留存 `doc-ltr03/receipt-append-check.json`；本行是该审查之后唯一追加的收据，审查器未读过本行 | session 469c1dbc-c449-49fd-9bb2-f9ce548be38b；output SHA256 a4a2fd63c1f93695681de1219c2579eb426f118f45574aa39a5f08ce908db962；input manifest SHA256 ff35d342158d855a1692d245d149fa93a1303cdb5fa4325c543ec28634b520ee；结束 2026-10-28T09:20:39.062144241Z；仓外 review-R9/ |
| 2026-10-28 EXEC-DOC03 R1 | DOC-LTR-03 VER-3 执行器准入 | FAIL | P1×4（双 env 加载未故障闭合、超时不终止自有进程树、漏 `CARGO_BUILD_JOBS` 控制、缓存/工具链未在 claim 前确认） | R2 修订执行器与合同 | session a4f6c7c9-677f-432f-bc9d-edd24af9156e；`review-exec-doc03/` |
| 2026-10-28 EXEC-DOC03 R2 | 同上 | PASS | P0/P1=0 | — | session ec473f23-9d71-49ca-bedc-86607ba858b3；`review-exec-doc03-R2/` |
| 2026-10-28 FIX06-A-ADMISSION R1 | FIX-LTR-06 A 合同与执行器准入 | FAIL | P1×2（测试未覆盖 AC-7 两个条件；A 合同与卡片 Performance budget 不一致且未登记更正） | 补 AC-7 用例与 card-correction 第三项 | session 5d0f31e1-a3d2-4803-b625-8b616ebe5e1e；`review-fix06-A-admission/` |
| 2026-10-28 FIX06-A-ADMISSION R2 | 同上 | FAIL | P1×1（GREEN 预算写成「剩余时间」不可执行） | 改为数字预算 1800/1800/6900 | session 446603e8-70a2-40dd-96b6-c4bc7d5d31b9；`review-fix06-A-admission-R2/` |
| 2026-10-28 FIX06-A-ADMISSION R3 | 同上 | PASS | P0/P1=0 | RED0 于 10:21:35Z claim | session 8347d953-77f2-4aa3-abc1-9823f167445f；`review-fix06-A-admission-R3/` |
| 2026-10-28 FIX06-BLAZE-ADMISSION R1 | blaze 全量合同（DEC-QUAL-BLAZE-FULL） | FAIL | P1×3（claim 未要求 GREEN PASS；ra-jit 守护进程与测试进程结束后查不到；失败记录与归属不具体） | R2 修订 | session 9906d342-1397-4685-b5fa-bccfaceab1ed；`review-fix06-blaze-admission/` |
| 2026-10-28 FIX06-BLAZE-ADMISSION R2 | 同上 | FAIL | P1×3（GREEN 真实失败对本窗口结论的影响未写；`blaze_summary.py` 按出现顺序配对会出错；无「是否跑全」标准） | R3 修订 | session e11bf25c-97df-4b61-b237-3e900e272112；`review-fix06-blaze-admission-R2/` |
| 2026-10-28 FIX06-BLAZE-ADMISSION R3 | 同上 | FAIL | P1×2（replace 失败被写成本机既有/特有，证据不支持；`blaze_summary.py` 解析为空时判 PASS） | R4 修订（空解析判 INCONCLUSIVE 并设下限） | session 4c5c73e3-f18b-4308-bd7d-54afd7cfc75c；`review-fix06-blaze-admission-R3/` |
| 2026-10-28 FIX06-BLAZE-ADMISSION R4 | 同上 | PASS | P0/P1=0 | blaze 随后 TIMEOUT（ra-jit 守护进程未就绪） | session 43f98b06-448e-425e-b4dc-c34e944e6b36；`review-fix06-blaze-admission-R4/` |
| 2026-10-28 FIX07-ER03 R1 | FIX-LTR-07 根因与 ER-03 | ROOT_CAUSE PASS；ER03 FAIL | P1×2（串行车道需 cwd+env 并同步两份 tsv；implementation 卡写 no-release 违反 G-07） | R2 修订；用户决定 DEC-QUAL-FIX07-RELEASE | session 2a9df0a2-ced0-4de2-9f34-71aa98593a63；`review-fix07-ER03/` |
| 2026-10-29 FIX07-ER03 R2 | 同上 | PASS；上轮 P1 2/2 关闭 | P0/P1=0；P2 已按卡处置 | 见 FIX-LTR-07 卡 | session dbbe2623-2a1b-4629-a57f-c3eb62ca2ae6；`review-fix07-ER03-R2/` |
| 2026-10-29 FIX07-ER05 | FIX-LTR-07 实现与聚焦 VER | PASS；ER-03 R2 P2 7/9 处置 | P0/P1=0；P2×4（授权依据、AC-1 数字出处、VER-1 RED 引用、计划登记时点） | 写入 FIX-LTR-07 卡与本段 | session ce62cf45-699c-4aac-bed8-1102ebd1f6bd；`review-fix07-ER05/` |
| 2026-10-29 FIX08-ER03 R1 | FIX-LTR-08 根因与 ER-03 | ROOT_CAUSE PASS；ER03 FAIL | P1×5（归属计数、FIX-LTR-05 条款取代未登记、性能上限、迁移路径、用例确定性） | R2 修订 | session fcc17bd6-2e69-4955-a987-7a43f2ea3a63；`review-fix08-ER03/` |
| 2026-10-29 FIX08-ER03 R2 | 同上 | FAIL；上轮 P1 5/5 关闭 | P1×1（锁用例借 cwd 与进程级预算影响同进程其它用例） | R3 改为子进程探针 | session 1a6e61e5-c305-45c7-b8ef-76df876d91f2；`review-fix08-ER03-R2/` |
| 2026-10-29 FIX08-ER03 R3 | 同上 | PASS；上轮 P1 1/1 关闭 | P0/P1=0；P2×5 已写入卡与实现 | 见 FIX-LTR-08 卡 | session 6254861f-e64f-4956-98f8-74cd94f98cdb；`review-fix08-ER03-R3/` |
| 2026-10-29 FIX08-ER05 | FIX-LTR-08 实现、RED/GREEN2、A1/A2 与诊断处置 | PASS | P0/P1=0；P2×7（开发文档、提交基准、行号、查询分支真实错误用例、计时余量、进程内预算共用、措辞） | 处置写入 FIX-LTR-08 卡；随后 green3 | session 7d2e6c72-2dbd-4180-9446-fc99b33e32df；`review-fix08-ER05/` |
| 2026-10-29 FIX06-ER05 R1 | FIX-LTR-06 实现、R4 nextest 证据与本计划治理文字 | ER05 FAIL；DOCS FAIL | P1×2（第三个提交未绑定 R4 被测树哈希；审查日志缺 FIX08-ER05、FIX-LTR-06 A 准入三轮、blaze 准入四轮、DOC-LTR-03 执行器两轮） | 提交脚本绑定最终（R5）被测树逐文件哈希与门文件 `r5-gate.json`；补齐本表与修订记录 | session 289e6052-8188-47d0-86c7-18244eb5d648；`review-fix06-ER05/` |
| 2026-10-29 FIX06-ER05 R2 | 同上，含 DEFER 登记与提交脚本修订 | ER05 PASS；DOCS FAIL；上轮 P1 2/2 关闭 | P1×1（DEFER 绕过 FIX-LTR-08 已登记的失败处置与关闭条件且未披露：进程内失败中 3 例带存储配置拒绝文字） | 依用户决定 DEC-QUAL-FIX08-NEXTEST-RULE 修订 FIX-LTR-08 规则与 DEFER 行；P2 措辞、时间、门描述、修订记录、提交脚本（显式检查、R5 head 与快照绑定、恢复说明）、网站提交步骤已处置 | session 465ccf84-62f2-4a71-b54f-26cee7bbfe32；`review-fix06-ER05-R2/` |
| 2026-10-29 FIX06-ER05 R3 | 同上 | ER05 PASS；DOCS FAIL；上轮 P1 1/1 关闭 | P1×1（带存储配置文字的进程内病例实为 4 例，计划与用户提问写 3 例） | 4 例改正并注明；P2 处置见修订记录 | session 32823329-1f9c-4497-acc0-70169e2e0132；`review-fix06-ER05-R3/` |

R2实际ER03 FAIL/H0 PASS，session e9f0e17d-07b4-40d8-9098-3f2a6e68c857，output SHA256 92b3745fc0f02790221e06d6db9f80ce56acab39d945378cfeeb23cab6e6c5ba；原三P1已闭，新的终态证据/被测树矛盾由本轮预登记终止链修订。**R3时点快照：** 当时R3尚待实际审查；后来R3 FAIL与R4 PASS见实际日志，不倒改原结论。P2-7已实际核GitHub上游存在旧plan-20261015（blob bfcf4e20aff8289dce9c7a0889e5ebb9ddb926f9）；不能因此认为含当前本地治理。新计划改用纯文本说明原计划；发布clone仍须自身链接核验。

### 本轮前滚修订历史

| 时间 | 触发 | 变更内容 | 原卡→新卡 | 受影响引用 |
|---|---|---|---|---|
| 2026-10-24 19:47:13 UTC | 本地Claude R1 FAIL | 同步五门、状态登记窄写集、bump后C及发布后资格树、逐项D与安装缓存界限、风险/审查/粒度表 | 三卡编号/轴不变 | 两仓8份治理、外部新计划、原LTR10顺序/卡片/审计表；R2待审 |

### R3审查修订

| 时间 | 触发 | 变更 | 卡/依赖影响 | 当前事实 |
|---|---|---|---|---|
| 2026-10-24 19:56:18 UTC | R2 ER03 FAIL/H0 PASS | C前冻结pending事实、C到merge零文档漂移，后继补记C/D，v2.14终态纯文档提交/例行补记，FIX03自身与计划完成门分离 | 三卡编号/轴/依赖不变；新增末端文档两路径门与一次3600秒预算，非新产品发布卡 | R3候选；未实施/测试/提交/发布 |

**R2残余P2处置（root）：** 非Linux与PID取得前的身份能力在claim前核，不足则ER03；D的release/**措辞修正。来源存在性现以独立clone HEAD=origin/main=`0ca47cc147b2099ffb835ade91272c4cfe1efd21`为冻结锚点，不能把未绑定ref的旧GitHub查询当精确来源证据。独立clone已核干净、独立SQLite、4702个祖先不含ed566b0/2755eb7；身份已从继承的测试默认值改为Eli Ma <eli@patch.sh>，本地签名开启、公钥1067ADBC49C4E7D18BA154AC21594333739ACBAF，尚无新提交签名验证。旧target927GB、reflink不支持，缓存复制/实际冷编译预算尚待claim前核定，不以方案PASS跳过该门。

**上游发布状态行边界：** 独立clone的plan-status本计划行只写本计划的三卡状态、实际来源/验收/版本/提交/UTC；不复制原工作区的scoped-traces0/26、两仓治理R轮或其未发布状态。本文自身R1/R2/R3实审记录是本独立计划的历史，可以随本计划发布。原工作区本地索引中的交叉前沿只由原Traces承载。

**B证据能力：** 自动检查证明字段/链接/依赖表一致、历史字节保持与写集边界；`published_tree_qualification_bound`等布尔项只是合同存在检查，不能证明未来运行或语义正确。终止链的可执行性由本轮实际Claude审查，未来树/hash与退出由真实执行核验。

### R4前滚与历史纠正

| 时间 | 触发 | 变更 | 结果 |
|---|---|---|---|
| 2026-10-24 20:03:41 UTC | R3 ER03 FAIL/H0 PASS | 终态ER05先审、仅补记收据、最终字节本地门、三树相等；STOP时前一卡C/D落点；例行维护自身门；只读B脚本合同 | R4待真实审查，零产品执行 |

R3真实session `82ca0f53-d8f6-4148-af66-13721b381b95`，output SHA256 `8f15fea4fd997f6157f3a82d323b286a9838b91fe8ef5e960a05be1baaa9876b`，ER03 FAIL/H0 PASS，原件保留。R3的候选生成器把R2已写的两条历史行时间从2026-10-24 19:47:13 UTC改成19:56:18 UTC；本轮恢复原始行时间，仅本轮新行使用新时钟。不倒改R3原件或把其失败写成成功。

**RISK-QUAL-TERMINAL-BASE（root，P2）：** FIX03 merge之后main可能前进，使终态树相对冻结资格树不再只有两路径。必须STOP并ER03审新差异/所需门；不能换基线后继承旧PASS、force或忽略新增来源。该风险可能延长交付，但不降低资格范围。

**签名证据边界（root，P2）：** 新clone仅完成本地身份、公钥与signing设置。首次实际提交后必须在同一个SHA核DCO、gpgsig并以导出公钥实际验签，失败不得push；GitHub签名verification单独按实际回执记录，不把本地开关或新公钥当成GitHub已信任。若实际仓库策略要求额外信任/注册，先解决后发布，不绕过规则。

### R4 PASS后的P2具名处置与治理自身审查

| 时间 | 事项 | root处置 | 边界 |
|---|---|---|---|
| 2026-10-24 20:09:49 UTC | P2-1 ISO历史横幅 | 逐字去时钟比较R1/R2后，将两仓8份文档共同横幅恢复至首次相同文本的原时间；R4未覆盖的ISO时间问题在本轮纠正，UTC两行恢复事实保持。原R1/R2/R3/R4候选及审查输出不改 | 本轮真实修改范围由最终diff/hash绑定，不把R4对旧字节PASS扩大到尚未审的修订 |
| 2026-10-24 20:09:49 UTC | P2-2 review log与旧状态 | 表内补R2/R3/R4真实结论、session/hash，R3待审句标历史快照；当前索引明确R4方案PASS、治理自身ER05待 | 不填未来审查/验收PASS |
| 2026-10-24 20:09:49 UTC | P2-3 后续例行维护 | 维护提交照终态第5项顺序：先文档核验及真实Claude，收据仅写plan-status本计划行，再对最终字节跑相关本地门，核提交/merge/被测树一致。执行前必须在当次维护写集中登记其独立的一次窗口及预算并实审，不挪用本计划终态3600秒、不自动再开；目前维护尚未执行 | 属v2.14例行维护，未获自己的实际执行准入，不影响本次方案审查；缺准入不得执行 |
| 2026-10-24 20:09:49 UTC | P2-4 缓存复制 | 原cache-selection只是开始前的依赖文件子集11178；实际复制31147还包括第三方build/.fingerprint，排除libra-*目录、第一方库/测试二进制、incremental、硬链接。当前复制仍在运行；必须取实际终态、逐项核数/排除与独占来源后才能claim | 缓存不是源码/测试资格，也未消耗或获得新产品验收PASS |
| 2026-10-24 20:09:49 UTC | P2-5 执行期残余 | 后继STOP补记、终态基线变化、同SHA验签/远端信任与终态只读执行器冻结均保留硬门 | 三卡和末端执行均未开工 |

本次R5仅对以上事实修订及两仓治理自身做真实本地Claude DOCS-ER05/H0审查，并确认R4方案准入没有规范回退；结果未取得前不宣称治理自身review完成。两仓原治理写集及MTR01/LTR01承载关系不变，新独立计划未来随自身三卡发布，原工作区仅本地保存。


### FIX-LTR-01 A终态及具名限制（2026-10-24 21:42:59 UTC）

- 单一A claim `2026-10-24T20:33:43.564369Z`，终态 `2026-10-24T21:39:46.332180Z`，3962.768秒/7200秒，执行器与原nextest配置/timeout/retries未改；原资格FAIL/STOP及耗尽的旧窗口保留，没有自动重跑。输入manifest2126项 SHA256 `eb26c80a4cc4b05274a1349b20e8ee59ec5dc56e337bd2aaa0da096b5e363dde`；终态SHA256 `c393a8baaf2cf741e19c1b4cac91fc1ad0bcaf9fd865b865f881670bf284a0cf`；root限定A收据SHA256 `e92bbed501956b8019ecadfae14cfbd2e2c7c6d3d652fdf391fe6baeda64692d`。
- 自身准入来自真实本地Claude session `f8b78ffe-b7d4-4459-a633-333bf24c55b0`，EXECUTION/VERDICT PASS，仅A准入、不是本卡代码ER05。自己的最终代码ER05仍待；未使用隔离Codex。
- 三个执行集合按listing/JUnit逐(target,case)核实：exact1、完整compat_global_config_schema_future54、默认全量10761；各自0失败、0framework skip、0flake，未丢失/额外case。全量10782个列举身份中原21 ignored保持逐名相同。full stderr SHA256 `c22ec3f2f66d6e9e7c3939a03cebffdcaee1e9b8a4fc764e5d19c7c080d4b1de`；JUnit SHA256 `80f85859f67f676cb7632bde4d81cc5a2019b866bfdca7a7d3e7089a3be44953`。本卡唯一测试差异仍为完整字串增加Ui排除，生产cli和所有runtime/startup guard不变；测试文件SHA256 `535f31b6aa50c500ad9fe3876e893e8fb02581caef83a6aef2e23ed13ae000bd`。
- **P2-QUAL-RUNTIME（owner root）：** 当前38个标记与原来源逐身份/文字相同，来源hash逐项核实；1个是`opencode_plugin_skip_visible_when_interpreter_missing`的真实负向assert，不是跳过。其余37个包含：9个default-feature下未启用test-live-cloud、10个显式GitHub live未配置、12个Mega2 browser live未配置、1个Mega LFS endpoint未配置、2个大小写不敏感文件系统前提、1个当前build拒绝stage非UTF8文件、Bun与PowerShell各1个。保持已登记默认全量命令并披露缺口，不称已证明这些行为；本卡exact/完整target没有runtime skip。FIX02必须另跑具名真实云用例；H1/LTR的live门不被本卡覆盖。原件 `/tmp/fix-ltr01-A-20261025/root-terminal-audit.json` 及 `runtime-skip-source-reference.json`；自己的Claude必须独立判断此限定是否充分。
- **P2-QUAL-WORKER（owner root）：** 本次唯一新PPid1进程为PID2074789/start_ticks10678129、session1941756，exe属于本clone且fixture cwd已deleted；命令为read-worker，STATUS_PAUSE=1及本fixture pause.pid、无READ_DEADLINE_MS，匹配当前`read_worker_status_spawns_no_helpers`的唯一环境形状，区别于带2000ms deadline的另一调用点。没有原PID文件，不倒推旧历史PID4064241身份。终态后pidfd绑定身份、单次SIGTERM，`2026-10-24T21:40:25.108156Z`确认退出并已回收，全量baseline差集无其它新孤儿/owned残留。人工清理SHA256 `456b49ad48fc97b83ceb29025f3b465703f5ffb6b609742659dea36cf5dfe70b`；原资源失败事实保留，不宣称自动cleanup通过、FIX03修复或最终来源资格完成。此处仅处置已登记FIX03风险；自己的Claude若判P0/P1则STOP，不以root的P2归属代替PASS。
- **P2-QUAL-COMPILER（owner root）：** strict Clippy exit0；既有`proc-macro-error2 2.0.1` future-incompatibility提示仍存在，不把“零lint失败”写成“零任意诊断文本”。若后续C门对此有新的禁止条件，先独立归属，不静默免门。
- C入口只读预检：远端main仍0ca47cc，三版本面0.30.86一致；账户有push/admin权限但不使用admin bypass，release environments无人工review要求。两clone均没有release target，不能把debug cache冒充release暖缓存；C一次10800秒仍未claim，须以实际冷release条件和本次66分钟A重新进行真实Claude执行准入/预算判断，不在窗口外预编译、不自动延长。草稿 `/tmp/fix-ltr01-C-20261025/run-C.py` 尚未获准或执行。
- FIX02外部条件只读预检在 `2026-10-24T21:30:04.500055Z` 完成：D1仅SELECT常数、R2仅HEAD既有bucket，均HTTP200；无fixture/用户数据读写，无secret值日志。它不是FIX02的真实写入/erase/cleanup验收，更不是本卡full的live覆盖。
- 当前代码完成、A通过；自己的ER05待、提交无、push无、CI/Release无；本卡仍in-progress/locally-accepted，正式0/3。B按implementation无额外类型门，但两文档结构/链接、状态行only与原生产字节保持须在自己的审查前再核。C前只允许追加实际自己的review收据、更新本计划状态行，然后按已登记版本/bump/最终树门执行；没有放行后继实施或借原工作区治理推送。


### FIX-LTR-01 自身ER-05实际收据及C准入条件（2026-10-24 21:54:29 UTC）

- 真实本地Claude于2026-10-24 21:48:29 UTC返回`ER05_VERDICT: PASS`及`VERDICT: PASS`，P0/P1无；session `c7fb3f6b-3738-4d33-830a-be70172cd39f`，输入manifest SHA256 `504664b7f68d4b13f6f02703543fee559db322f45de7580d978f1d40397467e7`，输出SHA256 `8b43a841bf8566e6a3692601e6c0a13643dc49bfe5534005c4ec31a6177d83a1`。原件 `/tmp/fix-ltr01-own-review-20261025`；root实际重算manifest/output及逐输入hash，未漂移。Claude明确披露未自行计算hash，不能把root核验写成Claude独立计算。该结论只覆盖既定源码/文档和已发生A终态，不代表C准入或C/D完成。
- P2-1由root承接：37项runtime early-return缺口只是打印标记识别的下限，不穷举静默early-return或未启用feature；FIX02及最终资格必须保持此限定和具名live门。1项负向assert与37项缺口分开。
- P2-2由root承接：C执行期间如发现与本次A同样形状的已登记FIX03孤儿，仅允许在终态后、核本窗口观察到的PID/start identity、nextest session、独立PGID、read-worker argv、已删除fixture cwd及STATUS_PAUSE/pause.pid/无READ_DEADLINE_MS后，以pidfd单次SIGTERM人工回收并记录exit/reap；不发组信号、不按进程名清扫、不自动重试。任何其它形状、多个未能唯一归属的进程或stdout-holder（RISK-QUAL-HOLDER）立即STOP/ER03；不以手工清理冒充自动cleanup PASS。FIX03后的最终来源资格必须零孤儿。C执行器只记录并拒绝未知残留，不自行执行人工清理。
- P2-3由root承接：保留proc-macro-error2的future-incompat提示，strict Clippy实际exit0不扩称零诊断文本。P2-4的旧候选句/横幅格式在FIX02开工的既定文档写集中标为历史，本次不改已审正文。
- P2-5由root承接：cold release、实际A耗时及C一次10800秒上限必须另获真实本地Claude执行准入；尚未claim、bump或预编译，超时/失败STOP，不追加或转移旧窗口。只冻结本次实际后置收据和本计划status行后，再由C审查绑定最终字节/执行器/环境控制。
- 审查边界披露：Claude为核计数使用Grep读取full.stderr的Summary、两条MCP PASS和FLAKY/TRY计数，并读Clippy提示两行，超出原prompt禁止原始测试输出的约束；未读.env/credentials/JUnit正文。保留原输出及披露，不伪造为完全遵守；后续准入明确允许这些无secret的受限摘要。
- 当前：源码及A通过、自身ER05 PASS，Lifecycle/Acceptance仍in-progress/locally-accepted；提交、push、CI/Release与C/D均未完成，正式0/3。本文收据为审查后的事实追加，不声称Claude事先审过自己的输出；未来C/D由FIX02按既定终止链补记。


### Task DOC-LTR-01: 清除PR635暴露的项目Rustdoc警告

**Task type:** `docs`

**Lifecycle / Acceptance:** `in-progress` / `locally-accepted`（A/B及自己的Claude ER-05已实际通过；signed/DCO 5c449bba已推送并经PR636合并b73e98a，PR/main全部适用D实际通过；完整C/D仍待FIX-LTR-03覆盖，不提前done）

**Description:** 用户要求“在 Libra 的下一个提交修复这些 warning”，随后明确排除第三方 `proc-macro-error2 2.0.1` future-incompatibility。只修PR635实际job 121059262116的104条项目Rustdoc诊断：72私有目标链接、28非法HTML占位符、4冗余显式链接。不改依赖、lock、可见性、运行逻辑、CLI输出或lint等级。

**Out of scope:** 第三方未来相容性警告按用户指令保留；FIX02/03源码、原scoped-traces治理及功能未完成状态不借此关闭。本卡没有版本bump、tag、Release或release workflow。

**Current evidence:** PR635 head2852497、被测tree90ca1d20、squash merge f33be958dcc3266df3a6ab796ed90d1766f3cd93；base40143598901 attempt2在Mechaevo的strict Clippy与Rustdoc均exit0但有104条文档warning。原attempt1在eli-MI缺fontconfig.pc失败保留；用户已移除该不一致runner，不能称已修复该机器。Cargo第三方警告出现两次，不计入104条项目warning。日志 https://github.com/libra-tools/libra/actions/runs/40143598901/job/121059262116 。精确源锚点：`src/command/remote.rs:139`（Add.master，不是set-head）、`src/utils/worktree_blob.rs:15`（私有与冗余诊断同处）、`src/command/auth.rs:66`（相对时间占位符）。

**Acceptance criteria:**

- [x] 私有目标的说明保留为代码字面量，不扩大生产可见性。
- [x] HTML占位符在Rustdoc显示为原意字面量。
- [x] 公开目标的3条冗余显式链接（sandbox/mod.rs:88、d1_client.rs:8/361）改为等价短链接；worktree_blob.rs:15同处私有目标已由AC1覆盖，保留代码字面量。
- [x] 非文档源保持逐字节相同。
- [x] 普通编译的Clap help文档输入与输出保持相同。
- [x] 第三方依赖及lock保持原字节，不抑制lint。

**Verification:**

- [x] `source .env.test && source .env.live-test && RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features`；全部项目Rustdoc警告为0，唯一允许的Cargo级提示是用户排除的proc-macro-error2 future incompatibility，分开记录不称零诊断。
- [x] 精确diff守卫：仅下列53文件的文档注释/文档属性变化；还原普通编译的cfg_attr(not(doc),doc=原文)后，去文档源码与基线逐字节相同；版本、Cargo.lock、依赖、配置、所有测试不变。
- [x] 真实新二进制与已验收0.30.87二进制逐字节比较受影响命令的`--help`/`-h` stdout、stderr、exit：auth login、cat-file、cherry-pick、clone、commit、format-patch、rebase、remote add、show-ref、switch、symbolic-ref。help子进程不加载.env文件，以白名单环境运行在仓外空临时目录：stdout/stderr管道（非TTY），COLUMNS=100、LANG=LC_ALL=C.UTF-8、TZ=UTC、TERM=dumb、NO_COLOR=1、CLICOLOR=0、CLICOLOR_FORCE=0，隔离HOME/XDG/全局及系统config路径。旧binary是已核C树90ca1d20的`/tmp/fix-ltr01-C-20261025/install/bin/libra`，SHA256 `acc1e55fad788f8c809beeb2025b1b839842d51bb8151018571d5742d1ae985c`；新binary来自本卡实际debug build。所有调用必须exit0；debug/release或元数据差异也STOP，不能忽略。

**Full-suite trigger:** `none`（本卡no-release，不自行执行C；完整覆盖仍继承FIX-LTR-03；本卡只有文档注释及cfg(doc)文档属性，没有CLI注册、参数/错误/配置/执行逻辑改动。若普通编译行为/元数据不再等价，则STOP、ER03重分类及所需全量，不能沿用本判定）。

**Dependencies:** `DEP-DOC-PR635`（incoming：精确已合并f33be958/tree90ca1d20可读取；本卡只消费已保存源与本地A/C，不宣称FIX01远端D或整体来源资格已完成）。

**Deliverables:** 下列53源文件内的Rustdoc注释/属性、本计划与plan-status本计划行；不新增仓库证据文件。

**Implementation write set:** `src/cli.rs`；`src/command/add.rs`；`src/command/agent/investigate.rs`；`src/command/agent/review.rs`；`src/command/auth.rs`；`src/command/bisect.rs`；`src/command/blame.rs`；`src/command/branch.rs`；`src/command/cat_file.rs`；`src/command/cherry_pick.rs`；`src/command/clone.rs`；`src/command/cloud.rs`；`src/command/commit.rs`；`src/command/format_patch.rs`；`src/command/gc.rs`；`src/command/init.rs`；`src/command/rebase.rs`；`src/command/rebase/state.rs`；`src/command/remote.rs`；`src/command/rename_detect.rs`；`src/command/rerere.rs`；`src/command/shortlog.rs`；`src/command/show_ref.rs`；`src/command/status.rs`；`src/command/status/input.rs`；`src/command/switch.rs`；`src/command/symbolic_ref.rs`；`src/internal/ai/agent_bridge/vcs.rs`；`src/internal/ai/agent_run/event.rs`；`src/internal/ai/agent_run/workspace_strategy.rs`；`src/internal/ai/capture/ingress.rs`；`src/internal/ai/capture/live.rs`；`src/internal/ai/capture_scope.rs`；`src/internal/ai/history.rs`；`src/internal/ai/hooks/runner.rs`；`src/internal/ai/hooks/runtime.rs`；`src/internal/ai/observed_agents/builtin/gemini.rs`；`src/internal/ai/observed_agents/reasoning.rs`；`src/internal/ai/observed_agents/redaction.rs`；`src/internal/ai/sandbox/mod.rs`；`src/internal/ai/sandbox/proxy.rs`；`src/internal/ai/sandbox/runtime.rs`；`src/internal/alternates/mod.rs`；`src/internal/auth.rs`；`src/internal/branch.rs`；`src/internal/db.rs`；`src/internal/sequencer/mod.rs`；`src/internal/upgrade/txn.rs`；`src/internal/workspace.rs`；`src/utils/client_storage.rs`；`src/utils/d1_client.rs`；`src/utils/redact.rs`；`src/utils/worktree_blob.rs`；`docs/development/plan/plan-20261025-qualification.md`；`docs/development/plan/plan-status.md`仅本计划行。

**Release write set:** `N/A`。

**Files likely touched:** 与I相同。53源文件中只改Rustdoc；Clap消费的HTML占位符文档使用成对cfg_attr(doc)/cfg_attr(not(doc))保留普通编译原始doc字符串，不能改arg属性或产品分支。

**Docs and compatibility impact:** 正式Rust API文档由源注释生成；CLI普通help逐字保持，不增加/删除命令、参数、配置或用户语义，因此双语命令指南、COMPATIBILITY、错误码、网站内容N/A。任何实际help变化先STOP，不能宣称文档-only继续。

**Rollback mode:** `immutable-release`

**Migration and rollback:** 无数据迁移；已推送只以精确后续提交前滚，不重置或force。保留原warning日志与两次CI事实。

**Security and privacy:** 不动权限/可见性；只读隔离help，不传真实凭据；审查不读env值，原始证据位于仓外受控目录，仓内仅本计划摘要。

**Performance budget:** 一次新的本卡A窗口3600秒（含nightlyfmt、strict all-target/all-feature Clippy、上述Rustdoc、debug build、help比对、B及资源检查）；不运行全量、不占用旧FIX01或FIX3预算，不自动重跑。与Mega C的Cargo/构建/提交/push/发布严格串行。失败STOP并保留真实终态后ER03。

**Estimated scope:** `M`；按G-11文档-only的≤2人日口径，53源文件文档+2治理文件，预计≤1人日；不以生产代码卡的5文件上限伪装53文件实现。

**Version increment:** `N/A`

**Release boundary:** `no-release`；先精确signed/DCO本地提交（满足用户下一提交约束），可推短期docs分支并经本地Claude PASS、实际PR base/CodeQL后gh squash merge；不自行触发版本发布。作为现行资格计划文档，进入FIX-LTR-03唯一指定覆盖树，不把FIX01的旧C覆盖本次修改。

**C/D coverage from:** `FIX-LTR-03`；本卡自己的PR/main D还须实际核验；FIX03最终C/完整D覆盖本卡后才done/complete。在此前只可locally-accepted，不新增资格实现卡计数（原三卡仍分开计0/3）。

**Granularity:** `type=docs; axis=项目Rustdoc诊断收敛并保留CLI help; recovery=文档精确前滚; complete=yes; self-contained=yes; AC=6/20; VER=3/20; landing=1同主题Rust API文档集; prod-files=0; scope=M(≤2人日); deps=DEP-DOC-PR635; writeset=与FIX02/03共享计划行串行; release=no-release; split-from=用户指定PR635 warning后续提交; exception=N/A`

**ER01/02/03与消费规则：** 独立原生clone，当前branch docs/pr635-rustdoc-warnings来自f33be958，不含原工作区unsigned merge或未发布治理。准入R2与执行器R2实际PASS后已采纳55路径，A/B实际通过；自己的Claude ER05仍须绑定当前最终源码及下述实际终态，通过后才精确提交。source候选的diff自动守卫不等于真实Rustdoc/二进制验收。模板G-03文档清单与G-11时间口径适用；没有G08/G12/G13聚合。root唯一写者与发布者。FIX02/03依赖不改，不制造DOC与覆盖点的执行依赖环；最终资格收口同时核本文档卡覆盖。

**后置事实落点：** 本卡提交前冻结最终文档字节；自己的审查/A/commit/PR/main收据由FIX02的既有本计划/状态行写集补记，FIX03 C/D后补记覆盖。当前未知项不得预填PASS，后置事实不声称已被先前审查读取；不新建证据目录。若FIX02开始时本卡PR未完成，保持pending并补同步，不机械重跑未变源码。

| 修订UTC | 来源 | 变化 | 状态 |
|---|---|---|---|
| 2026-10-25T03:14:21.976945+00:00 | 用户指定下一提交修PR635项目warning并明确排除第三方 | 新增DOC-LTR-01，53文件文档-only；无依赖升级/版本bump；FIX03承载C/D | 候选待Claude，未实施 |


**DOC-LTR-01开工R1实际FAIL与R2前滚（2026-10-25）：** 本地Claude session `17ca5be0-55a2-4f0f-acc4-3821a4d22635`，ER03 FAIL / DOC_ONLY PASS / 总FAIL；输出SHA `ab500ad0f082f89210ab4e98f2a3cd2f8d661721000bde845c38d4eac07655ca`。两P1已在此候选改正：VER3必须测remote add，AC3区分3个公开短链接与1个私有字面量。R1精确126输入已仓外封存，源码候选53文件字节不变；仍未采纳、未运行A、未提交，R2待审。

- root/P2-1：guard还原成对普通doc后，逐文件行数和每个原诊断行号/位置逐字比较，非文档源码仍逐字相同；最终help证据只能来自22次真实二进制调用。
- root/P2-2：55个前置文件实际用`libra show f33be958:<path>`核blob SHA，HEAD/index/worktree清洁；采纳前再核前像，采纳后允许diff必须恰好55路径，Cargo/lock/配置及测试hash仍全量守卫。
- root/P2-3：VER3具名旧二进制与固定隔离环境；Cargo门仍source双env，help自身用独立白名单env，不继承凭据或真实config。任何输出差异STOP，不删断言。
- root/P2-4/5：B明确包括本计划结构/相对链接、I精确55路径及原历史记录保持；Rustdoc -D warnings承担源内部链接门。A比CI更严格；新lint不扩修，失败STOP/ER03。本次none仅文档注释，不改CLI注册/全局flag。
- root/P2-6：cat_file.rs两处同句`<input>`虽不在104诊断里，Rustdoc会把它当HTML input元素；仅cfg(doc)侧同样按字面量呈现，普通help原字节保持，不作为删减旧诊断计数。
- root/P2-7：首次claim前将执行器/控制项/源树真实Claude审查；A后若只追加收据，限此两治理文件并记录精确diff，最终ownER05仍须绑定实际终态及最终字节。不得在Mega C期间执行本卡Cargo/提交/push/发布。
- root/P2-8：FIX02/03开工来源必须包含本卡实际merge SHA（待发生后填入），FIX03最终C树须含本卡最终55路径；不符STOP/ER03，不将未覆盖的DOC记done。此为来源入场门，不改变原两卡依赖边或制造覆盖循环。
- root/P2-9：上方Current evidence补精确源行。默认debug build如出现原FIX01已审的capture-ingress两associated functions dead_code提示，需绑定未变源码、单列原诊断与具名P2审查，不称零诊断，不用allow或夹带生产修复。本卡目标仍为所给job的104项目Rustdoc警告；第三方警告按用户排除。


| 2026-10-25T03:30:53.098949+00:00 | DOC-LTR-01准入R2 | 两P1已关闭，ER03/DOC_ONLY/总PASS，源码仍未采纳；执行器实审与A/ownER05待 | 准入通过，未验收 |

**DOC-LTR-01准入R2实际收据（2026-10-25T03:30:53.098949+00:00）：** 本地Claude session `b2377d23-968e-469f-ad00-521220bdbdd1`，`ER03_VERDICT / DOC_ONLY_VERDICT / VERDICT: PASS`，输出SHA `47dd9c0d4d3be46a6b3b0d523655bfa1882d378543539f3f7c99be48353f633d`；132输入实核无漂移并仓外封存，原R1 FAIL保留。root具名承接R2六项P2：采纳前程序逐项比较55原生blob前像、采纳后精确55路径；22次真实help比对使用每次全新空HOME且相同固定路径/环境，不能忽略debug/release差异；本收据及状态行补记时点；旧binary先核SHA，缺失STOP；Cargo/构建/提交/push/发布仅在`/tmp/mtr99-C1-20261025/root-local-C-acceptance.json`实际成立后启动；R1 P2处置继续有效。上述仅准入PASS，不是实施、A、ER05、C或D结果。

### DOC-LTR-01 A/B实际终态与最终审查入口（2026-10-25 04:25:21 UTC）

一次A1从2026-10-25 04:08:57 UTC至04:23:30 UTC，实际874.796/3600秒，coordinator exit0、STOP=false。48命令全部exit0：nightly fmt无diff、strict all-target/all-feature Clippy、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features`、debug build，以及11命令×2help选项×旧/新二进制44个进程。22组stdout/stderr/退出码逐字节相同，实际help与CLI语义未变化。四Cargo门加载双env；第三方proc-macro-error2提示按用户指令保留，Clippy/Rustdoc/build的项目warning均0（本次build没有出现先前已登记的capture-ingress dead_code提示）。不称所有诊断为零，不升级依赖、不抑制lint。

53源文件仅文档变化、16组成对cfg_attr保留普通Clap输入；全部2126输入在A期间保持冻结，版本/lock/测试/配置不变。B实际55路径、190相对链接、6AC/3VER通过；剩余owned进程0、新孤儿0、无需人工清理。本卡按已审ER13不运行全量，不能替代FIX03完整C/D。终态SHA256 `72f9bcd758491d27bcbd27280d553897865d16ef4231ce5595281cd45b865847`；命令结果SHA256 `7d471fb2d5ace8d669828784c5eda1f1d2f88c90e4764c0dacbcb6248c8282ce`；help比对SHA256 `ad7afb84a0eb26ae65b37e0625876a5751f113dc832cc5fdfef24f762c5da680`；B SHA256 `34bf4a1954cfd0e936a11c874eb97bac67d1f7115d13a5fe25374000250146b0`；输入manifest SHA256 `b02b8ceadd197d69df767d20c9e95fc716f96fdcf6a705f8034eeb8a217e5ece`。原件仓外`/tmp/doc-ltr01-A1-20261025`。

实际执行器R1 FAIL（status领先空格被strip）保留；该问题于claim之前修复，R2本地Claude session `8dda363f-cecb-48ea-9d67-6b5b01caefc1`，EXECUTION/DOC_ONLY/总PASS，输出SHA256 `8f9dd9147b5d3e921255c31e8dcf2aa36ac622809a4bb728ae0c6aab6bfea292`。源码准入R1 FAIL/R2 PASS同样保留。执行器审查不是自己的ER05；本节之后只变更本计划状态/实际证据与plan-status本计划行，源码/测试/配置/版本/lock与A相同，最终文档差异另做B并纳入真实Claude审查，不机械重跑未变编译/二进制。自己的审查、signed/DCO提交、push/PR/main D目前均待；审查以后事实按既定后置协议由FIX02补记，不预填结果。

root继续承担准入与执行器P2：任何新源码/元数据/help变化先STOP重评，既有FAIL/预算不重置；环境/进程异常不得自动续窗或杀非owned进程；53文件文档范围不可扩大。DOC自己的PR/main必须实际核验，FIX03最终覆盖树必须含本卡最终55路径。FIX01的main base40156941519尚未终态前不合并DOC至main，以免取消前一卡适用D；CodeQL40156941508及release40157032761已实际success，FIX01仍未done。原计划三实现卡0/3不变。

| 修订UTC | 来源 | 变化 | 状态 |
|---|---|---|---|
| 2026-10-25 04:25:21 UTC | DOC-LTR-01 A1实际终态 | 48命令/22help/B/资源通过，保留历史FAIL；仅两治理文件补事实 | locally-accepted，自身ER05及提交/PR/覆盖待 |

### FIX04旧依赖字段原件（2026-10-26前）

```text
**Dependencies:** `FIX-LTR-01`、`DEP-QUAL-SOURCE`、`DEP-QUAL-PUBLISH`；FIX02的已保存C1警告是来源证据，不以其done为前置。`FIX-LTR-04 -> FIX-LTR-02`，无反向覆盖依赖。
**Granularity:** `type=implementation; axis=同步ingress测试构造器的profile可用性; recovery=immutable-release; complete=yes; self-contained=yes; AC=4/8; VER=5/8; landing=1; prod-files=1; scope=S; deps=FIX-LTR-01,DEP-QUAL-SOURCE,DEP-QUAL-PUBLISH; writeset=两cfg及既定两治理+版本面; release=independent; split-from=FIX02-C1实际release警告STOP; exception=N/A`
```


### 2026-10-26 DOC02 R1真实FAIL与前滚修订

本地Claude session `df82ac52-8f07-4963-9319-786726e02a92`，五项FAIL，manifest `4788f76846dea304dd9293b0149c9682a2fee41e0504496ca5b2c4857964de2a`、output `e424f6da315e011a52e390d2267163583c6004ee5bbd2e550e47f04151751600`；root保留原结论。P1-1五卡汇总/完成门已前滚；P1-2拆ATTRIB与CLOSE，诊断只在已准入FIX05内；P1-3/4登记真实源码限制及未准入粒度，不称一文件设计可行。P2 owner=root：补FIX05 keyring门、env/serial条件与配置panic范围、LIVE/未发布incoming边、新stash；DOC02改docs并登记fmt/Clippy/main D。历史行号差一行保留原文。以上是修订候选，复审未取得前不记PASS，不授FIX05实现或warning例外。

以下原规范行按2026-10-25快照保留；现行要求见前滚后的对应段，禁止把旧四卡门当成当前完成条件。

```text
候选独立 A 窗口：FIX-LTR-01一次7200秒，FIX-LTR-02/03/04各一次10800秒（共享夹具真实云消费者或最终doc/network，均包含完整默认套件）；每卡最终 bump 后本地 C 各一次10800秒；FIX-LTR-03 的 C 包含完整全量、doc及原3例network。四卡实际D全绿后另有一次计划最终资格窗口10800秒，执行最终发布树fmt/clippy/full/doc/network，不以C代替模板计划完成门。上限是新独立来源的命令编排预算，不修改任何测试内部 deadline、并发组、断言或原 nextest retries。依据本机已测 build+listing+full4983秒及 strict Clippy约876秒，额外预留最终 release build/网络 feature 编译；不把冷构建慢当代码原因。本地窗口从第一条Cargo命令到本地门/构建/安装结束，包含编译和listing；PR CI、merge及Release D远端等待不计入该本地执行预算，单独登记run/job/ref，观察超时不重启作业、不记PASS。任一意外失败/超时/漂移立即保留终态并STOP，不自动再开窗口；后续只读归属与另行ER-03，不因剩余时间继续命令。
每卡 A 包含其 Verification、nightly fmt及strict all-target/all-feature Clippy；触发全量的卡另跑原双env默认全量。自己的 ER-05 PASS 后，C严格按模板ER-04/08顺序：版本面parity→实际patch→工具链刷新lock→最终fmt/clippy和本卡完整全量/所需focused→`cargo build --release`（四卡全局C⑤要求项目warning=0，唯一排除为用户具名proc-macro-error2 future提示；FIX04另加keyring release门）→将本卡release二进制安装到仓外、每卡独立的 `install/bin/libra` 并以绝对路径核version/hash（复制+chmod0755，hash须等于本次target/release/libra）；不覆盖原工作区使用的libra或改变全局PATH→精确`libra add`及signed+DCO提交→feature branch/PR→该head的base/CodeQL全部适用job通过→squash merge并核merge SHA/tree/DCO/签名；merge tree必须等于已测bump后tree，任何上游变化或树不同即STOP并ER-03，不tag、不借旧PASS→人工Release及版本tag→release工作流与渠道artifact真实核实。遇分支保护、必需他人review或环境approval，保留实际pending状态；不使用admin bypass、不自行批准。未完成D不得done。编译诊断按实际记录；若既有future-incompat提示违反当次发布门，先独立归属，不静默豁免。
4. **唯一终态登记提交。** 计划最终资格通过后，在独立clone创建一笔纯文档登记提交，I精确为 `docs/development/plan/plan-20261025-qualification.md` 与 `docs/development/plan/plan-status.md` 的本计划行；R=N/A、version=N/A、无tag/Release、不改变任一产品/测试/配置/lock/版本面。该提交承接FIX03真实C/D、最终资格和四卡状态，以及它自己的真实Claude文档ER-05收据；核对实际证据，登记本终态文档自身D仍pending。它是本计划预登记终止链末端，不是第五个产品修复或新版本发布点，不替代四卡各自C/D。没有独立发布原工作区Traces治理的权限。
**终态B的只读执行器：** 使用仓外 `/tmp/libra-qualification-recovery-plan-20261025/terminal-document-check.py`（在该末端执行前生成、hash冻结并纳入真实Claude输入，不属于产品源码）。执行命令为 `python3 /tmp/libra-qualification-recovery-plan-20261025/terminal-document-check.py`，以已核FIX03 merge SHA、候选树及两路径allowlist为显式冻结输入；缺输入即失败。必须以原生Libra diff逐路径证明只有本计划与plan-status本计划行变化；全其它来源hash一致；卡片/索引状态与已发生原件逐项对应；本计划全部必需章节/四卡字段存在；相对链接解析到当前clone；已登记审查收据只作后置事实，不改审查结论或其它字节；保留所有旧FAIL/STOP。自动结构结果不替代人审语义和真实门结果。
| DEP-QUAL-F2 | incoming | 当前失败归属 | reset_pathspec_from_file_missing_path_errors真实stderr与根因 | Libra接收Agent | 按2026-10-26入口完成受审诊断及P1处置；未知不能关闭，独立根因另卡 | 旧stderr缺失，当前未完成 | 不借native retry PASS或未复现关闭；诊断预算另审 |
| DEP-QUAL-HANDOFF | outgoing | 来源资格 | plan-20261015 LTR-10 / LTR-01 | root | 四卡独立C/D完成、FIX03 bump后C及发布后最终资格窗口全量/doc/network与本地Claude明确PASS（绑定实际merge树）、终态登记提交实际PR/main D通过并具名例行补记，再核受控整合树、原工作区受控整合/受影响门通过 | 后续真实提交/版本/远端ref/原件，当前无 | 来源资格未闭合不得解除LTR-10前置；不更改H1或替代LTR各卡门 |
`DEP-QUAL-SECURITY + DEP-QUAL-SOURCE + DEP-QUAL-PUBLISH -> FIX-LTR-01 -> FIX-LTR-05 -> FIX-LTR-04 -> FIX-LTR-02 -> FIX-LTR-03 -> DEP-QUAL-HANDOFF`；另有 `DEP-QUAL-F2 + DEC-QUAL-05-RELEASE-WARNING -> FIX-LTR-05`，当前授权与诊断未闭合，候选顺序不等于准入。顺序用于共享本计划/版本面的串行发布，不表示合并发布。先修确定性全量阻断，再修共享fixture，最后完成MCP测试清理与整份修复计划的最终资格。每卡真正 C/D 闭合后才发布下一卡。

现行顺序（2026-10-26，不以本快照为准）：`FIX-LTR-01 -> FIX-LTR-05P -> FIX-LTR-05Q -> FIX-LTR-05R -> FIX-LTR-05S -> FIX-LTR-05T -> FIX-LTR-05U -> FIX-LTR-05V -> FIX-LTR-05W -> FIX-LTR-05X -> FIX-LTR-05Y -> FIX-LTR-05Z -> FIX-LTR-05`。05Y 已推送 `ec98926`。05Z 的 ER03 `272c8d7d` PASS，实现审查 `4a42ec63` PASS。父卡发布仍使 FIX-LTR-05 blocked。见上文「实施顺序与发布窗口」。
### 四卡共同适用的逐项D清单
**Granularity:** `type=implementation; axis=global-schema doctor的auto-upgrade结构oracle对齐; recovery=immutable-release前滚本卡单轴测试修复; complete=yes; self-contained=yes; AC=3/8; VER=3/8; landing=1; prod-files=0; scope=S; deps=DEP-QUAL-SOURCE, DEP-QUAL-SECURITY, DEP-QUAL-PUBLISH; writeset=四卡本计划与R串行; release=independent; split-from=新上游资格20261025实际FAIL/STOP; exception=N/A`
**Granularity:** `type=implementation; axis=合成checkpoint fixture的索引结清前置; recovery=immutable-release前滚本卡单轴测试修复; complete=yes; self-contained=yes; AC=4/8; VER=3/8; landing=1; prod-files=0; scope=S; deps=FIX-LTR-01,FIX-LTR-04; writeset=四卡本计划与R串行; release=independent; split-from=新上游资格20261025实际FAIL/STOP; exception=N/A`
- [ ] 完整agent_mcp_exec_test通过，原deadline、内存、slot、禁止helper断言全部保持；本卡bump后C全量/doc/network通过，实际本卡Claude ER-05通过；四卡D之后的计划最终资格属于计划完成门，不倒挂本卡done条件。
**Granularity:** `type=implementation; axis=MCP read-worker测试夹具完整teardown; recovery=immutable-release前滚本卡单轴测试修复; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=0; scope=S; deps=FIX-LTR-02; writeset=四卡本计划与R串行; release=independent; split-from=新上游资格20261025实际FAIL/STOP; exception=N/A`
| 修复里程碑 | FIX01(done)→FIX04→FIX02→FIX03分别独立完整C/D | 发布串行，不能合并、不能以手工worker清理当FIX03修复 |
四卡分别实际A/B、Claude ER-05 PASS、精确signed/DCO提交、独立patch/PR/merge/tag/人工Release、实际CI与发布artifact D完成；全部P0/P1关闭，P2具名处置。FIX-LTR-03 的C④在bump后树执行完整默认套件、`cargo test --doc`及原3例network；merge树与被测树不等即STOP。四卡各自实际D完成后，在与FIX03发布merge树相同的冻结来源执行上述一次10800秒计划最终资格窗口（fmt、strict Clippy、双env完整默认套件、doc、原3例network），核全部身份/退出/ignored/flake/runtime skip与owned资源；根因未知的缺口不得改成已证。最终实际本地Claude来源资格PASS后，才把四个具体发布SHA/版本及适用覆盖交给原scoped-traces计划；其H1、原merge签名政策、LTR自身A/B/C/D仍独立。
当前两原计划正式17/118、0/26、17/144（活动17/142）；本外部候选四个实现节点独立单列1/4，不增加原26卡或改写初始统计。联合依赖审计展开FIX01→FIX04→FIX02→FIX03并与H0/H1/LTR10核无环；任一卡/末端交接证据不足均未完成。
```


### 2026-10-26 DOC02 R2真实终态与修订

本地Claude `6efd0e29-4c53-4b59-beda-261df8c0bab2`：HANDOFF/SOURCE_AND_DEPENDENCY/EXECUTION_BOUNDARY/VERDICT四FAIL、GRANULARITY PASS（仅候选登记，不准入FIX05）；manifest `1a72abb34ce985319ee0f39ae444a3d0b5ca59df6571913e194d4dd8ead30315`，output `4fcc1356ac3c1b62860c70a9e9bef82f7b87afa7106c5071a421dd78b55583a4`。R1原问题已关闭，R2三个P1由本次修订候选处理：FIX04粒度边改F2-CLOSE，并规定FIX05后置C/D由FIX04的两治理补记；执行器在claim前及每个命令加载双env后均拒绝三个全局目录override；提交器核own-review包含最终正文与实际gate终态，signed message补manifest/terminal/gate-input hash。复审未取得前不写PASS。

P2 owner=root：真实DAG仍须Claude逐字段人审，机器图仅辅助；环境异常移到claim前，不消费新窗口；deadline后的未知/遗留进程保持STOP并核PID/start身份，不靠进程名清理；当前root为Codex进程，Claude CLI是原生可执行文件，竞争检查适用本机，新机器重新核；两env同hash仅说明本机字节相同，不证明live凭据有效；签名失败只留下未推送本地提交，后续必须审定恢复；缓存容量/现存build产物在准入前只读登记，冷编译不自动延长。原“四修复轴”候选横幅及历史行号不改。

补存R1未逐行抄入快照的三行原文（2026-10-25旧规范，当前已被前滚段替代）：

```text
- 修复 checkout 的公开写集仅本计划、基于实际已发布上游版本的 `docs/development/plan/plan-status.md` 中本计划登记行、当前卡已登记测试文件、该卡实际版本面/工具链 lock；仅FIX-LTR-04额外允许 `src/internal/ai/capture/ingress.rs` 两个方法定义前各加一行 `#[cfg(any(test, debug_assertions))]`，不改其方法体或生产入口。与原工作区未发布 scoped-traces 治理提交做 ancestry 和逐文件 diff 守卫，禁止携带 `ed566b0`、其本地治理父链或原三份治理 WIP；plan-status 只在上游版本中新增/更新本计划行，禁止复制原工作区整份文件。本文是新的独立修复计划，不复制未发布的 scoped-traces 文本到发布分支。
- 原工作区两仓既有八份治理文件只登记事实/外部依赖，仍由原 MTR-01/LTR-01 与 Mega 后续既定 C/D 承载；其中原 scoped-traces 治理增量不属于四个修复分支的发布写集；plan-status 的新修复计划行是下述独立同步写集。新修复发布后，原工作区只通过原生前滚整合保留双方历史；若本文本地未跟踪副本与远端已发布版本碰撞，先保留快照、核同源与内容，再只处理 root 自建副本，不清理无关文件、不 reset/force。
所有 Cargo 门 source `.env.test` 和 `.env.live-test`，核非秘密 presence/控制项、原工具链与 Cargo 配置；缺文件或必要凭据停止相应门。延续隔离 HOME、已核 CARGO_BUILD_JOBS=1、umask0022，不复制用户凭据到仓库或日志。listing与JUnit按(target,case)精确核数、保留原ignored/重试及成功输出中的runtime skip；缺环境、缺 feature、early return不冒充所需行为通过。代码完成、自身review、提交、push、D逐项分别登记。
```
