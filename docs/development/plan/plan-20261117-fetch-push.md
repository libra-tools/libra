# Fetch 与 Push Git 兼容性修复计划（2026-11-17）

> 模板版本：v2.16；继承 [plan-template.md](plan-template.md) 的 GC-01..13、ER-01..14、G-01..11。本计划不在 v2.16 具名 `code-only` 范围内，按版本化发布合同执行：每张 `implementation` 卡各自执行完整 C 组（含 `patch + 1`、`compat_version_surface_sync` 三处版本面同步、工具链刷新 `Cargo.lock`、`gh release create`）与适用 D 组。
> 状态：2026-11-21 增补草稿待评审整改；历史 Claude R19 与 Codex R19 双审 PASS（受审 SHA-256 前 12 位 `bda4c9f4798d`）仅适用于旧字节。任务进度以各卡和 `plan-status.md` 为准；本次草稿未获 PASS，不可据此开工。
> 审阅合同：按用户 2026-11-18 指令，本计划的计划文本提交门为 Claude（Subagent）与 Codex 双审，二者对同一受审字节（同一 SHA-256）均给出 `VERDICT: PASS` 方可提交（取代 2026-11-17「计划提交门为 Codex 单审，不再要求 Claude Review」口径）。执行期各卡的 ER-05 review（模板默认审查者 Codex）与网站交付的独立 Claude Code `PASS` 不受本条改变，三者互不替代。
> 跟踪范围：GitHub open issues #683、#710、#719、#722、#749、#863、#864、#865、#867、#872、#873、#874、#877、#878、#879、#1000、#1031、#1036（共 18 项，2026-11-18 以 `gh issue view -R libra-tools/libra` 核对均为 OPEN）。

## 文档职责

- **网站交付路径（权威）:** 唯一生产网站 checkout 为 `/Volumes/Data/gitmono/libra-backend`（`cf` 生产分支），依据是用户 2026-11-18 11:55:09 UTC 确认：本批计划的网站文档交付检出为该路径（记为 `DEP-FP-08`，已满足）；该路径同时是模板 ER-06a 默认的 Libra checkout 同级 `../libra-backend`（`plan-template.md:332`）。仓库 `AGENTS.md:135` 仍写 `/Volumes/Sea/libra-backend`，该路径在本机不存在（2026-11-18 核对），属已知文档滞后；用户决定本批不改 `AGENTS.md`，本计划以上述确认为准。修订历史 2026-11-18 08:19:08 行所称「用户最新指令」不作为依据，以本条确认为准。2026-11-18 核对：`/Volumes/Data/gitmono/libra-backend` 为 Libra 检出（有 `.libra/`、无 `.git`），`libra --offline symbolic-ref --short HEAD` 输出 `cf`，`libra status --short --branch` 首行为 `## cf...origin/cf`。写入前须按「文档与网站验收命令」第 2 步核验该路径存在、仅含单一 VCS 元数据且（必要时按该步切换后）分支为 `cf`；按该步仍不满足即 STOP，禁止改用其它检出。

本文解决 `libra fetch` 与 `libra push` 相对 Git（基线 `git@6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`，v2.56.0-116）的 18 项已登记兼容缺口：裸本地仓库目录名识别、命令行 refspec 短目标补全、`push` 的 `remote.<name>.push`/`--prune`/通配符/负向 refspec、本地推送 `receive.denyCurrentBranch`、`--force` 与 `--force-with-lease`、无参 fetch 回落 `origin`、多 refspec、非常规目标命名空间、`--multiple`/`--jobs`/`fetch.all`、多远程失败继续、`--[no-]write-fetch-head`、人读报告的输出流与格式、`--no-porcelain`、`--prune` 作用域、`--tags` 冲突拒绝、`remote.<name>.tagOpt` 与 fetch 后自动打包。目标是在这些行为上与 Git 一致，并同步测试、命令文档、兼容矩阵与网站文档。

本文只规划任务，不宣称实现完成。落地时每个任务都必须先刷新源码锚点，再按任务卡验收（GC-01）。

### 适用范围

- 命令与 CLI：`libra fetch`（`src/command/fetch.rs`）、`libra push`（`src/command/push.rs`、`src/internal/protocol/local_push.rs`）、共享仓库参数解析（`src/internal/protocol/repository_arg.rs`）、`gc --auto` 判定（`src/command/gc.rs`，`FP-24`）与 `src/cli.rs` 的 `Commands::Fetch` 分发后钩子（仅 `FP-18`）。
- 配置与环境变量：`remote.<name>.push`、`receive.denyCurrentBranch`（目标仓库）、`fetch.all`、`remote.<name>.tagOpt`、`gc.auto`、`gc.autoPackLimit`、`GIT_TRACE`（仅绝对路径形式，`FP-10`）。
- 测试与文档：`tests/command/{fetch,push,gc}_test.rs`（均属 `command_test` 目标）、`src/command/push.rs` 的 `mod test`；只读回归 `compat_fetch_remote_refspec`、`compat_local_push_no_cwd_switch`、`compat_config_defaults_semantics`；`docs/commands/{fetch,push,gc}.md`、`docs/commands/zh-CN/{fetch,push,gc}.md`、`docs/development/commands/{fetch,push,gc}.md`、`COMPATIBILITY.md`（`| push |` 第 222 行、`| fetch |` 第 223 行、`| gc |` 第 267 行原地更新）及 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/{fetch,push,gc}.en.md`。

### 非目标

- 不重构 `libra clone`、`libra pull` 的参数解析（`DEFER-FP-01`）；`pull` 的仓库参数解析保持不变（ADR-FP-01）。
- 不修改 SQLite schema 或迁移 ledger（不触发 GC-13），不新增网络传输协议或 `StableErrorCode`。
- 不实现 `--prune-tags` 三态与 remote/global 优先级（`DEFER-FP-03`）、`fetch.unpackLimit` 小包解包为松散对象（`DEFER-FP-04`）、`fetch --jobs` 的真正并发执行（`DEFER-FP-05`）、`refs/tags/*` fetch 目标、`receive.denyCurrentBranch=updateInstead`、remote 组与 `fetch.parallel`。

### 成功定义

- 23 张任务卡全部 `Lifecycle=done` 且 `Acceptance=complete`，各卡卡特有用例与共享回归组按「验证执行协议」通过。
- `docs/commands/{fetch,push,gc}.md` 及 zh-CN、`docs/development/commands/{fetch,push,gc}.md`、`COMPATIBILITY.md` 与网站页同步反映新行为与有意差异。
- 全部任务卡完成后通过 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings` 及 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`。
- **Issue 关闭口径:** 本计划不自行关闭 issue。全部卡完成后由协调者逐条核对各 issue 的验收条目：只由 fetch/push/gc 行为构成的条目以本计划卡特有用例为准；依赖其它命令或上游整文件 harness 的条目由登记的承接项负责，未闭环前 issue 保持 OPEN 并只同步进展——`#683` 的 `TC-0180`（`DEFER-FP-02`/`DEP-FP-07`）、`#719` 的 `checkout -b new origin`（`DEP-FP-03`）、`#710` 的 `remote add -f` 与 `checkout`/`switch` DWIM 提示（不属本计划，协调者另行登记），以及各 issue 引用的外部 `TC-*` 整文件 harness 运行（`DEP-FP-06`，含 `#863` 的 `TC-1338`）。

## 使用规则

- 每次状态推进与「修订历史」写入都记录实际 `YYYY-MM-DD HH:MM:SS UTC` 时间（v2.11）。
- 推进或完成任务卡时，执行者须在同一变更中同步更新 [`plan-status.md`](plan-status.md) 的「计划一览」行（状态、版本或 commit、时间）；本计划入库时的初始登记行由协调者随本批计划合入一并登记，`plan-long.md` 日期索引同样由协调者统一维护。
- 任务 ID 固定：`FP-01`..`FP-19`、`FP-21`、`FP-23`、`FP-24`、`FP-25` 为现行卡；`FP-15a`、`FP-15b`、`FP-15c`、`FP-15R`、`FP-20` 已废弃（见「废弃编号」），`FP-22` 已在 2026-11-17 05:10:00 UTC 修订移除；均不复用。拆分按 G-09 追加新编号并登记「修订历史」。
- 本计划的 `FP-*` 卡号属本计划自有命名空间；`docs/development/plan/issues/529.md` 中同名的 `FP-05`、`FP-07` 等为该 issue 计划的卡，其它计划以 `issues/529.md#FP-xx` 引用的是后者，与本计划无关。
- 本计划不新建证据目录或证据文件（v2.12），所有门禁结果、SHA-256 与测试摘要写入本计划「实施证据汇总」或对应卡内。

## 事实基线

> 所有行号和源码锚点必须在开工当天按 ER-02 刷新。以下锚点核对于 `2026-11-18`（`Cargo.toml` = `0.30.210`）；标注「评审期预探」的行为观测使用本机已安装的 `libra 0.30.207`，只作线索，开工时以当前构建复核。

| 类别 | 当前事实 | 证据 |
|---|---|---|
| 代码入口 | `FetchArgs`（680-813）、`execute_safe`（1263）、`run_fetch`（1429）、`render_fetch_output`（1653）、`parse_fetch_refspec`（1920）、`validate_fetch_destination`（1887）、`write_fetch_head`（4504）、`resolve_tag_mode`（5113）；`PushArgs`（96-184）、`validate_push_args`（884）、`run_push`（942）、`validate_force_with_lease`（1895）；`apply_local_push_to_libra`（35）、`apply_one_update`（92）；`gc::execute_safe`（151） | `src/command/fetch.rs`、`src/command/push.rs`、`src/internal/protocol/local_push.rs`、`src/command/gc.rs` |
| 共享解析 | `is_anonymous_repository_spec` 只识别 SSH/URL、`.`/`..`、绝对路径与 `./`、`../` 前缀；`fetch.rs:1576`、`push.rs:966`、`pull.rs` 共用 | `src/internal/protocol/repository_arg.rs:17-31` |
| 用户命令 | `Commands::Fetch`/`Commands::Push` 分发；命令期间持有 `MaintenanceLock::shared(storage)` | `src/cli.rs:3760-3772,4067` |
| 机器输出 | `FetchOutput { all, requested_remote, refspec, remotes }`；fetch `--porcelain` 每行 `<flag> <old> <new> <ref>`；push 人读输出在 stdout | `src/command/fetch.rs:914-919,1310-1353`、`src/command/push.rs:2508-2531` |
| 错误码 | 复用 `LBR-CLI-002`（默认 129）、`LBR-CLI-003`（129）、`LBR-REPO-001`（128）、`LBR-REPO-003`（128）、`LBR-CONFLICT-002`（128）、`LBR-IO-001`（128）；默认退出码见左，`LIBRA_FINE_EXIT_CODES` 等可改变数值退出码 | `docs/error-codes.md:98-115`、`src/command/push.rs:417-478`、`src/command/fetch.rs:1035,1089-1100` |
| 文档 | `fetch`/`push`/`gc` 的中英文命令页、开发文档与网站页均存在；`COMPATIBILITY.md` 状态矩阵行 `push`=222、`fetch`=223、`gc`=267（以「文档与网站验收命令」W-1 刷新）；网站同步脚本 `scripts/sync-libra-command-docs.py` | `docs/commands/{fetch,push,gc}.md`、`COMPATIBILITY.md:222,223,267`、`/Volumes/Data/gitmono/libra-backend/scripts/sync-libra-command-docs.py` |
| 测试 | `tests/command/{fetch,gc,push}_test.rs` 经 `tests/command_test.rs` 的 `mod command` 挂载于 `command_test`（nextest 名形如 `command::fetch_test::<fn>`）；子进程辅助 `run_libra_command`/`run_libra_command_with_env`（隔离 HOME 与全局配置库）；compat：`compat_fetch_remote_refspec`、`compat_local_push_no_cwd_switch`、`compat_config_defaults_semantics` | `tests/command_test.rs:10`、`tests/command/mod.rs:385,392,1047,1052,1110`、`Cargo.toml:303,577,589`、`tests/INDEX.md:102,105,165` |
| 外部参照 | 上游 Git 源码与测试树 `git@6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`（v2.56.0-116；本机 Git 仓库 `/Volumes/Data/gitmono/git` 含该提交，2026-11-18 核对；该仓库工作区 HEAD 已前移至 `750f5de7df`（v2.56.0-349），行号不同，本计划所有 Git 源码与测试行号均指钉选提交，须以 `git -C /Volumes/Data/gitmono/git show 6de20f6092:<路径>` 读取，不读工作区文件）；外部用例库 `../libra-testcases`（Git 仓库，`cases/<tc>/<tc>.sh` 以整份上游测试文件运行） | `t/t5510-fetch.sh`、`t/t5514-fetch-multiple.sh`、`t/t5533-push-cas.sh`、`t/t5574-fetch-output.sh`、`t/t5582-fetch-negative-refspec.sh` 等（各卡逐行引用） |

### 当前缺口

| ID | 缺口 | 影响 | 证据 | 计划动作 |
|---|---|---|---|---|
| GAP-FP-01 | 裸本地仓库目录名被当作 remote 名；命令行 refspec 不补全短目标，匿名 fetch 丢弃显式目标 | `fetch parent main:origin`、`push src1 ...` 报 `remote not found` 或 `LBR-CLI-002` | `#719`、`#710`；`src/internal/protocol/repository_arg.rs:17-31`、`src/command/fetch.rs:1576-1597,1920-1978` | FP-01、FP-23、FP-02 |
| GAP-FP-02 | `push` 不读 `remote.<name>.push`、缺 `--prune`/通配符/负向 refspec；本地推送忽略 `receive.denyCurrentBranch`；`--force` 与 `--force-with-lease` 互斥且租约判定与 Git 不一致 | Git 常见推送工作流与租约语义失效 | `#683`、`#877`、`#1031`、`#1036`；`src/command/push.rs:118-119,884-894,1855-1926`、`src/internal/protocol/local_push.rs:92-112` | FP-04、FP-03、FP-05、FP-06、FP-21 |
| GAP-FP-03 | 无参 fetch 不回落 `origin`；只接受一个 refspec；拒绝非常规目标命名空间；缺 `--multiple`/`--jobs`/`fetch.all`；多远程首错即中止 | 镜像克隆、多远程与自定义 refspec 工作流失败 | `#749`、`#1000`、`#722`、`#867`、`#865`；`src/command/fetch.rs:684-691,1489-1565,1887-1917` | FP-07、FP-08、FP-09、FP-10、FP-19、FP-11 |
| GAP-FP-04 | 缺 `--[no-]write-fetch-head`；人读报告在 stdout、无 `<src> -> <dst>` 与 `-> FETCH_HEAD`；缺 `--no-porcelain` | 脚本无法按 Git 方式解析或抑制 fetch 输出 | `#864`、`#878`、`#879`；`src/command/fetch.rs:711-714,1287-1294,1653-1739` | FP-12、FP-13、FP-14 |
| GAP-FP-05 | `--prune` 带显式 refspec 越界修剪；`--tags` 静默跳过冲突 tag；`tagOpt` 精确大小写读取；fetch 不触发自动打包 | 误删远程跟踪引用、冲突被掩盖、配置被忽略、pack 堆积 | `#872`、`#873`、`#874`、`#863`；`src/command/fetch.rs:776-780,1451,2840-2856,5113-5126,5223-5228`、`src/command/gc.rs:151-187` | FP-15、FP-16、FP-17、FP-24、FP-18 |

## 与其它计划的关系

| 计划/文档 | 关系 | 本计划处理 |
|---|---|---|
| `plan-20261117-merge-pull.md` | 同批：`pull.rs` 复用 `repository_arg.rs`；共用 `COMPATIBILITY.md` | `DEP-FP-02`：`FP-01` 只新增 opt-in helper，不改 `is_anonymous_repository_spec`（ADR-FP-01）；写入按 `DEP-FP-01` 串行 |
| `plan-20261117-branch.md` | `BR-24`（`skipFetchAll`）改 `fetch.rs` 的 `--all` 循环；`DEFER-BR-02` 对应 `branch --delete-merged` | `DEP-FP-04`（`FP-10`、`FP-19`、`FP-11` 与 `BR-24` 串行）；`DEFER-FP-02`/`DEP-FP-07` 与 `DEFER-BR-02` 对齐 |
| `plan-20261117-clone.md` | 其 `DEP-CL-04`/`DEP-CL-05`/`DEFER-CL-03` 以 `FP-07`、`FP-15` 为顺序或闭环前置 | `DEP-FP-05`；`FP-07`、`FP-15` 编号与范围保留（`FP-15` 仍只修 `--prune` 显式 refspec 作用域） |
| `plan-20261117-checkout-switch-restore.md` | `#719` 验收 3 的 `checkout -b new origin`（`#676`） | `DEP-FP-03`（outgoing） |
| `plan-20261117-worktree-clean-reset-apply.md` | 其 `DEP-WC-07` 把本计划 `FP-18` 列为 `src/cli.rs` 写者 | `FP-18` 仍写 `src/cli.rs`，开工前按 `DEP-FP-01` 取得含 `src/cli.rs` 的全写集锁 |
| `docs/development/plan/issues/529.md` | 卡号同为 `FP-xx` 但属不同计划 | 不引用、不协调；见「使用规则」 |
| `plan-long.md`、`plan-status.md` | 长期路线与状态视图 | 由协调者登记；执行期按「使用规则」同步 `plan-status.md` |
| `docs/development/plan/plan-template.md` | 模板 v2.16 | 全文遵守 |

## 评审结论与修订记录

| 维度 | 结论 | 修订动作 |
|---|---|---|
| 合理性 | 18 个 issue 均有评审期预探或上游测试复现 | 按单一行为轴拆为 23 张 `implementation` 卡 |
| 可行性 | 每卡只改 1–3 个生产文件，复用既有 fetch/push/gc 设施 | `FP-15a/b/c/R` 家族与 `FP-20` 超出 issue 范围且难以收敛，废弃并登记 `DEFER-FP-03`、`DEFER-FP-04` |
| 任务卡粒度 | 全部卡 `M`、AC 与 VER 均 ≤ 8，无 waiver | 撤销 `EX-FP-07-PRUNE`（`FP-07` 只保留默认 remote 选择轴，prune 优先级为既有实现）；拆出 `FP-23`、`FP-24`、`FP-25` |
| 依赖与顺序 | 全串行 DAG（共享 `fetch.rs`/`push.rs`/`COMPATIBILITY.md`），`FP-04` 先于 `FP-03`（配置 refspec 复用命令行引擎） | 「实施顺序」与各卡 `Dependencies` 一致 |
| 完整性 | 每卡含卡特有用例、共享回归组、四端文档与兼容矩阵 | 统一「验证执行协议」与「文档与网站验收命令」 |
| 安全性 | 裸目录名只在解析为存在的 Libra 仓库目录时生效；非常规目标仍做 refname 校验 | 继承 GC-07、GC-09 |
| 功能正确性 | 对齐钉选 Git 源码与上游测试（`remote.c`、`builtin/fetch.c`、`builtin/receive-pack.c`、`odb/source-files.c`） | 各卡 `Current evidence` 逐行引用 |
| 接口兼容 | `--json` 只新增字段（`refspecs`）；有意差异写入 `COMPATIBILITY.md` | 失败类 AC 只断言稳定码 |
| 数据流与控制流 | 多 refspec 先整体校验再写；多远程失败继续但配置错误仍预检 fail-closed；自动打包在命令级维护锁释放后执行 | ADR-FP-04 |
| 性能与容量 | 多远程 fetch 串行、`--jobs` 不引入并发（真正并发延后）；自动打包只在阈值命中时执行 | 「性能与容量摘要」 |
| 可靠性与容错 | 租约不匹配与冲突 tag 拒绝时引用不变 | 「故障恢复矩阵」 |
| 可维护性 | `repository_arg.rs`、push refspec 引擎、gc 判定各为单一事实源 | GC-02 |

### 修订历史

| 时间 | 触发 | 变更内容 | 原卡 → 新卡 | 受影响的引用 |
|---|---|---|---|---|
| 2026-11-21 11:13:42 UTC | 本地 Artifacts 测试 | 新增 FP-23 后续验证及 FP-09 trace 验收草稿；Claude 对基于 `c84109a` 的候选文本给出 FAIL（5 项 P1），本草稿待整改与复审 | N/A | FP-23、FP-09 |
| 2026-11-17 03:05:00 UTC | 初稿创建 | 建立覆盖 18 个 fetch/push 兼容问题的 18 张任务卡计划 | N/A | 全文 |
| 2026-11-17 03:12:00 UTC | Claude R1 FAIL + Codex R1 FAIL | 修正 `FP-06` `--force` 覆盖 lease 语义与 `mod test` 过滤器；`FP-01`/`FP-02` 改由共享 `repository_arg.rs`（`T-1` + `DEP-FP-02`）解决裸目录名；补全 `FP-04` 负向 refspec、`FP-08`/`10` `cli.rs`（`T-1`）与 JSON/`fetch.all`/`--jobs`、`FP-09` 配置通配、`FP-12` `--all`、`FP-13` `T-5` 与 dry-run、`FP-15` single-branch/`--tags`、`FP-16` 非冲突 tag 与 stderr、`FP-18` `unpackLimit`/`autoPackLimit`；修正 `landing` 与独立谓词计数、锚点及 Git `2.56.0` pin | N/A | 事实基线、ADR-FP-01/02、依赖表、粒度审计表、FP-01..18 正文、review log |
| 2026-11-17 03:24:00 UTC | Codex R2 FAIL | `FP-18` 区分 `fetch.unpackLimit` 接收阶段与 `gc.auto`/`gc.autoPackLimit` 维护阶段；`FP-15` 区分 `--tags` 与显式 tag refspec；`FP-03` 保留无配置拒绝错误；`FP-01` 纳入 `DEP-FP-02` 与 `DEP-FP-03`；`FP-08` 注明 `fetch_test.rs:2306` `local_fetch_args`；重算 `FP-03`/`FP-10`/`FP-18` 谓词计数；更新 D 组真实 CI job 名 | N/A | 依赖表、D 组、FP-01/03/08/10/15/18、粒度审计表、review log |
| 2026-11-17 03:35:00 UTC | Claude R3 FAIL + Codex R3 FAIL | ADR-FP-01 改为在 `repository_arg.rs` 新增 opt-in `is_anonymous_repository_spec_with_bare_dir`（`FP-01` 仅接入 `fetch.rs`、`FP-02` 接入 `push.rs`，不意外改变 `pull`）；新增 `DEP-FP-04`（`plan-20261117-branch.md#BR-24`）与 `DEP-FP-05`（`plan-20261117-clone.md#CL-13`）；`FP-17` 仅消费既有 config helper（移除 `config.rs`）；`FP-18` 覆盖 `t5510#308`/TC-1338；移除 `FP-08`/`10` 误列的 `src/cli.rs`；补全 `FP-06`/`08`/`11`/`13`/`14` P2 与 `FP-13/15/16` 谓词计数 | N/A | ADR-FP-01、依赖表、FP-01/02/06/08/10/11/13/14/17/18、粒度审计表、review log |
| 2026-11-17 03:40:00 UTC | Claude R4 FAIL + Codex R4 FAIL | 彻底清除 `FP-08`/`FP-10` 残留 `src/cli.rs` 引用（审计表 `2/2→1/1`、追溯表、第 80/123 行与 `FP-08` Current evidence）；补齐 `FP-10`/`FP-11` 对 `DEP-FP-04` 与 `FP-07` 对 `DEP-FP-05` 的挂接；修正 `FP-17` `config.rs:536` `get_var_case_insensitive_with_conn` 锚点并移除追溯表冗余 `config.rs`；补全 R2-R4 review log | N/A | 第 80/123 行、粒度审计表、FP-07/08/11/17、追溯表、review log |
| 2026-11-17 03:50:00 UTC | Claude R5 FAIL + Codex R5 FAIL | 拆出 `FP-19`（`fetch.all` 配置，G-01/G-04）与 `FP-20`（`fetch.unpackLimit` 接收阶段）并补双向 `split-from`；`FP-11` 补全 `--multiple`/`--jobs` 逐远程失败报告与 `t5514#13`；`FP-18` 标 `T-1`（共享 `gc.rs`）并补 `t/t5510-fetch.sh#308`/`TC-1338` 门禁；`FP-07` 收紧为仅回退 `origin` 并加 `for-each-ref refs`；`FP-08` 明确 `FetchOutput.refspec` 与 `fetch_repository_with_result` 保持 `Option<&str>` 不改 `pull.rs`/`clone.rs`；`FP-03`/`15` 替换宽泛 compat 过滤器；`FP-04` 补充单轴理由；`DEP-FP-03` 具名 `plan-20261117-checkout-switch-restore.md`；`完成判据` 补 ER-13 全量失败处置 | FP-10 → FP-10, FP-19; FP-18 → FP-18, FP-20 | 实施顺序、依赖表、审计表、FP-03/04/07/08/10/11/15/18/19/20、追溯表、完成判据、review log |
| 2026-11-17 04:02:00 UTC | Claude R6 FAIL + Codex R6 FAIL | 全文同步 20 张卡计数、`发布窗口顺序` 与 `M2`/`M3`；为 `FP-01/02/07/09/11/12/13/14/15/16/18/20` 补齐 `COMPATIBILITY.md`；`FP-18` 补 `gc` 四端文档与 `gc_test` 新用例；`FP-20` 修正 `../libra-testcases/...` 路径、`cargo build --bin libra` 与自洽集成测试并增补风险行；`FP-01`/`03`/`04`/`08`/`10`/`18` 补 P2 精确语义与谓词计数；`codeql.yml` 补 `release/**`；`完成判据` 补齐模板全部条目 | N/A | 全文多处、FP-01/02/03/04/07/08/09/10/11/12/13/14/15/16/18/20、追溯表、里程碑、风险表、完成判据、review log |
| 2026-11-17 04:12:00 UTC | Claude R7 FAIL + Codex R7 FAIL | 拆出 `FP-21`（`--force-with-lease` 租约矩阵，G-01/G-03）并补双向 `split-from`；严格重算 `FP-05/08/13/16` AC 计数；`FP-20` 明确 `>= fetch.unpackLimit`（默认 `100`）边界与 `tc1338` 非门控外部对照 + `DEP-FP-06`；`DEP-FP-05` 扩展至 `CL-05/10/11/13/19` 并于 `FP-15` 承接 `#876`；对齐 `FP-07` 描述、补 `FP-10/11/16` 退出码/`--json`、`FP-18` `Already up to date` 根因、`codeql.yml` PR `main`、`文档收口` `gc`/`error-codes`/`Cargo.toml`、`发布者` 具名与 `事实基线` 错误码行 | FP-06 → FP-06, FP-21 | 事实基线、D 组、实施顺序、依赖表、审计表、FP-05/06/07/08/10/11/13/15/16/18/20/21、追溯表、里程碑、文档收口、review log |
| 2026-11-17 04:20:00 UTC | Codex R8 FAIL + Claude R8 网络中断 | `FP-10` 对齐 Git `--jobs=0` 语义（选择合理默认并发度而非拒绝），仅负值/非整数返回 CLI 参数错误 | N/A | `FP-10`、review log |
| 2026-11-17 04:25:00 UTC | Codex R9 FAIL | `FP-20` 与 `DEP-FP-06` 将 `TC-1338` / `t5510#308` 升级为硬门控验收，缺失外部语料时阻塞 `FP-20` 并提供检出路径而非跳过 | N/A | `DEP-FP-06`、`FP-20`、review log |
| 2026-11-17 04:29:00 UTC | Claude R10 FAIL + Codex R10 FAIL | `FP-21`/`FP-16`/`FP-11` 退出码对齐 `docs/error-codes.md` `128`（`LBR-CONFLICT-002`/`LBR-NET-001`）并于 `COMPATIBILITY.md` 记为 `intentionally-different`；`FP-20` 与 `风险登记` 澄清 `tc1338-fetch-verbose.sh` 即钉选 `t5510#201`（对应 `#863` `#308`）唯一可执行门控并明确 `SKIP`/前置失败按 `FAIL` 阻塞；重算 `FP-04`（`5/8`）与 `FP-18`（`4/8`）AC；`FP-05` 测试名含 `ignore`、`FP-18` 移除 `1555` 重叠、`FP-13` 改用 `T-5` 显式说明、补 `Phase 0` | N/A | Phase 0、审计表、FP-04/05/11/13/16/18/20/21、风险登记、review log |
| 2026-11-17 04:33:00 UTC | Codex R11 FAIL + Claude R11 FAIL | 新增 `ADR-FP-03` 约束 `fetch.unpackLimit` 仅在显式配置时生效（保持未配置时 `install_fetched_pack` 默认保留 pack，不波及 `pull`/`clone`/`mega2_traces`），补 `GC-08` 原子解包、`GC-10` 与 `T-1`；`FP-18` 明确 `auto-gc` 仅在 `libra fetch` CLI 触发并补 `#863` 跟踪分支推进 AC（`AC=5/8`）；`DEP-FP-06` 明确前置失败转 `Lifecycle=blocked`；修正 `21 张任务卡`、`Phase 2` 入口 `FP-21`、`RemoteNotFound`→`LBR-CLI-003`、`FP-11` 保留原始稳定错误码、单一具名发布者 `Plan Author 5` 与 `^TC-1338 PASS$` 校验 | N/A | 合理性、ADR-FP-01/03、DEP-FP-06、发布者、Phase 2、审计表、FP-01/02/11/18/20、review log |
| 2026-11-17 04:38:00 UTC | Claude R13 FAIL + Codex R13 FAIL | `FP-14` 补齐 `#879`/`TC-1399`（`--porcelain --no-porcelain` 时 stderr 输出 `<old>..<new>  <src> -> <dst>` 且 stdout 为空，`AC=4/8`）；`FP-20` AC-1 与 `ADR-FP-03` 统一为仅显式配置 `fetch.unpackLimit` 时按阈值解包、未配置保持保留 pack 并在 `COMPATIBILITY.md` 记为 `intentionally-different`；`FP-18` 补 `split-from` 双向注记；`FP-11` 明确 `could not fetch '<remote>'` 与 `GIT_TRACE` `preparing to run up to <N> tasks`；`FP-01` 补 `fetch.rs:1593-1599` 兼容说明 | N/A | 审计表、FP-01/11/14/18/20、review log |
| 2026-11-17 04:46:00 UTC | Codex R14 FAIL + Claude R14 网络中断 | `FP-03` 补齐 `#683` 完整 `t/t3200-branch.sh#188` / `TC-0180` 门禁校验（`VER=3/8`） | N/A | 审计表、FP-03、review log |
| 2026-11-17 04:50:00 UTC | Codex R15 FAIL | `FP-03` `TC-0180` 门禁命令修正为实际脚本路径 `../libra-testcases/cases/tc0164-0189-branch-upstream/tc0164-0189-branch-upstream.sh 0180` | N/A | FP-03、review log |
| 2026-11-17 05:03:00 UTC | Codex R16 FAIL + Claude R16 网络中断 | 新增 `DEP-FP-07` 明确 `TC-0180`（`branch --delete-merged` + `remote.<r>.push`）跨计划依赖 `plan-20261117-branch.md`，将 `FP-03` 独立验收收敛为 `push` 侧 `test_push_uses_remote_push_refspec_config` 与 `lib command::push::test`（`VER=2/8`），避免 `FP-03` 在 `branch` 计划合入前被 `TC-0180` 卡死；同步更新 `DEP-FP-05` 覆盖 `CL-05, CL-10, CL-13, CL-19, CL-20, CL-21` | N/A | 修订历史、依赖表、审计表、FP-03、review log |
| 2026-11-17 05:08:00 UTC | Codex R17 FAIL | 将 `DEP-FP-07` 改为 `incoming`（等待 `plan-20261117-branch.md` 合入 `branch --delete-merged`），并在本计划新增 `FP-22`（`implementation`，`independent`，`Dependencies: FP-20, DEP-FP-07`）显式承接 `#683` 完整 `tc0164-0189-branch-upstream.sh 0180` / `^TC-0180 PASS$` 门控与 `DEFER-FP-02` 14 天超时降级，接入 `完成判据` 闭环 | 新增 `FP-22` 承接 `#683` `TC-0180` 闭环 | 修订历史、DAG、DEP-FP-07、发布分组、Phase 3、审计表、FP-03/22、追溯表、M3、DEFER-FP-02、完成判据、review log |
| 2026-11-17 05:10:00 UTC | Codex R18 FAIL | 移除违反 G-02 的测试拆卡 `FP-22`，由 `FP-03` 独立完整交付 `#683` `push` 侧 `remote.<name>.push` 行为与测试；将依赖本批次之外 `branch --delete-merged` 的完整 `TC-0180` 外部脚本（`tc0164-0189-branch-upstream.sh 0180`）登记为 `DEFER-FP-02` + `DEP-FP-07`（`outgoing` 移交后续 `branch --delete-merged` 计划，与 `DEFER-BR-02` 对齐），并在 `完成判据` 排除延后项验证矛盾 | 移除 `FP-22`（并入 `DEFER-FP-02`） | 修订历史、DAG、DEP-FP-07、发布分组、Phase 3、审计表、FP-03/20、追溯表、M3、DEFER-FP-02、完成判据、review log |
| 2026-11-17 05:17:00 UTC | Codex R19 FAIL | 补齐 `ADR-FP-03`/`FP-20` `GC-08` 多对象解包事务边界与崩溃恢复（内容寻址临时文件校验后逐对象 `rename` 入 `.libra/objects/<xx>/<yyyy>` 幂等落盘，全部就绪并清理暂存目录后才提交 ref 更新，补崩溃注入测试）；同步 `成功定义` 与 `DEFER-FP-02`（`Owner: CLI / Git-compat 维护者`）说明 `#683` `TC-0180` 延后边界；展开全卡 `Docs and compatibility impact` 的 `EN+zh` 为逐文件显式路径 | N/A | 修订历史、成功定义、ADR-FP-03、FP-01..FP-21 文档字段、DEFER-FP-02、review log |
| 2026-11-17 05:22:00 UTC | Codex R20 FAIL | `ADR-FP-03` 与 `FP-20` 对齐 Git 默认 `fetch.unpackLimit = 100`（仅对 `libra fetch` CLI 路径生效，`pull`/`clone`/`mega2_traces` 显式传 `None` 保持保留 pack 不受影响，满足 `#863` 兼容目标）；修正 `FP-20` `AC=2/8`（不计 ER-06/ER-06a 强制门）与行 5 `R21` 状态标识 | N/A | 状态行、修订历史、ADR-FP-03、审计表、FP-20、review log |
| 2026-11-17 05:27:00 UTC | Codex R21 FAIL | `ADR-FP-03` 与 `FP-20` 改用 `path::try_objects()`（`src/utils/path.rs:46`）解析对象目录（支持 linked worktree），并在松散解包期间持有仓库 `install.lock` / `unpack` 租约保护直至 `refs/remotes/...` 与 `FETCH_HEAD` 提交完成，防止并发 GC 误删不可达松散对象；补充并发 GC 与 linked worktree 回归测试 | N/A | 状态行、修订历史、ADR-FP-03、FP-20、review log |
| 2026-11-17 05:32:00 UTC | Codex R22 FAIL | 修正 `ADR-FP-03` 与 `FP-20` 的真实接收路径为 `fetch.rs:2647` 直接调用的 `write_pack_and_index`（而非仅由 `mega2_traces` 调用的 `install_fetched_pack`，`pull`/`clone`/`mega2_traces` 传 `unpack_limit: None` 保持保留 pack）；并将松散解包期间的并发 GC 保护锁从 per-pack `install_lock` 改为 `gc.rs:261,433` 实际遵循的 `MaintenanceLock::shared(&configured_storage)`（解包前获取并在 `refs/remotes/...` 与 `FETCH_HEAD` 提交完成后释放，禁止持共享锁期间升级独占锁），补充并发 `gc --prune=now` 回归测试 | N/A | 状态行、修订历史、ADR-FP-03、FP-20、review log |
| 2026-11-17 05:37:00 UTC | Codex R23 FAIL | 在 `ADR-FP-03` 与 `FP-20` 中要求松散解包路径同样构造并校验 `PackCommitEdges` 与完整 `validate_network_history`（`fetch.rs:2659,2779` 等价历史完整性与浅边界校验），未通过前 fail-closed 拒绝且不发布 refs（也不留半成品对象），并补充残缺/畸形网络历史 pack（`< unpackLimit`）拒绝回归测试 | N/A | 状态行、修订历史、ADR-FP-03、FP-20、review log |
| 2026-11-17 05:43:00 UTC | Codex R24 FAIL | 修正 `ADR-FP-03`/`FP-20` 执行时序为“在持有 `MaintenanceLock::shared` 下先将校验通过的松散对象原子 `rename` 落盘至 `<objects_dir>/<xx>/<yyyy>` 使 `configured_storage`（`shallow_response_validation.rs:296`）可见，再运行 `validate_network_history` 与 `PackCommitEdges` 校验，通过后才提交 `refs/remotes/...`/`FETCH_HEAD` 并释放锁（失败则不更新 refs 并清理新装对象）”；同时在 `FP-20` 纳入 `src/command/gc.rs`（`MaintenanceLock::try_exclusive` 下清理过期 `tmp_unpack_*` 目录）并补充有效小网络 pack 与残缺历史 pack 双测试 | N/A | 状态行、修订历史、ADR-FP-03、审计表、FP-20、review log |
| 2026-11-17 06:58:00 UTC | Claude R25 FAIL + Codex R25 FAIL | 1) 修复 `FP-18` 自锁死：因 `src/cli.rs:3718-3722` 在整条 `fetch` 命令期间持有 `MaintenanceLock::shared(storage)`，改为在 `src/cli.rs` 释放 `_maintenance_lock` 后的 post-dispatch 钩子触发 `gc --auto`（纳入 `src/cli.rs` 与 `T-1`，并补测证明 `repack -a -d` 实际打包无 `LBR-CONFLICT`）；2) 修复 `ADR-FP-03`/`FP-20` 锁与存储设计：直接复用 `cli.rs:3718` 对 `storage`（`repo_path`，含 linked worktree）持有的命令级 `MaintenanceLock::shared`（移除不存在的 `MaintenanceLock::shared(&configured_storage)` 调用），通过 `configured_storage.put`（`client_storage.rs:2549`）写入松散对象以保持分层后端与 object-index 修复标记一致，校验失败时不删除已落盘内容寻址对象（交由 `gc --prune` 清理，防并发 `fetch` 误删）；3) 修复 `gc.rs` `tmp_unpack_*` 清理早退：在 `MaintenanceLock::try_exclusive` 下即使 `candidate_oids.is_empty()` 或 `cutoff == Never` 也清理过期（mtime 早于截止时间）`tmp_unpack_*` 目录并补零候选测试；4) 修正 `validate_response_shallow_updates`（`fetch.rs:2677`）与 `validate_fetched_history`（`fetch.rs:2776`）名称与锚点、补全 `FP-20` `gc.rs`/`gc` 文档与 `AC=7/8` 独立谓词计数 | N/A | 状态行、修订历史、ADR-FP-03、审计表、FP-18、FP-20、追溯表、review log |
| 2026-11-17 07:03:00 UTC | Codex R26 FAIL + Claude R26 网络中断 | 在 `FP-20` `Verification` 显式补充 `cutoff == Never`（`gc.pruneExpire=never` / `--no-prune`）下仍清理过期 `tmp_unpack_*` 目录的回归测试场景；将 `FP-20` `AC` 计数由 `7/8` 校准为 `6/8`（按 G-03 不计入 ER-06/ER-06a 强制文档门）并同步审计表 | N/A | 状态行、修订历史、审计表、FP-20、review log |
| 2026-11-17 12:27:28 UTC | 用户单审指令 | 计划提交门改为 Codex 单审，不再要求 Claude Review | N/A | 头部、成功定义、Phase 0、M0、完成判据 |
| 2026-11-17 12:27:28 UTC | 机械自审：网站交付合同 | 现行网站路径统一到权威 `/Volumes/Data/gitmono/libra-backend`；补按卡同步、前端验证、独立 Claude Code PASS、cf 签名推送及部署门 | N/A | 使用规则、现行网站写集 |
| 2026-11-17 16:30:00 UTC | Codex R28-A FAIL/P1 | FP-03 补齐配置多值合并、通配符展开、负向排除及显式参数覆盖的 Git 对照，明确 FP-04 复用引擎不得降低前卡配置支持 | N/A | FP-03、FP-04、审计表、Review log |
| 2026-11-17 16:45:00 UTC | Codex R29-A FAIL/P1 | FP-04 增加配置 refspec 与 prune 组合测试并复跑 FP-03；FP-21 补匹配 lease 删除成功、无 lease 删除与 Git 对照，重计粒度 | N/A | FP-04、FP-21、审计表、Review log |
| 2026-11-17 17:00:00 UTC | Codex R30-A FAIL/P1 | FP-21 拆分陈旧更新/删除及空期望创建/拒绝为独立谓词（8/8）；FP-07 明确镜像更新、无 prune 保留和启用 prune 删除的 Git 对照 | N/A | FP-21、FP-07、审计表 |
| 2026-11-17 17:15:00 UTC | Codex R31-A FAIL/P1 | FP-07 按有效 prune 模式及配置/CLI 优先级限定删除，拆出六条验收与 Git 对照 | N/A | FP-07、审计表 |
| 2026-11-17 17:30:00 UTC | Codex R32-A FAIL/P1 | FP-07 固定 CLI last-wins、remote 优先于全局和默认 false，补双向配置冲突与 CLI 顺序的 Git 对照 | N/A | FP-07、Review log |
| 2026-11-17 18:00:00 UTC | Codex R33-A 范围、结论与发现均未核实（原始输出未找到，不得以 R31-A 代证） | 未核实；后续仅以 R34-A 及之后可核验复审处理 FP-05/FP-07 | N/A | FP-05、FP-07、Review log |
| 2026-11-17 17:30:00 UTC | FP-07 粒度修订 | 将配置优先级与 CLI 覆盖拆分，默认 false 由有效模式关闭验收及测试矩阵覆盖，完整合同按七项行为验收和完整安全矩阵计数 | N/A | FP-07、审计表 |
| 2026-11-18 08:19:08 UTC | 本轮 Codex 复审修订 | 统一网站交付路径为 `/Volumes/Data/gitmono/libra-backend`（用户最新指令指定；`AGENTS.md` 的 `/Volumes/Sea/libra-backend` 已被取代且本机不存在），并在「文档职责」写明权威依据与 STOP 条件；`FP-02` 补不存在路径与非仓库目录的 `LBR-CLI-003` 具名回归与四场景 Git 对照；`FP-07` 验收由 11 项重算为 13 项独立判据；`FP-13` 明确普通拉取更新 `FETCH_HEAD`、仅 `--dry-run` 保持不变；`FP-15` 拆为 `FP-15a`/`FP-15c`/`FP-15b` 三张家族子卡与唯一发布点 `FP-15R`，固定 Git 2.56.0 对照的来源、构建与身份核据，`--no-prune-tags` 覆盖配置以负向与正向双对照验证 | N/A | 全文对应章节 |
| 2026-11-18 09:43:54 UTC（更正：原记 `2026-11-18 10:35:00 UTC`，晚于该行实际写入；该次写入后文件修改时间为 09:43:54 UTC，故以此为写入时间上限如实更正） | Codex 复审 FAIL 修复（验证契约与门禁） | 新增「验证执行协议」（精确 `-E 'test(=完整路径)'`、先行 `cargo nextest list` 恰好 1 匹配、记录 exit code 与四计数、逐条 AC 断言、模块回归标为共享回归组）；裸名过滤补全模块路径、损坏过滤 `-E 'test(=X)'::后缀` 修正、模块过滤统一为 `test(/^X::/)` 并标共享回归组；每卡补「AC → 断言映射」并写明 exit code／stdout+stderr 文案／前后状态期望；补「文档 AC」逐文件验收映射与「文档与网站验收命令」块（含家族子卡只做本地门、唯一发布点统一推送、哈希与提交树核对、部署绑定 `$FINAL`、STOP 条件）；完成判据补计划 Codex 终审字面 `VERDICT: PASS` 与最终树 `HEAD`/`HEAD^{tree}` 记录及树变重跑；追溯表模块过滤标注共享回归组 | N/A | 执行检查必备需求、全部任务卡、完成判据、追溯表、修订历史 |
| 2026-11-18 13:13:26 UTC | 用户 2026-11-18 双审指令 | 计划文本提交门由「Codex 单审、不再要求 Claude Review」（2026-11-17 12:27:28 UTC 行）改为 Claude（Subagent）与 Codex 对同一受审字节双审 `VERDICT: PASS` 方可提交；只改计划提交门措辞（头部审阅合同、Phase 0 进入条件、M0、完成判据），执行期 ER-05 review 与网站 Claude Code `PASS` 门不变 | N/A | 头部、Phase 0、M0、完成判据 |
| 2026-11-18 13:13:26 UTC | 时间戳核对（Claude subagent claude-opus-5-5） | 2026-11-18 10:35:00 UTC 行晚于实际写入，已在该行如实更正为写入时间上限 09:43:54 UTC；2026-11-17 16:30:00–18:00:00 UTC 各行为整点/整刻记录、无法核实到秒，且 17:30:00 UTC「FP-07 粒度修订」行排在 18:00:00 UTC 行之后（逆序）——这些历史行保持原文，在此登记 | N/A | 修订历史 |
| 2026-11-18 13:13:26 UTC | 双审 R1 前按六类清单全文重写（Claude subagent claude-opus-5-5，依据 2026-11-18 源码与评审期预探） | 统一「验证执行协议」（`Ref`/`RefMap`/`R`/`W`/`S` 快照、精确过滤、Git 同夹具期望复核）与「文档与网站验收命令」第 1–8 步（W-1/W-2，不落盘）；网站路径依据改为用户 2026-11-18 11:55:09 UTC 确认（`DEP-FP-08`）；全部卡改写为单谓词 AC 与逐条映射、补全夹具与 `COMPATIBILITY.md` 行；`Full-suite trigger` 统一为 T-1（版本面）；自 `FP-01` 拆出 `FP-23`（短目标补全与匿名显式目标写入）；`FP-04` 移到 `FP-03` 之前（配置 refspec 复用命令行引擎）；`FP-07` 收窄为默认 remote 回落并撤销 `EX-FP-07-PRUNE`；`FP-10` 修正 `--jobs` 负值语义与 `--all` 不互斥事实、补 `GIT_TRACE` 追踪行；`FP-19` 补 `--no-all`；废弃 `FP-15a/c/b/R`（→ `DEFER-FP-03`）与 `FP-20`（→ `DEFER-FP-04`，ADR-FP-03 改为 Superseded），`TC-1338` 改为 `DEP-FP-06` 补充证据；新增 ADR-FP-04（自动打包在维护锁释放后）、ADR-FP-05（人读报告格式） | FP-01 → FP-01, FP-23；FP-15a/FP-15c/FP-15b/FP-15R → 废弃（DEFER-FP-03）；FP-20 → 废弃（DEFER-FP-04、FP-18） | 全文：头部、文档职责、事实基线、缺口、与其它计划的关系、ADR、执行检查必备需求、实施顺序、依赖登记表、发布分组、Phase、字段默认、废弃编号、审计表、全部任务卡、测试矩阵、追溯表、里程碑、故障恢复、风险、性能、兼容收口、非目标与延后项、完成判据 |
| 2026-11-18 13:25:41 UTC | Claude R1 FAIL + Codex R1 FAIL（A/B/C/D 四分区，受审 SHA-256 `ef9fe264659d`） | 按两方发现修订：G-03 计数口径改为共享回归组不计入 `VER`、禁止以元组合并不同观测对象，全部卡重算 `VER`（卡特有用例数 × 2）；`R`/`S` 快照改为 `reference`/`reflog`/`config_kv` 全部行全部列，并允许夹具「前置断言」；ADR-FP-04 改正松散对象判定（沿用 Libra 精确 `>=`，登记有意差异，不再称对齐 Git）；ADR-FP-05 补 URL 显示（沿用已脱敏 URL、去尾 `/` 与 `.git`）与 Git `refcol_width`/`term_columns` 列宽算法；`FP-01` 改正 `remote_get` 引用为 `remote.c:800-830`、`RefMap`/`config_kv` 全量不变断言（AC=8）；`FP-23` 改为整表等值并补拒绝后 `S` 不变；`FP-04` 各 `--prune` 改为 `RefMap` 整表等值（覆盖模式外与负向保留）；`FP-03` 目标 `main` 落后于源以验证命令行覆盖确实推送、合并新增断言为整表等值并补拒绝后 `R` 不变；`FP-06` AC7 只断言解析 `Ok`；`FP-21` 夹具自含（不再「同 F06」）且 AC7 先在 `dst` 建 `branch`；`FP-07` 按 Git `remotes_remote_for_branch`（唯一 remote 或 `origin`）补唯一非 origin remote 场景、失败场景改为两个非 origin remote，并修正 AC8 指向失败调用；`FP-08` 去掉 dry-run 专项、`--json` 的 `refspec` 改为空格连接、补 `--set-upstream` 多 refspec 告警断言；`FP-09` AC2/AC3 改为整表等值；`FP-19` 夹具自含并在克隆后推进 `D/o` 以区分「未 fetch」；`FP-11` 诊断格式改标为 Libra 契约；`FP-12` 补 `--multiple`；`FP-13` 去掉元组（无更新只断言 stderr 为空）；`FP-15` 改为 `RefMap` 整表等值（AC=5）；`FP-17` 夹具改在临时工作仓库造 tag 后推入裸镜像；自 `FP-18` 拆出 `FP-24`（`gc --auto` 的 `gc.autoPackLimit` 判定与提示，`src/command/gc.rs`、`gc` 文档），`FP-18` 只保留 fetch 后触发（`src/cli.rs`、`src/command/fetch.rs`）；审计表 axis 去掉裸 `<name>`；`FetchArgs` 与 `cli.rs` 锚点修正 | FP-18 → FP-18, FP-24 | 执行检查必备需求、ADR-FP-04/05、事实基线、实施顺序、Phase 3、审计表、FP-01/03/04/06/07/08/09/11/12/13/15/17/18/19/21/23/24、测试矩阵、追溯表、性能摘要、完成判据 |
| 2026-11-18 13:31:23 UTC | Claude R2 FAIL + Codex R2 FAIL（A/B/C/D 四分区，受审 SHA-256 `9bde2b6fc0bc`） | 失败类 AC 的映射统一为「退出码非 0 且 stderr 含稳定码」（同一错误结果的两个读取面，计为一个谓词），全部失败映射同步；`RefMap` 改由 `libra show-ref` 取得（`for-each-ref` 对自定义命名空间显示有既有缺陷，登记风险）；新增 `M(r)` 元数据快照；ADR-FP-05 改正 summary 列宽（Libra 固定 7 位缩写，Git 动态缩写为有意差异）并限定非 `--verbose`（Git `--verbose` 的 `[up to date]` 行不实现，有意差异）；`codeql.yml` 触发条件写准；`FP-01` 合并引用/配置不变为 `M(child)` 并补裸仓库目录名 fetch（`pbare`）；`FP-02` 补非裸仓库目录名 push（`wt`）；`FP-03` 夹具改为只有 M0 时先推入目标；`FP-04` 补源侧负向排除场景；`FP-09` 读出要求改为 `rev-parse`/`show-ref` 并补存储锚点；`FP-10` AC3 按已配置 URL 归属；`FP-11` 拆出 `LBR-CLI-002` 失败 AC（AC=8）；`FP-12` AC5 文本与映射同谓词；`FP-13` 登记缩写与 `--verbose` 差异；`FP-18` 以 `--dry-run` 不触发自动打包替换「Should I try again」AC | N/A | 执行检查必备需求、ADR-FP-05、D 组、FP-01/02/03/04/09/10/11/12/13/18 及全部失败类映射、风险登记 |
| 2026-11-18 13:35:38 UTC | Claude R3 FAIL + Codex R3 FAIL（A/B/C/D 四分区，受审 SHA-256 `ffc13a70d6f7`） | `RefMap` 改为只去掉 `show-ref` 合成的符号 `refs/remotes/<remote>/HEAD` 并写明枚举范围与夹具前置断言（远程跟踪行只挂已配置 remote），删去组合快照 `M`，`S` 的 `FETCH_HEAD` 路径区分裸/非裸并声明 `S` 为一个整体谓词；风险登记补 `for-each-ref` 全名分支显示缺陷；`FP-01` 拆回「引用不变」与「配置不变」两条 AC 并删去对拒绝调用的 `S` 断言；`FP-02` 夹具为 AC2/AC3 配置同名 remote `src1`；`FP-05` AC2 改为子串断言（与 Git `remote: ` 前缀兼容）；`FP-08`/`FP-10`/`FP-11`/`FP-19` 夹具写明源仓库无 tag；`FP-09` AC3 只断言 `refs/repo_d/`（`extra_dir` 存为未配置 remote 名下，`show-ref` 不枚举）；`FP-10` AC3、`FP-11` AC6 改按 OID 判定 `FETCH_HEAD`（本地路径 URL 会被规范化）；`FP-13` 以 `--dry-run --no-write-fetch-head` 仍显示计划行替换重复的 `FETCH_HEAD` 内容 AC；`FP-18` 以 `--dry-run` 前后 `S` 不变、默认阈值下 stderr 逐字等于 fetch 报告与 stdout 为空替换原输出断言 | N/A | 执行检查必备需求、风险登记、FP-01/02/05/08/09/10/11/13/18/19 |
| 2026-11-18 13:38:26 UTC | Claude R4 FAIL + Codex R4 FAIL（A/B/C/D 四分区，受审 SHA-256 `a47fe1b18bdd`） | 按 G-09 与模板 `plan-template.md:460` 把拆出卡 `FP-19`、`FP-23`、`FP-24` 的卡片移到所属 Phase 末尾（执行 DAG 不变，「实施顺序」写明编号/位置与执行顺序之别）；ADR-FP-05 写明 `--dry-run` 时总显示 `FETCH_HEAD` 计划行（含同时给 `--no-write-fetch-head`）；`RefMap` 写明 `refs/remotes/<已配置 remote>/HEAD` 必为合成条目的依据（fetch 拒绝写以 `HEAD` 结尾的远程跟踪目标）；测试矩阵 M-INT 限定为 `command_test` 用例；`FP-10` AC2 文本改为与映射同一 `RefMap` 谓词；`FP-12` AC3 由先行 `libra fetch` 产生字节 B 并在源端推进后再验 `--no-write-fetch-head`；`FP-13` 「无更新不输出」限定非 `--verbose` | N/A | 实施顺序、ADR-FP-05、执行检查必备需求、测试矩阵、FP-10/12/13、卡片位置（FP-19/23/24） |
| 2026-11-18 13:41:42 UTC | Claude R5 FAIL + Codex R5 FAIL（A/B/C FAIL、D PASS，受审 SHA-256 `ed1a7b65049d`） | ADR-FP-05 的 `COLUMNS` 改按 Git `atoi` 语义（`pager.c:213-240`）；`RefMap` 改为调用前后均断言远程跟踪行只挂已配置 remote（错误写入未配置 remote 名下即测试失败）；`FP-01` AC2 改用完整 `R(child)` 快照；`FP-09` 把未配置 `<x>` 的 `refs/remotes/<x>/...` 目标改为全名行存储并使 AC3 断言两个命名空间四项；`FP-10` 夹具在克隆后推进 `D/o` 以区分误拉取 `origin`；`FP-21` 以修订表达式 stale 删除拒绝（`t5533:171-180`）替换原拒绝后 `R` 不变断言，使跳过租约检查的实现可被发现；`FP-23` AC3 改用已配置 remote 下的 `remotes/origin/copy`；`FP-24` 「不运行」类 AC 改为完整 `S(D/g)` 快照 | N/A | ADR-FP-05、执行检查必备需求、FP-01/09/10/21/23/24 |
| 2026-11-18 13:44:54 UTC | Claude R6 FAIL + Codex R6 FAIL（A/B/C/D 四分区，受审 SHA-256 `687b3bb6aec4`） | `RefMap` 合成 HEAD 依据改为「fetch 只拒绝恰为 `refs/remotes/<remote>/HEAD` 的目标」；`base.yml` 触发写准（PR 不限分支）；「验证执行协议」写明夹具步骤与快照读取在各自仓库目录执行；单元行与 M-UNIT 纳入 `command::gc::tests`；`FP-01`/`FP-02` 夹具只把 `cwd` 约束限于被测调用；`FP-06` 的租约期望改用完整 OID（修订表达式留给 `FP-21`）；`FP-21` 以「`main:main` 删除成功」替换跟踪租约删除，使删除路径的表达式解析可被区分；`FP-24` 新增 `gc.autoPackLimit` 缺省解析为 50 的单元断言（AC=8、VER=4） | N/A | 执行检查必备需求、测试矩阵、FP-01/02/06/21/24 |
| 2026-11-18 13:47:38 UTC | Claude R7 FAIL + Codex R7 FAIL（A/C FAIL，B/D PASS，受审 SHA-256 `9501fa5b82d2`） | `FP-10` 的 `--jobs` 改为接受但串行执行（不再承诺并发，登记有意差异与新增 `DEFER-FP-05`），同步评审结论、字段默认、测试矩阵与性能摘要；ADR-FP-05 列宽规则补 Git 对浅历史拒绝项的排除；`FP-21` Description 去掉无映射的「远端不变」表述 | N/A | 非目标、评审结论、ADR-FP-05、字段默认、测试矩阵、性能摘要、非目标与延后项、FP-10、FP-21 |
| 2026-11-18 13:51:07 UTC | Claude R8 FAIL + Codex R8 FAIL（C FAIL，A/B/D PASS，受审 SHA-256 `380506545d1a`） | `FP-23` 夹具把 `cwd` 约束限于被测 fetch 调用，夹具建立与快照读取回到各自仓库目录（与「验证执行协议」第 4 项一致）；`FP-11` AC5/AC6 的计数类断言改为保留重复的排序列表比较，可区分重复输出；性能摘要的串行预算改记为设计约束并写明卡特有用例不观测传输重叠 | N/A | FP-23、FP-11、性能与容量摘要 |
| 2026-11-18 13:57:27 UTC | Claude R9 PASS + Codex R9 FAIL（A/C FAIL，B/D PASS，受审 SHA-256 `1fa843a6db68`） | 按 G-09 自 `FP-21` 拆出 `FP-25`（空期望租约 `<ref>:`，Phase 1 末尾追加；执行顺序 `FP-21` → `FP-25` → `FP-07`）：`FP-21` 为两个 stale 拒绝调用各增 `R(D/src)` 前后相等断言（AC=8），`FP-25` 夹具把远端 `branch` 置于 B（C 的父提交），使不带租约推送为快进、可区分忽略空期望的实现并断言 `R(D/src)` 不变（AC=3）；故障恢复矩阵写明 `R` 不变的断言来源；`FP-10` AC3 的副本预写 X1 旧 `FETCH_HEAD` 记录以观测「先截断再追加」，夹具补 O1、T1、X1、X2 两两不同；同步 ADR-FP-02、任务 ID、依赖范围、Phase 2 进入条件、测试矩阵 M-SEC 与卡数 | FP-21 → FP-21, FP-25 | 成功定义、使用规则、评审结论、ADR-FP-02、实施顺序、Phase 1/2、测试矩阵、故障恢复矩阵、完成判据、FP-06、FP-07、FP-10、FP-21、FP-25 |
| 2026-11-18 14:03:44 UTC | Claude R10 PASS + Codex R10（A/B/D PASS；C 未报 P0/P1 但给出 FAIL）+ Codex R11-C 同字节复审 FAIL（受审 SHA-256 `ad01616f6482`） | `FP-23` 新增 AC6：`main:tags/t` 的 stderr 须含标签目标专用诊断 `tag destinations are not supported`，区分「补全后按标签目标拒绝」与「未补全即按完整 ref 校验拒绝」（AC=7）；`FP-10` AC7 由仅成功退出改为断言 `--jobs=0` 的追踪行含 CPU 核数（`available_parallelism`），Description 写明取值来源 | N/A | FP-10、FP-23 |
| 2026-11-18 14:06:42 UTC | Claude R12 PASS + Codex R12（A FAIL，B/C/D PASS，受审 SHA-256 `4a76a229239c`） | 更正 2026-11-18 13:57:27 UTC 行的「原卡 → 新卡」字段（原误记 `N/A`，应为 `FP-21 → FP-21, FP-25`，按 G-09 拆分协议登记）；ADR-FP-05 列宽截断判断写明 `w` 为 Git `utf8_strwidth` 的终端显示宽度 | N/A | 修订历史、ADR-FP-05 |
| 2026-11-18 14:10:24 UTC | Claude R13 PASS + Codex R13 FAIL（A/C FAIL，B/D PASS，受审 SHA-256 `42c182093ef7`） | 「验证执行协议」`W(r)` 改为不跟随符号链接的「路径 → (类型, 权限位, 内容)」清单，可检出只改可执行位、文件类型或链接目标的变化；`FP-21` AC8 改为 `dst` 提交 D 后以 `main:main^`（解析为 C=远端）删除，使删除路径的修订表达式解析可被区分；`FP-09` 写明保留目标仅为 `refs/heads/HEAD` 与 `refs/remotes/<remote>/HEAD`（现有限制、登记有意差异），嵌套 `x/HEAD` 与 `refs/<ns>/.../HEAD` 不受限，新增 AC7（嵌套 `refs/remotes/origin/x/HEAD`）、AC8（`refs/custom/HEAD`）并同步兼容登记（AC=8） | N/A | 执行检查必备需求、FP-09、FP-21 |
| 2026-11-18 14:14:13 UTC | Claude R14 PASS + Codex R14 FAIL（A/D FAIL，B/C PASS，受审 SHA-256 `a0fa23ae2945`） | ADR-FP-02 把 `--force` 覆盖范围限定为推送端 `set_ref_status_for_push` 的拒绝原因，写明接收端 `receive.denyCurrentBranch` 拒绝不受影响（与 `FP-05` 一致）；`FP-16` 夹具增加调用前 `tag-1`=E1、`tag-2` 不存在的前置断言，AC3/AC7 改为调用前后 `Ref` 逐字相等；`FP-24` AC8 改为直接测试 `gc --auto` 与 `FP-18` 共用的触发判定函数在缺省配置下对 50/51 个 pack 的结果，单元用例更名为 `command::gc::tests::auto_gc_decision_default_pack_limit_is_50`，同步 Description 与测试矩阵单元行；`FP-12` 夹具对「删除 `FETCH_HEAD`」增加调用前不存在的前置断言 | N/A | ADR-FP-02、测试矩阵、FP-12、FP-16、FP-24 |
| 2026-11-18 14:27:49 UTC | Claude R15 PASS + Codex R15（A/B/D FAIL，C PASS；B 未报 P0/P1、仅以核验不完整判 FAIL，受审 SHA-256 `418e7d0053de`） | `FP-24` 把原 AC8 以列表合并的 50/51 两个边界拆为 AC7（50 个 pack 判不触发）与 AC8（51 个判触发），删去缺省配置下 `S(D/g)` 不变一条，改以新 AC6「缺省配置下 `libra gc --auto` 的 stderr 不含 `Auto packing`」覆盖缺省配置端到端（AC2 已断言触发时必输出提示，误触发或无条件提示均使其失败），`.keep` 场景移为 AC5，仍 AC=8/8；`FP-17` AC4 改为断言 `refs/tags/tag_lone`→L1（自动跟随取不到 L1，可区分同时忽略配置与命令行的实现），Git 依据改为钉选 `builtin/fetch.c:600-610,1927-1932`；「验证执行协议」第 4 项 `W(r)` 排除顶层 `.libra-test-home/`（`run_libra_command*` 在调用 cwd 下创建的隔离 HOME，`tests/command/mod.rs:354-360`），消除 `FP-13` AC7 等在某目录首次作为被测调用 cwd 时的前后 `W` 误不等；「事实基线」写明本机 Git 仓库工作区 HEAD 已前移至 `750f5de7df`，Git 行号一律按钉选提交读取 | N/A | 事实基线、执行检查必备需求（验证执行协议第 4 项）、FP-17、FP-24、review log |
| 2026-11-18 14:34:54 UTC | Claude R16 PASS + Codex R16（A/C FAIL，B/D PASS，受审 SHA-256 `63e52b8e608d`） | 「验证执行协议」第 4 项 `S(r)` 的对象目录清单改为含 `*.lock` 的全部文件（pack 安装/保留锁只由写入方创建并常驻、读取方不创建，写明锚点），使「无写入」类断言能发现锁文件残留；`FP-24` 改为在 `gc --auto` 实际路径上验证缺省阈值：夹具以重复「源端提交 + `libra fetch --no-auto-gc`」造出 50 与 51 个 pack，AC6 为缺省配置 50 个 pack 时前后 `S(D/g)` 相等、AC7 为 51 个 pack 时 `gc --auto` 后 pack 数为 1、AC8 为 AC6 的 stderr 不含 `Auto packing`，删去判定函数单元用例（VER 4/8→2/8，改为 `command::gc::tests` 共享回归组），同步测试矩阵与「验证执行协议」第 1 项；`FP-08` 改正 `--set-upstream` 的 Git 规则表述（Git 按 ref 映射中无本地目标的条目计数，`builtin/fetch.c:2114-2165`、`remote.c:2158-2159`），Libra 对多于一个命令行 refspec 一律告警不写，带目标 refspec 参与时登记为有意差异，AC7 的 Git 依据限定为两个 refspec 均不带目标 | N/A | 执行检查必备需求（验证执行协议第 1、4 项）、测试矩阵、追溯表、审计表、FP-08、FP-24、review log |
| 2026-11-18 14:37:17 UTC | Claude R17 PASS + Codex R17（D FAIL，A/B/C PASS，受审 SHA-256 `da78f5405e13`） | `FP-24` 夹具在克隆后随即写入 `fetch.unpackLimit=1` 与 `gc.autoDetach=false`（Libra 不读取二者；使钉选 Git 的同构夹具同样逐次保留 pack——Git 缺省 `unpack_limit = 100` 会把小 pack 解包，`fetch-pack.c:43,991-999`——并以前台提示文本运行，`builtin/gc.c:656-661`），使 AC2、AC5–AC7 的「Git 同夹具期望」可按「验证执行协议」第 4 项复核 | N/A | FP-24、review log |
| 2026-11-18 14:40:43 UTC | Claude R18 PASS + Codex R18（A/C FAIL，B/D PASS，受审 SHA-256 `bcf2de50e44e`） | `DEP-FP-01` 写锁释放时点改为：仓库内文件持有至本卡 Libra PR squash merge（C 组第 ⑧ 步）后释放，网站页持有至「文档与网站验收命令」第 7 步完成后释放（`remote-pending` 期间继续持有，STOP 时由协调者裁决），第 8 步要求第 3–7 步开始前确认仍持有网站页写锁；`FP-10` 的 `FETCH_HEAD` 截断改为与 Git `fetch_multiple` 一致的条件截断（未给 `--append` 时截断，给出时保留旧记录后追加，`builtin/fetch.c:2330-2334`），新增 AC3（`--append --multiple` 后第一列为 `[X1, O1, T1]`），原 AC1 与「`origin` 跟踪引用不变」合并为一条 `RefMap(w)` 整表等值（误拉取 `origin` 同样使整表不等），AC6 同改整表等值，仍 AC=8/8，兼容登记补 `--append` | N/A | 依赖登记表（`DEP-FP-01`）、执行检查必备需求（文档与网站验收命令第 8 步）、FP-10、review log |
| 2026-11-18 14:42:51 UTC | Claude R19 PASS + Codex R19 PASS（A/B/C/D 四分区全部 PASS，受审 SHA-256 `bda4c9f4798d`） | 双审对同一受审字节 `bda4c9f4798d2f5ab234ef57510daa794ba0b3800f4a3ae241de55095903380b` 均给出 `VERDICT: PASS`，计划可提交；本行、头部状态行与 review log 的 R19 行为 PASS 后记账，其余内容与受审字节相同；R19 残余 P2（`FP-01` 无 tag 夹具未覆盖匿名 fetch 不自动跟随 tag、`FP-05` 未知值与 `ignore` 时工作区快照、`FP-11` `--multiple` 部分失败的整体失败、`FP-21` 显式期望匹配的非快进更新）与此前登记的 P2 一并由计划协调者具名接受 | N/A | 头部状态行、修订历史、review log |

## 已决议设计决策

### ADR-FP-01: 裸本地仓库目录名识别边界（`repository_arg.rs` opt-in helper）

- **Status:** Accepted
- **Context:** `#719`、`#710`：未配置同名 remote 的裸目录名（`parent`、`src1`）被当作 remote 名而报 `remote not found`；`./x`、`../x` 与绝对路径已由 `is_anonymous_repository_spec`（`src/internal/protocol/repository_arg.rs:17-31`）识别，且该函数也被 `pull` 使用。
- **Decision:** 保留 `is_anonymous_repository_spec` 语义不变，在同文件新增 opt-in `is_anonymous_repository_spec_with_bare_dir(spec, cwd)`：现有判定命中，或 `spec` 不含 `:`、相对 `cwd` 解析为存在的目录且该目录是 Libra 仓库（非裸含 `.libra/`，或裸仓库含 `libra.db` 与 `objects/`）时返回 `true`。调用方必须先查已配置 remote（同名 remote 优先），只在未命中时调用。`FP-01` 只在 `fetch.rs` 接入，`FP-02` 只在 `push.rs` 接入；`pull` 不接入。
- **Alternatives considered:** 直接改 `is_anonymous_repository_spec`——拒绝，会隐式改变不在本计划写集内的 `pull`；按 Git 把任意未配置名当 URL/路径尝试——拒绝，会把拼写错误的 remote 名变成本地路径错误，丢失 `LBR-CLI-003` 提示。
- **Consequences:** `FP-01` 修改共享单一事实源，按 ER-13 命中 `T-1`；与 `plan-20261117-merge-pull.md` 经 `DEP-FP-02` 协调。未配置名与 Git 的差异登记于 `COMPATIBILITY.md`。
- **Revisit when:** 引入统一的传输 URL/路径解析器重构时。

### ADR-FP-02: `push --force` 与 `--force-with-lease` 的租约判定

- **Status:** Accepted
- **Context:** `#1036`：clap 层把 `--force` 与 `--force-with-lease` 设为互斥（`src/command/push.rs:118`）；租约 `<expect>` 只做字符串/前缀比较：空期望与远端不存在不匹配、与远端已存在的任意 OID 前缀恒匹配（`src/command/push.rs:1855-1893`）。
- **Decision:** 对齐 Git `set_ref_status_for_push`（`6de20f6092:remote.c:1682-1771`）：租约 `<expect>` 在本地按修订表达式解析为 OID；空 `<expect>` 表示远端该 ref 必须不存在；`=<ref>` 与裸形式以本地远程跟踪引用为期望；租约匹配即允许非快进更新与删除；租约不匹配记为 stale 拒绝（诊断含 `stale info`，`LBR-CONFLICT-002`）；同时给出 `--force` 时，`set_ref_status_for_push` 在推送端判定的拒绝原因（租约 stale、非快进等）被覆盖、更新照常执行并标记为强制更新；接收端策略拒绝（如 `receive.denyCurrentBranch` 未配置或为 `refuse` 时更新检出分支，`6de20f6092:builtin/receive-pack.c:1543-1556`，由 `FP-05` 保持）不受 `--force` 影响。`FP-06` 交付 `--force` 覆盖，`FP-21` 交付无 `--force` 时非空期望与跟踪租约的判定，`FP-25` 交付空期望判定。
- **Alternatives considered:** 让 stale 在 `--force` 下仍拒绝——拒绝（与 `t5533` #3 相反）。
- **Consequences:** 替换单元测试 `force_with_lease_conflicts_with_force`（`src/command/push.rs:4172`）；退出码沿用 `LBR-CONFLICT-002` 默认 128（Git 为 1），登记为有意差异。
- **Revisit when:** 扩展 `--force-if-includes` 语义时。

### ADR-FP-03: `fetch.unpackLimit` 小包解包（已废弃）

- **Status:** Superseded（2026-11-18，由 `DEFER-FP-04` 取代）
- **Context:** 原 `FP-20` 计划在 `libra fetch` 接收路径按 `fetch.unpackLimit` 把小 pack 解包为松散对象，涉及命令级维护锁、`ClientStorage::put`、历史完整性校验与 `tmp_unpack_*` 清理，历经十余轮评审未收敛。
- **Decision:** 不在本计划实现。`#863` 的上游场景（`t/t5510-fetch.sh:1423-1440`）设 `fetch.unpackLimit 1`，任何非空 pack 都保留，与 Libra 现行「接收即保留 pack」一致；自动打包由 `FP-18` 按 `gc.autoPackLimit` 完成。
- **Alternatives considered:** 继续保留 `FP-20`——拒绝，超出 18 个 issue 的验收范围。
- **Consequences:** Libra 对 `< fetch.unpackLimit`（默认 100）个对象的 fetch 仍保留 pack，登记为 `COMPATIBILITY.md` 有意差异与 `DEFER-FP-04`。
- **Revisit when:** 出现依赖小包解包行为的 issue，或对象存储统一打包/解包策略重构时。

### ADR-FP-04: fetch 后自动打包在命令级维护锁释放后执行

- **Status:** Accepted
- **Context:** `#863`；`src/cli.rs:3760-3772` 在整条 `fetch` 命令期间持有 `MaintenanceLock::shared(storage)`，`gc` 的 `repack -a -d` 与松散对象删除需要独占维护锁（`src/command/gc.rs:424-433`）；在 fetch 内部直接调用 gc 会与自身共享锁冲突。
- **Decision:** `src/cli.rs` 在 `Commands::Fetch` 分发成功返回、命令级共享锁释放后，若该次 fetch 非 `--dry-run` 且非 `--no-auto-gc`，调用 `gc` 的自动入口（与 `libra gc --auto` 同一判定函数，单一事实源，`FP-24` 交付）；需要时先向 stderr 输出 `Auto packing the repository for optimum performance.` 再以静默子配置运行，不需要时不输出任何内容。`gc --auto` 的 pack 数判定对齐 Git（`odb/source-files.c:369-386,544-565`）：`gc.auto<=0` 关闭；不带同名 `.keep` 的 pack 数 `> gc.autoPackLimit`（默认 50，`<=0` 不按 pack 数）即运行；松散对象判定沿用 Libra 现有精确计数与 `>= gc.auto`（默认 6700），不复刻 Git 的 `objects/17` 抽样与 `> DIV_ROUND_UP(gc.auto,256)*256`（`odb/source-files.c:326-342`），登记为有意差异。`pull`/`clone` 不触发。
- **Alternatives considered:** 在 `fetch_repository_with_result` 内触发——拒绝，会改变 `pull`/`clone` 并自锁；派生子进程 `libra gc --auto`——拒绝，父进程仍持共享锁。
- **Consequences:** `FP-24` 修改 `src/command/gc.rs`、`FP-18` 修改 `src/cli.rs` 与 `src/command/fetch.rs`（均 `T-1`）；与其它写 `src/cli.rs` 的计划经 `DEP-FP-01` 串行。`pull`/`clone` 不自动打包、无后台模式，登记为有意差异。
- **Revisit when:** 引入 `libra maintenance run --auto` 统一自动维护入口时。

### ADR-FP-05: fetch 人读报告的输出流与列格式

- **Status:** Accepted
- **Context:** `#878`、`#879`：Libra 把人读报告写到 stdout、格式与 Git 不同（`src/command/fetch.rs:1653-1739`）；Git 一律写 stderr（`6de20f6092:builtin/fetch.c:829-861`）。
- **Decision:** `FP-13` 把人读报告改到 stderr，按 Git `display_ref_update`（`6de20f6092:builtin/fetch.c:829-861`）格式 ` <code> <summary 左对齐补到 17 列> <src 左对齐补到 W 列> -> <dst>[  (<错误>)]`：summary 列宽按 Git `transport_summary_width` 为 `2×缩写宽度+3`，Libra 沿用现有固定 7 位缩写（`src/command/fetch.rs:1704-1705`），故恒为 17、`<old>..<new>` 为 16 字符（Git 按 `core.abbrev` 与唯一性加长缩写时列宽随之变化，Libra 不复刻，登记为有意差异）；`src`/`dst` 去掉 `refs/heads/`、`refs/tags/`、`refs/remotes/` 前缀；列宽 `W` 按 Git 非 `--verbose` 时的 `refcol_width`（`builtin/fetch.c:687-725`）：初值 10，只考察有本地目标、非 `HEAD`、未因浅历史被拒（`REF_STATUS_REJECT_SHALLOW`）、本次非「已是最新」的条目，跳过 `21 + w(src) + 4 + w(dst) >= 终端列数` 的条目（`w` 为 Git `utf8_strwidth` 计算的终端显示宽度，非字节数或字符数，`builtin/fetch.c:708-718`），其余取最大 `src` 显示宽度；终端列数按 Git `term_columns`（`6de20f6092:pager.c:213-240`）：环境变量 `COLUMNS` 经 `atoi` 解析（取数字前缀，如 `80junk` 得 80）大于 0 时取之，否则 stdout 为终端且其宽度大于 0 时取该宽度，否则 80。无本地目标而只进 `FETCH_HEAD` 的 ref 以 ` * branch <src> -> FETCH_HEAD` 显示（`--dry-run` 时总显示，即使同时给 `--no-write-fetch-head`；只有非 `--dry-run` 且 `--no-write-fetch-head` 时不显示，对应 Git 条件 `write_fetch_head || dry_run`，`builtin/fetch.c:1354`）；非 `--verbose` 时无更新不输出；Git `--verbose` 的 `[up to date]` 行（`builtin/fetch.c:959-979`）及其参与列宽不实现（Libra `--verbose` 只保留现有 `Fetching <remote> from <url>` 诊断，登记为有意差异）；删除 ` N objects fetched` 与 `Already up to date with '<remote>'`。`From <url>` 的 `<url>` 沿用现有构造时已脱敏的 remote URL（与 Git `transport_anonymize_url` 同目的，`builtin/fetch.c:729-760`），并按 Git 去掉末尾 `/` 与 `.git` 后缀。`--json`/`--machine`/`--porcelain` 契约不变。
- **Alternatives considered:** 保留 stdout 并加环境变量切换——拒绝，脚本按 Git 习惯从 stderr 读取。
- **Consequences:** 外部脚本若从 stdout 读取 fetch 人读报告需迁移；`FP-13` 标 `T-5` 跑全量；`COMPATIBILITY.md` 登记。
- **Revisit when:** 支持 `fetch.output=compact` 时。

## 全局工程约束

继承 [plan-template.md](plan-template.md) 的 `GC-01`..`GC-13`。特别强调：
- **GC-FP-01:** 所有 Git 行为期望以 `git@6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`（v2.56.0-116）的源码与上游测试为准；Libra 有意差异（稳定错误码与数值退出码、`--json` 结构、`refs/tags/*` fetch 目标、`pull`/`clone` 不自动打包等）必须写入对应卡的 `COMPATIBILITY.md` 登记要点。
- **GC-05 / ER-06a 文档同步:** 每张卡按其 `Docs and compatibility impact` 所列命令（`fetch`、`push` 或 `gc`）同卡更新 `docs/commands/<cmd>.md`、`docs/commands/zh-CN/<cmd>.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/<cmd>.en.md`，并对 `docs/development/commands/<cmd>.md` 与 `COMPATIBILITY.md` 对应行（`push` 222 / `fetch` 223 / `gc` 267）逐文件做语义核对（兼容矩阵原地修改、不增删行）；均按「文档与网站验收命令」第 1–7 步在「文档职责」指定的授权网站检出（`cf` 分支）完成同步、前端校验、Claude Code `PASS`、提交推送与部署核验。

## 执行检查必备需求（强制）

继承 [plan-template.md](plan-template.md) 的 `ER-01`..`ER-14`：
- **ER-04 / ER-08 / ER-13 / ER-14:** 每张 `implementation` 卡均为 `independent` 发布切片，完成后按 ER-08 执行 `patch + 1`、同步 `compat_version_surface_sync` 权威三处（`Cargo.toml`、`install.sh`、`install.ps1`）、工具链刷新 `Cargo.lock`；因每卡均修改 `install.sh`/`install.ps1`（`T-1`），C 组第 ④ 步除运行本卡 A 组 focused `nextest` 外，必须跑双环境全量 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`、`fmt`、`clippy`、`cargo build --release`，随后签名提交推送 PR，squash merge 后 `gh release create v<version>`。
- **ER-04 D 组远端后置门（每张 `independent` 卡默认适用；推送前以模板「仓库配置与 CI 展开规则」现场读取 workflow 复核 job 集合）:**
  - `base.yml`（`pull_request` 不限目标分支；`push` 限 `main`、`release/**`）：`format`、`clippy`、`redundancy`、`test` 等适用 job 在 PR head 与 main merge SHA 全部 green。
  - `codeql.yml`（`pull_request` 到 `main`、`push` 到 `main`/`release/**`）：`analyze` green。
  - `release.yml`（`push` `v*` tag，由 `gh release create "v${VERSION}" --target "$MERGE_SHA"` 触发）：`build-and-upload`、`upload-install-scripts`、`update-homebrew-tap`、`verify-homebrew-formula`、`request-stable-manifest` 全部 green；并核验 `download.libra.tools` / stable manifest 可获取。
- **ER-05:** 每卡代码 review 按模板默认由 Codex 执行至 `PASS`；计划文本提交门（Claude 与 Codex 双审）不替代 ER-05。

- **验证执行协议（对全计划每条 `Verification` 条目强制）：**
  1. **卡特有用例**（标 `(new)`）：每条 Verification 只含一个测试，过滤式为 `-E 'test(=command::<模块>::<函数>)'` 的完整路径精确匹配（`command_test` 目标）或 `--lib -E 'test(=command::<模块>::<测试模块>::<函数>)'`（`src/command/push.rs` 的 `mod test`）；不得用前缀、正则或 `|` 并集代替。
  2. 卡特有用例运行前先以同一前缀与过滤执行 `cargo nextest list`（例：`source .env.test && cargo nextest list --test command_test -E 'test(=command::fetch_test::fetch_multiple_command_line_refspecs)'`），列出的测试必须恰为 1 个且名字与预期逐字一致，否则 STOP。
  3. 卡特有用例运行后记录 exit code 与「匹配 / 通过 / 失败 / 跳过」四计数，要求 exit code = 0、匹配 = 通过 = 1、失败 = 0、跳过 = 0，任一不满足即 STOP。共享回归组不适用第 2–3 项的「恰为 1 个」与「匹配 = 通过 = 1」，其预检与判定只按第 5 项执行。
  4. 测试实现必须对卡内「AC → 断言映射」逐条断言（exit code、stdout/stderr 的精确内容或必含/必不含片段、引用与文件状态），执行记录逐条引用 AC 编号；只断言映射所列谓词，不加入映射之外的判据。各卡映射使用下列记号（`r` 为仓库目录，裸仓库即其目录本身）：
     - `Ref(r, x)`：在 `r` 内运行 `libra rev-parse --verify <x>` 的 stdout 去尾换行；退出码非 0 记为 `absent`。
     - `RefMap(r)`：在 `r` 内运行 `libra show-ref`，把每行 `<oid> <完整引用名>` 解析为「完整引用名 → OID」映射（以 `LBR-CLI-003`「no matching refs found」失败时记为空映射，其它失败即测试失败），只去掉键为 `refs/remotes/<已配置 remote>/HEAD` 的条目——这类条目必为 `show-ref` 合成的符号远程 HEAD（`src/command/show_ref.rs:295-309`），因为 Libra 的 fetch 拒绝写入恰为 `refs/remotes/<remote>/HEAD` 的目标（`validate_fetch_destination` 要求分支段不等于 `HEAD`，`src/command/fetch.rs:1902-1910`；`refs/remotes/<remote>/x/HEAD` 这类嵌套名不受此限，但不会与合成键同名），且本计划夹具不以其它方式创建名为 `refs/remotes/<remote>/HEAD` 的真实引用；其余以 `HEAD` 结尾的真实引用（如 `refs/heads/HEAD`）保留；「新增条目」指调用后映射中调用前不存在的键值对。`show-ref` 枚举本地分支（含以 `refs/` 开头的全名分支行）、全部已配置 remote 下的远程跟踪引用与 tag（`src/command/show_ref.rs:261-321`）；测试在每次取 `RefMap(r)` 时（调用前与调用后）都断言「`r` 的 `reference` 表中 `remote` 非空的行，其 `remote` 值均为已配置 remote」，不满足即测试失败；因此被比较的每个 `RefMap` 都覆盖该仓库的全部分支与 tag 引用，挂在未配置 remote 名下的错误写入也会使测试失败。（不用 `libra for-each-ref`：其把全名分支行显示为 `refs/heads/refs/...`，属既有缺陷，见「风险登记」。）
     - `R(r)`：`r` 的仓库数据库（非裸为 `.libra/libra.db`，裸为 `<r>/libra.db`）中 `reference` 表与 `reflog` 表的全部行、全部列（含 `id`、提交者与时间戳列），各按 `id` 排序，表定义见 `sql/sqlite_20260309_init.sql:19-41`。
     - `W(r)`：工作区内除 `.libra/`、顶层 `.libra-test-home/`（第 7 项的 `run_libra_command*` 经 `base_libra_command` 在每次调用的 cwd 下创建 `.libra-test-home/.config`，并以其下 `.libra/config.db`、`.libra/system-config.db` 为全局/系统配置库路径，`tests/command/mod.rs:354-360`；它是测试基建的隔离 HOME 而非工作区内容，在某目录首次作为调用 cwd 时才出现，计入会使正确实现在该目录的首次被测调用前后 `W` 不等）与嵌套仓库目录（含 `.libra/` 或 `libra.db` 的子目录，如夹具中嵌在工作区内作为 fetch/push 对象的仓库）外全部条目的「相对路径 → (类型, 权限位, 内容)」清单，遍历与取元数据均不跟随符号链接（`symlink_metadata`）：类型为普通文件 / 符号链接 / 目录；权限位为 Unix `mode & 0o7777`（非 Unix 平台记只读标志）；内容对普通文件记 SHA-256、对符号链接记链接目标字符串、对目录记空；另加 `.libra/index` 的 SHA-256（不存在记为 `absent`）。因而只改可执行位、文件类型或链接目标而字节不变的变化也会使 `W` 不等。
     - `S(r)`：`R(r)` + `W(r)`（裸仓库无此项）+ `config_kv` 表全部行、全部列（按 `id` 排序，表定义 `sql/sqlite_20260309_init.sql:12-17`）+ 对象目录（非裸 `.libra/objects/`，裸 `<r>/objects/`）下全部文件（含 `*.lock`）的「相对路径 → SHA-256」清单（pack 安装/保留锁 `pack-*.install.lock`、`pack-*.keep.lock` 只由安装 pack 的写入方以 `flock` 创建并常驻，读取方只打开已存在的锁文件而不创建，`src/internal/pack_writer.rs:410-413`、`src/command/fetch.rs:3837,3858`、`src/utils/storage/local.rs:741-748`；因此无写入的调用前后锁文件集合不变，写入后的锁文件残留也会使 `S` 不等） + `FETCH_HEAD`（非裸 `.libra/FETCH_HEAD`，裸 `<r>/FETCH_HEAD`，即 `fetch_head_path()` 解析的调用方 gitdir 下文件，`src/command/fetch.rs:4457-4470`）的字节（不存在记为 `absent`）。`S(r)` 作为一份整体快照逐字比较，计为一个谓词。
     「不变」类 AC 一律以调用前后对应快照逐字相等判定，不以计数或抽样代替。夹具描述中标为「前置断言」的条件由测试在调用前断言，失败即测试失败（不计 AC）。夹具建立步骤（各仓库内的 `init`/提交/切换/配置）与 `Ref`/`RefMap`/快照读取一律在对应仓库目录内执行；卡内「以 `cwd=…` 运行」只约束被测命令调用。映射中标注「Git 同夹具期望」的值来自卡内 `Current evidence` 引用的钉选上游测试断言或源码，开工时（ER-02）须用 `git@6de20f6092` 构建的 Git 在同构夹具上重新取得并硬编码进测试；与本计划不一致时以钉选版本为准，按 ER-03 回写本卡（超出本卡行为轴时拆卡）。未标注的值为 Libra 自身契约（稳定码、`--json`、引用存储与输出约定），不向 Git 取值。
  5. **共享回归组**（标「共享回归组」）：可为整个 compat target 或 `test(/^…/)` 正则族；运行前以同一过滤 `cargo nextest list` 记录匹配数 N（必须 ≥ 1），运行后要求 exit code = 0、通过 = N、失败 = 0、跳过 = 0。共享回归组是按本卡改动表面选出的 ER-04 A 组 focused 回归命令，必须运行并记录，但只作回归证据：不计为卡特有 AC 证据，也不计入 G-03 `VER` 分子。本卡行为改变使既有用例断言失效时，只在同卡内修改被取代的断言、保留其余断言，并在本卡 `Current evidence` 记录被改断言与理由。
  6. **VER 计数口径（G-03）**：每条卡特有用例 Verification 含两个独立门——第 2 项的 `cargo nextest list` 预检与第 3 项的运行判定，因此各卡 `VER` 分子按 2 × 卡特有用例条目数计；共享回归组不计（第 5 项）。**AC 计数口径（G-03）**：一条 AC 只承载一个可独立失败的可观测谓词；对同一份状态（一个 `RefMap`、一个快照、一个输出流的全文、一个引用集合）作一次整体等值比较计为一个谓词；不同观测对象（如 pack 数与输出文本）、不同行为、不同选项、不同命令、不同错误条件一律分条计数，不得以元组合并。**映射口径**：AC 文本与其映射表述同一个谓词，映射只给出该谓词的精确判定值，不得加入 AC 文本之外的其它行为判据。成功类映射中的 `exit 0` 是读取输出与状态的前提。失败类 AC 一律写成「以 `<LBR-*>` 失败」，这是一个谓词（命令以该稳定码的错误结果结束），其映射判定为「退出码非 0 且 stderr 含该稳定码」——二者是同一 Libra 错误结果的两个读取面，计为一个谓词；不断言具体数值退出码——数值退出码通常由稳定码派生（`docs/error-codes.md:98-115`），但 `LIBRA_FINE_EXIT_CODES` 等可使其偏离；诊断文本中嵌入的退出码（如 `FP-11` 的 `(exit code: 128)`）在默认退出码配置下断言，夹具不设置 `LIBRA_FINE_EXIT_CODES`。
  7. **新增用例的进程级隔离**：本计划新增的卡特有用例（单元测试除外）一律经 `tests/command/mod.rs` 既有的 `run_libra_command` / `run_libra_command_with_env`（`tests/command/mod.rs:385,392`：清空并重设环境、隔离 HOME、`LIBRA_CONFIG_GLOBAL_DB`、`LIBRA_CONFIG_SYSTEM_DB`）以子进程调用 `libra`，通过显式工作目录与逐子进程环境变量（如 `GIT_TRACE`、提交者身份）传入设置，不修改测试进程的 cwd/环境，因而不使用 `#[serial(...)]`，不触及 `tests/command/mod.rs`、跨计划共享的 `tests/SERIAL_REGISTRY.tsv` 与生成的 `.config/nextest.toml`（不命中 ER-13 `T-4`）；若实现中确需 `#[serial]` 或修改共享测试基建，须先按 ER-03 把这些文件加入本卡 `Implementation write set`、补对应共享回归门并重新核验 `DEP-FP-01`，再开工。本计划的测试门（本地三门与 `base.yml` 的 `test` job）在 Unix 上执行。

- **文档与网站验收命令（ER-06/ER-06a 同卡强制门；各卡「文档 AC」引用；按序执行，任一步失败即 STOP 且本卡不得进入 `complete`）：**
  1. **仓库文档逐文件验收：** 对本卡 `Docs and compatibility impact` 所列每个仓库文件记录改动前后 `shasum -a 256 <file>`；以 `test -f docs/commands/<cmd>.md && test -f docs/commands/zh-CN/<cmd>.md` 确认 EN/zh 双份存在；对卡内「文档关键字」只在 EN/zh 两个命令页执行 `rg -n -F -- '<关键字>' <file>`（退出码 0 = 命中；1 = 缺失 → STOP；>1 = 命令失败 → STOP），这两页改动后 SHA-256 必须与改动前不同；`docs/development/commands/<cmd>.md` 与 `COMPATIBILITY.md` 对应行逐文件做语义核对，结论为「已更新」（记录改动前后 SHA-256 与所改段落）或「无需修改」（记录理由与未变 SHA-256），不得为通过机械门而添加不合该文件粒度的内容；`COMPATIBILITY.md` 先以下方命令块 W-1 刷新目标行号（必须恰好输出一个行号），有改动时断言改动前后 `wc -l < COMPATIBILITY.md` 相等（原地修改）。本计划每张卡都改变用户可见行为，因此每张卡都有文档关键字，EN/zh 命令页与网站页必须同卡实际更新（ER-06a），不得以「无需修改」通过。
  2. **网站检出预检：** 前置：`DEP-FP-08`（用户 2026-11-18 11:55:09 UTC 确认，见「文档职责」）仍有效、未被用户撤回或改变，否则 STOP。按序执行：(a) `test -d /Volumes/Data/gitmono/libra-backend/.libra && test ! -e /Volumes/Data/gitmono/libra-backend/.git`，不满足即 STOP；(b) `cd /Volumes/Data/gitmono/libra-backend && libra --offline symbolic-ref --short HEAD`，输出不是 `cf` 时按 ER-06a 执行 `libra switch cf`（被脏工作区等阻止即 STOP）后重跑 (b)；(c) 此后 `libra --offline symbolic-ref --short HEAD` 输出须恰为 `cf`，且 `libra status --short --branch` 首行须以 `## cf...origin/cf` 开头，否则 STOP；(d) 本卡目标页面 `apps/tanstack-app/content/docs/commands/<cmd>.en.md` 若已有未提交改动（`libra status --short -- <页面路径>` 输出非空）即 STOP；随后执行 `libra fetch origin`，要求 `libra rev-parse HEAD` 等于 `libra rev-parse refs/remotes/origin/cf`（本地 `cf` 不含未推送提交）——本地落后时只允许 `libra pull --ff-only origin cf` 快进后重验，领先或分叉即 STOP；`libra status --porcelain` 中不得有任何已暂存条目（首列既非空格也非 `?`），否则 STOP。已有的无关改动保留、不暂存。
  3. **同步与漂移检查：** 以包含本卡最终 EN 参考文档的 checkout 为 `--libra-root`（默认 `/Volumes/Data/gitmono/libra`；执行前以第 1 步记录的 SHA-256 确认其中 `docs/commands/<cmd>.md` 已是本卡合入 `main` 的字节），执行 `cd /Volumes/Data/gitmono/libra-backend && python3 scripts/sync-libra-command-docs.py --libra-root /Volumes/Data/gitmono/libra --write --only <cmd…>`（多个命令以空格分隔，如 `--only fetch gc`），随后 `cd /Volumes/Data/gitmono/libra-backend && python3 scripts/sync-libra-command-docs.py --libra-root /Volumes/Data/gitmono/libra --only <cmd…>` 必须 exit 0（1 = 漂移、2 = 安全拒绝，均 STOP）；记录每个 `apps/tanstack-app/content/docs/commands/<cmd>.en.md` 的改动前后 SHA-256。本计划每张卡都已在第 1 步改变 EN 命令页，同步后网站页 SHA-256 必须与改动前不同；若未变即 STOP 并排查，不设「网站无变化」通过分支。每张卡都必须完成第 4–7 步。
  4. **前端校验：** `cd /Volumes/Data/gitmono/libra-backend && pnpm typecheck` 与 `pnpm build` 分别记录退出码，均须为 0。
  5. **网站评审：** 对第 3 步记录的最终页面字节取得独立 Claude Code `VERDICT: PASS`；评审后页面字节变化须重审。
  6. **精确提交与推送：** 只 `libra add apps/tanstack-app/content/docs/commands/<cmd>.en.md`（本卡页面；多命令卡逐页添加）→ `libra status --porcelain` 的已暂存条目必须恰为本卡页面集合，否则 STOP → `libra commit -s -m "docs(commands): sync <cmd> for <卡 ID>"` → 按 ER-07 对同一提交校验 `gpgsig` 与 `Signed-off-by` → `libra show <提交>:<页面路径> | shasum -a 256` 等于第 5 步受审字节 → 推送前再次 `libra fetch origin`，要求 `libra rev-parse <提交>^` 等于 `libra rev-parse refs/remotes/origin/cf`（本次推送只新增本提交）→ `libra push origin cf`，推送后 `libra ls-remote origin refs/heads/cf` 指向该提交。远端已前进（上述比较不等，或推送因非快进被拒）时不推送、不 force、不改写他人提交，按以下恢复后从第 2 步重新开始：`libra reset --mixed <提交>^`（只撤销本卡这一个未推送的提交，工作区与他人改动保留）→ `libra restore --staged --worktree -- <页面路径>`（只丢弃本卡生成的页面字节，第 3 步会重新生成）→ `libra pull --ff-only origin cf`；重新执行后第 3 步生成的页面字节与第 5 步受审字节不同时，须重新取得第 5 步 `PASS`。
  7. **部署核验（`<article>` HTML 逐字节比对）：** 基准构建前先过干净性门：`cd /Volumes/Data/gitmono/libra-backend && libra status --short --ignored --untracked-files=all` 的输出中，除后端根目录 `docs/` 下的条目（不参与站点构建；站点内容根为 `apps/tanstack-app/content/docs`，见 `scripts/gen-docs-manifest.ts:83`）与下列构建产物/依赖/缓存的忽略条目外不得有任何条目：`node_modules/`、`apps/tanstack-app/node_modules/`、`apps/tanstack-app/dist/`、`apps/tanstack-app/.wrangler/`、`apps/tanstack-app/src/generated/docs-manifest.ts`、`apps/tanstack-app/src/generated/docs-search.ts`（二者由构建从内容目录重新生成）、`scripts/__pycache__/`、`test-results/`，以及 `apps/tanstack-app/.dev.vars`（`wrangler dev` 的本地运行时变量；文档路由 `apps/tanstack-app/src/routes/$lang/(root)/docs.$.tsx` 只读取构建期文档清单，正文不依赖运行时变量）；特别地，后端根目录不得存在任何 `.env*` 文件（`apps/tanstack-app/vite.config.ts` 以仓库根为 `envDir`）；并要求 `libra rev-parse HEAD` 等于 `libra rev-parse refs/remotes/origin/cf`。任一不满足即 STOP（不清理、不暂存他人改动）。随后以 `pnpm preview:cf`（按 `apps/tanstack-app/package.json:8-9` 先构建工作区——过门后等同已验证的 `cf` 提交字节——再以 `wrangler dev --port 7001` 本地运行）作基准，对本卡每个命令的本地 `http://localhost:7001/en/docs/commands/<cmd>` 与生产 `https://libra.tools/en/docs/commands/<cmd>` 分别执行下方命令块 W-2：它在内存中取得同一次响应，要求 HTTP 状态为 200、`<article>` 存在且其文本非空，输出 `<article>` 内部 HTML 原始字节的 SHA-256 与字节数（不做任何规范化）；退出码非 0（1 = 请求或 HTTP 错误、2 = 缺 `<article>` 或文本为空、3 = 状态非 200）即 STOP。两份 SHA-256 必须相等，即生产页 `<article>` 的渲染 HTML 与已验证 `cf` 字节的本地构建逐字节一致；不相等即 STOP（不得改用规范化文本比较放行）。只把两份哈希、字节数与观察 UTC 时间写入本计划，不落盘任何响应内容；用后停止本地预览进程。部署传播延迟按 ER-10 暂时性错误有界重试（≤ 5 次、≤ 30 分钟），超预算置 `blocked`。
  8. **时序：** 第 1 步与第 2 步（含 `DEP-FP-08` 前置与目标页面干净检查）在本卡 C 组第 ⑦ 步签名提交之前完成，任一失败则本卡不得提交、合并或发布（`Lifecycle` 置 `blocked`，不进入 `remote-pending`）；第 3–7 步在本卡 Libra PR squash merge（C 组第 ⑧ 步）与 `gh release create`（第 ⑨ 步）之后执行，使网站不先于发布公开新行为（第 3–7 步开始前重跑第 2 步，并确认仍持有 `DEP-FP-01` 中本卡网站页的写锁）；第 3–7 步未完成前本卡停留在 `remote-pending`。

上列第 1、7 步引用的命令块（`<cmd>` 替换为本卡命令；`<URL>` 为第 7 步的本地或生产页面地址）：

```bash
# W-1：定位 COMPATIBILITY.md 中 <cmd> 的状态矩阵行；必须恰好输出一个行号（基线 push=222、fetch=223、gc=267）
awk -F'|' -v c=" <cmd> " '$2==c && $3 ~ /^ (supported|partial|unsupported|intentionally-different) $/ {print NR}' COMPATIBILITY.md

# W-2：在内存中取得 <URL> 的同一次响应，输出 <article> 内部 HTML 原始字节（不做规范化）的「SHA-256 字节数」；退出码须为 0，不落盘响应
python3 -I - "<URL>" <<'PY'
import hashlib, html, re, sys, urllib.request
req = urllib.request.Request(sys.argv[1], headers={"User-Agent": "curl/8.7.1"})
with urllib.request.urlopen(req, timeout=30) as r:
    if r.status != 200:
        sys.exit(3)
    s = r.read().decode("utf-8")
m = re.search(r"<article>(.*?)</article>", s, re.S)
if not m:
    sys.exit(2)
a = m.group(1)
if not " ".join(html.unescape(re.sub(r"<[^>]+>", " ", a)).split()):
    sys.exit(2)
b = a.encode("utf-8")
print(hashlib.sha256(b).hexdigest(), len(b))
PY
```

## 实施顺序

- `DEP-FP-01 -> FP-01..FP-25`（每张卡开工前重新核验本卡完整 `Implementation write set` 的跨计划写锁）
- `DEP-FP-08 -> FP-01..FP-25`（门控各卡「文档与网站验收命令」：已由用户 2026-11-18 11:55:09 UTC 确认，各卡第 2 步复核该确认仍有效）
- `DEP-FP-02 -> FP-01`；`DEP-FP-04 -> FP-10, FP-19, FP-11`；`DEP-FP-05 -> FP-07, FP-15`
- Phase 1: `FP-01 -> FP-23 -> FP-02 -> FP-04 -> FP-03 -> FP-05 -> FP-06 -> FP-21 -> FP-25`
- Phase 2: `FP-25 -> FP-07 -> FP-08 -> FP-09 -> FP-10 -> FP-19 -> FP-11`
- Phase 3: `FP-11 -> FP-12 -> FP-13 -> FP-14 -> FP-15 -> FP-16 -> FP-17 -> FP-24 -> FP-18`

（`FP-01..FP-25` 指本计划全部 23 张现行卡，即 `FP-01`..`FP-19`、`FP-21`、`FP-23`、`FP-24`、`FP-25`。按模板 `plan-template.md:460` 与 G-09，拆出的新卡（`FP-19` 自 `FP-10`、`FP-21` 自 `FP-06`、`FP-23` 自 `FP-01`、`FP-24` 自 `FP-18`、`FP-25` 自 `FP-21`）在所属 Phase 末尾追加编号与卡片位置，既有编号不重排；编号与卡片位置顺序 ≠ 执行顺序，执行以上列依赖边与各卡 `Dependencies` 为准。）

### 依赖登记表

| ID | direction | 类型 | 对象 | Owner | 产物与可用性判据 | 证据 | 超时与失败策略 |
|---|---|---|---|---|---|---|---|
| DEP-FP-01 | incoming | 跨计划全写集互斥锁 | `plan-status.md` 登记的全部在途写者（含 `plan-20261117-merge-pull.md`、`plan-20261117-branch.md#BR-24`、`plan-20261117-clone.md`、`plan-20261117-worktree-clean-reset-apply.md` 的 `DEP-WC-07` 所列 `src/cli.rs` 写者及共用 `COMPATIBILITY.md` 的同批计划） | 协调者 | **每张 `FP-*` 卡开工前**重新核实 `plan-status.md` 中该卡**完整 `Implementation write set`**（`src/command/fetch.rs`、`src/command/push.rs`、`src/internal/protocol/{repository_arg,local_push}.rs`、`src/command/gc.rs`、`src/cli.rs` 中本卡所列者，`tests/command/{fetch,push,gc}_test.rs`、对应命令文档、`COMPATIBILITY.md` 对应行与网站页）均无 `in-progress`/`blocked` 写者，取得独占写锁；其中仓库内文件的写锁持有至本卡 Libra PR squash merge（C 组第 ⑧ 步）后释放，网站页的写锁持有至本卡「文档与网站验收命令」第 7 步完成后释放（本卡因第 3–7 步未完成而停留 `remote-pending` 期间继续持有；第 3–7 步 STOP 时不自行释放，由协调者裁决） | `docs/development/plan/plan-status.md` | fail-closed：有并发写者或归属不清即不开工，等待释放并 rebase 后再核验 |
| DEP-FP-02 | incoming | 跨计划协调 | `plan-20261117-merge-pull.md`（`pull.rs` 消费 `repository_arg.rs`） | 协调者 | `FP-01` 只新增 opt-in helper，`is_anonymous_repository_spec` 与 `pull` 行为不变；`repository_arg.rs` 写入按 `DEP-FP-01` 串行 | `docs/development/plan/plan-20261117-merge-pull.md`；`src/internal/protocol/repository_arg.rs:17-31` | 冲突时由协调者排序，`FP-01` 等待 |
| DEP-FP-03 | outgoing | 外部验收移交 | `plan-20261117-checkout-switch-restore.md`（`#676`，`#719` 验收 3 的 `checkout -b new origin`） | `plan-20261117-checkout-switch-restore.md` | `FP-01`、`FP-23` 只交付 fetch 侧（`refs/heads/origin` 写入）；未出生分支检出由接收方闭环 | issue `#719`、`#676` | 不阻塞本计划；接收方未接手时 `#719` 保持 OPEN（「成功定义」关闭口径） |
| DEP-FP-04 | incoming | 跨计划协调 | `plan-20261117-branch.md#BR-24`（`#866` `skipFetchAll`） | 协调者 | `fetch.rs` 的 `--all` 循环、`fetch_test.rs` 与 `COMPATIBILITY.md` 按 ER-12 串行合入 | `docs/development/plan/plan-20261117-branch.md` | `FP-10`、`FP-19`、`FP-11` 与 `BR-24` 不并发，按协调者顺序 |
| DEP-FP-05 | incoming | 跨计划协调 | `plan-20261117-clone.md`（其 `DEP-CL-04`、`DEP-CL-05`、`DEFER-CL-03` 以 `FP-07`、`FP-15` 为前置） | 协调者 | 共享 `src/command/fetch.rs` 与 `COMPATIBILITY.md` 串行合入；`FP-15` 完成后 clone 计划可据此闭环 `#876` 端到端（由其 `DEFER-CL-03` 承接，本计划不声称已闭环） | `docs/development/plan/plan-20261117-clone.md` | 禁止并发写 `fetch.rs`；按协调者顺序 |
| DEP-FP-06 | outgoing | 外部 harness 证据移交 | `../libra-testcases`（Git 仓库）`cases/<tc>/<tc>.sh` 整文件运行，含 `TC-1338`（`cases/tc1338-fetch-verbose/tc1338-fetch-verbose.sh`，映射 `t/t5510-fetch.sh:1423` `fetching with auto-gc does not lock up`）及各 issue 引用的其它 `TC-*` | 外部测试语料维护者 | 各 harness 依次运行整份上游测试文件，结果取决于同文件全部前置用例；本计划各卡以仓内同构用例交付行为，harness 结果只作 issue 关闭的补充证据，不作任何卡的门 | `../libra-testcases/cases/tc1338-fetch-verbose/tc1338-fetch-verbose.sh:1-19` | 不阻塞任何卡；harness 失败归因到本计划卡行为轴的，按 ER-10 新建 `FIX-*` 前滚，归因于其它用例的不处理并在 issue 中说明 |
| DEP-FP-07 | outgoing | 外部上游前置移交 | 后续 `branch --delete-merged` 计划（`TC-0180` / `t/t3200-branch.sh:2006`） | CLI / Git-compat 维护者 | `FP-03` 交付 `push` 侧 `remote.<name>.push`；整序列依赖本批未立项的 `branch --delete-merged`（对照 `plan-20261117-branch.md#DEFER-BR-02`） | `#683`；`../libra-testcases/cases/tc0164-0189-branch-upstream/tc0164-0189-branch-upstream.sh` | 不阻塞 `FP-03` 与本计划收口；见 `DEFER-FP-02` |
| DEP-FP-08 | incoming | 审批（已满足） | 用户对网站文档交付检出路径的确认 | 用户 | **已满足**：用户 2026-11-18 11:55:09 UTC 确认本批计划的网站文档交付检出为 `/Volumes/Data/gitmono/libra-backend`（`cf` 生产分支）；对全部卡持续有效，各卡第 2 步只复核该确认未被撤回或改变 | 用户 2026-11-18 11:55:09 UTC 确认（记录于本计划「文档职责」）；`AGENTS.md:135`（`/Volumes/Sea/libra-backend`，本机不存在，已知文档滞后，本批不改）；`plan-template.md:332`（ER-06a 同级默认） | 确认已取得，无需等待；若用户撤回或改变该确认，各卡网站步骤即 STOP——尚未到 C 组第 ⑦ 步签名提交的卡不提交、不合并、不发布并置 `Lifecycle=blocked`，已完成 C 组的卡在网站第 3–7 步 STOP 并停留 `remote-pending`；按用户新指定的路径修订本计划全部网站路径并经评审后再执行 |

### 发布分组与并发窗口

本计划无 `REL-*` 合并发布分组：每张卡均为 `independent` 发布切片（G-07）。

- **并发声明:** 全串行（共享 `src/command/fetch.rs`、`src/command/push.rs`、`COMPATIBILITY.md` 与 `docs/commands/` 写集，依据 G-10 不并发；每张卡开工前均须重新核验 `DEP-FP-01`）。
- **发布者:** 单一指定发布者（协调者指派的执行 Agent），按 ER-12 串行执行 C 组与 D 组跟踪。
- **发布窗口顺序:** 按 `FP-01` → `FP-23` → `FP-02` → `FP-04` → `FP-03` → `FP-05` → `FP-06` → `FP-21` → `FP-25` → `FP-07` → `FP-08` → `FP-09` → `FP-10` → `FP-19` → `FP-11` → `FP-12` → `FP-13` → `FP-14` → `FP-15` → `FP-16` → `FP-17` → `FP-24` → `FP-18` 依次执行 C 组 bump 与 `gh release create`；同一时刻只允许一张卡处于「已 bump 未完成推送 / 未触发 release」状态。

### Phase 0: 基线冻结与锚点核验

**目标:** 执行 ER-01/ER-02 预检，刷新全部源码锚点，按「验证执行协议」第 4 项用 `git@6de20f6092` 构建的 Git 复核各卡「Git 同夹具期望」，核实 `DEP-FP-01`、`DEP-FP-08`。

**进入条件:** 本计划文本经 Claude（Subagent）与 Codex 对同一字节双审 `VERDICT: PASS` 并入库。

**退出条件:** 锚点与 Git 期望核对完成（不一致者已按 ER-03 回写对应卡），进入 Phase 1。

### Phase 1: 本地路径与 Push 兼容性（`FP-01`、`FP-23`、`FP-02`、`FP-04`、`FP-03`、`FP-05`、`FP-06`、`FP-21`、`FP-25`）

**目标:** 裸目录名识别与短目标补全、`push` 的 `--prune`/通配符/负向 refspec 与 `remote.<name>.push`、`receive.denyCurrentBranch`、`--force` 与租约语义。

**进入条件:** Phase 0 退出；`DEP-FP-02` 核实。

**退出条件:** `FP-01`、`FP-23`、`FP-02`、`FP-04`、`FP-03`、`FP-05`、`FP-06`、`FP-21`、`FP-25` 全部 `done` / `complete`。

### Phase 2: Fetch 默认 remote、多 refspec 与多远程（`FP-07`、`FP-08`、`FP-09`、`FP-10`、`FP-19`、`FP-11`）

**目标:** 无参 fetch 回落 `origin`、多 refspec、非常规目标命名空间、`--multiple`/`--jobs`/`fetch.all`/`--no-all` 与多远程失败继续。

**进入条件:** `FP-25` 完成。

**退出条件:** `FP-07`、`FP-08`、`FP-09`、`FP-10`、`FP-19`、`FP-11` 全部 `done` / `complete`。

### Phase 3: Fetch 输出、Prune、Tag 与自动打包（`FP-12`、`FP-13`、`FP-14`、`FP-15`、`FP-16`、`FP-17`、`FP-24`、`FP-18`）

**目标:** `--[no-]write-fetch-head`、人读报告输出流与格式、`--no-porcelain`、`--prune` 作用域、`--tags` 冲突拒绝、`tagOpt` 与 fetch 后自动打包。

**进入条件:** `FP-11` 完成。

**退出条件:** `FP-12`、`FP-13`、`FP-14`、`FP-15`、`FP-16`、`FP-17`、`FP-24`、`FP-18` 全部 `done` / `complete`，且「完成判据」全量收口门通过。

## 任务卡

### 任务卡粒度规则（强制）

继承 [plan-template.md](plan-template.md) 的 `G-01`..`G-11`。

### 字段全局默认与例外

- **Release boundary 默认:** `independent`（每卡独立执行完整 C 组含 `patch + 1`、`compat_version_surface_sync` 同步与 `gh release create`，并适用 ER-04 D 组：`base.yml`、`codeql.yml`、`release.yml`）
- **Task type 默认:** `implementation`
- **Rollback mode 默认:** `immutable-release`（推送/发布前：本地 `revert` 单提交；推送/发布后：已发布版本与 tag 不可撤回，按 ER-04/ER-10 新建 `FIX-*` 前滚修复并发新 `patch + 1` 版本；兼容窗口与降级指引：同 `0.30.x` 次版本内无 schema 变更，可回退安装上一版本 `v<prev>`）
- **Migration and rollback 默认:** `N/A：无 schema 迁移`
- **Security and privacy 默认:** 继承 GC-07、GC-11（路径与 refname 校验 fail-closed、无裸 `unwrap()`）
- **Performance budget 默认:** 继承 GC-10（无无界扫描；多远程 fetch 串行执行）
- **Release write set 默认:** `Inherited`
- **Full-suite trigger 默认:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）`（`FP-01`、`FP-13`、`FP-24`、`FP-18` 另附本卡依据）
- **Deliverables 默认:** `N/A`

**默认覆盖:** 无。

**规则 waiver（`EX-*`）:** 无。原 `EX-FP-07-PRUNE`（2026-11-17 登记）随 `FP-07` 收窄为默认 remote 回落轴而撤销（见「修订历史」2026-11-18 行）。

### 废弃编号

以下编号保留不复用，所列范围不在本计划实施：

| 编号 | 原内容 | 处置 | 承接 |
|---|---|---|---|
| `FP-15a`、`FP-15c`、`FP-15b`、`FP-15R`（原 `REL-FP-PRUNE-TAGS` 家族） | `--prune-tags` 与有效 prune 的组合、tag refspec 获取与远端组合、`pruneTags` remote/global 优先级与 `--no-prune-tags` CLI 顺序，及其联合发布点 | 2026-11-18 废弃：不属于 18 个 issue 的验收条目（`#872` 的上游用例 `t5510:379-463` 只涉 `--prune` 作用域，由 `FP-15` 交付）；既有 `--prune-tags` 实现保持不变并由 `test_fetch_prune_tags_matrix` 回归 | `DEFER-FP-03` |
| `FP-20` | `fetch.unpackLimit` 小包解包与 `TC-1338` 闭环 | 2026-11-18 废弃：`#863` 上游场景设 `fetch.unpackLimit 1`，无需解包路径（ADR-FP-03）；`TC-1338` 改为 `DEP-FP-06` 补充证据，自动打包由 `FP-24`（`gc --auto` 判定）与 `FP-18`（fetch 后触发）交付 | `DEFER-FP-04`、`FP-24`、`FP-18` |
| `FP-22` | `TC-0180` 测试拆卡 | 2026-11-17 05:10:00 UTC 已移除（G-02） | `DEFER-FP-02` |

### 任务卡粒度审计表

> `landing / prod-files` 仅计入生产代码落点与生产文件（不含随附测试与文档，符合 G-04）；`src/cli.rs` 按单个文件落点计。

| 任务 | type | axis | recovery | complete | self-contained | AC | VER | landing / prod-files | scope | deps | writeset | release | split-from | exception |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| FP-01 | implementation | fetch裸本地仓库目录名识别 | immutable-release | yes | yes | 8/8 | 2/8 | 2/2 | M | DEP-FP-01,DEP-FP-02,DEP-FP-08 | 序列化于 DEP-FP-01 | independent | N/A | N/A |
| FP-02 | implementation | push裸本地仓库目录名识别 | immutable-release | yes | yes | 6/8 | 2/8 | 1/1 | M | FP-23,DEP-FP-01,DEP-FP-08 | 序列化于 FP-23 | independent | N/A | N/A |
| FP-03 | implementation | push的remote.*.push配置refspec | immutable-release | yes | yes | 7/8 | 2/8 | 1/1 | M | FP-04,DEP-FP-01,DEP-FP-08 | 序列化于 FP-04 | independent | N/A | N/A |
| FP-04 | implementation | push命令行refspec通配符/负向与--prune | immutable-release | yes | yes | 7/8 | 2/8 | 1/1 | M | FP-02,DEP-FP-01,DEP-FP-08 | 序列化于 FP-02 | independent | N/A | N/A |
| FP-05 | implementation | 本地push的receive.denyCurrentBranch | immutable-release | yes | yes | 8/8 | 2/8 | 2/2 | M | FP-03,DEP-FP-01,DEP-FP-08 | 序列化于 FP-03 | independent | N/A | N/A |
| FP-06 | implementation | push--force覆盖--force-with-lease | immutable-release | yes | yes | 7/8 | 4/8 | 1/1 | M | FP-05,DEP-FP-01,DEP-FP-08 | 序列化于 FP-05 | independent | N/A | N/A |
| FP-21 | implementation | push非空期望与跟踪租约判定 | immutable-release | yes | yes | 8/8 | 2/8 | 1/1 | M | FP-06,DEP-FP-01,DEP-FP-08 | 序列化于 FP-06 | independent | FP-06 | N/A |
| FP-23 | implementation | 命令行refspec目标补全与匿名显式目标写入 | immutable-release | yes | yes | 7/8 | 2/8 | 1/1 | M | FP-01,DEP-FP-01,DEP-FP-08 | 序列化于 FP-01 | independent | FP-01 | N/A |
| FP-25 | implementation | push空期望租约 | immutable-release | yes | yes | 3/8 | 2/8 | 1/1 | M | FP-21,DEP-FP-01,DEP-FP-08 | 序列化于 FP-21 | independent | FP-21 | N/A |
| FP-07 | implementation | 无参fetch的默认remote选择 | immutable-release | yes | yes | 8/8 | 2/8 | 1/1 | M | FP-25,DEP-FP-01,DEP-FP-05,DEP-FP-08 | 序列化于 FP-25 | independent | N/A | N/A |
| FP-08 | implementation | fetch多个命令行refspec | immutable-release | yes | yes | 8/8 | 2/8 | 1/1 | M | FP-07,DEP-FP-01,DEP-FP-08 | 序列化于 FP-07 | independent | N/A | N/A |
| FP-09 | implementation | fetch目标命名空间放宽 | immutable-release | yes | yes | 8/8 | 2/8 | 1/1 | M | FP-08,DEP-FP-01,DEP-FP-08 | 序列化于 FP-08 | independent | N/A | N/A |
| FP-10 | implementation | fetch--multiple与--jobs | immutable-release | yes | yes | 8/8 | 2/8 | 1/1 | M | FP-09,DEP-FP-01,DEP-FP-04,DEP-FP-08 | 序列化于 FP-09 | independent | N/A | N/A |
| FP-11 | implementation | 多远程fetch的逐远程失败继续 | immutable-release | yes | yes | 8/8 | 2/8 | 1/1 | M | FP-19,DEP-FP-01,DEP-FP-04,DEP-FP-08 | 序列化于 FP-19 | independent | N/A | N/A |
| FP-19 | implementation | fetch.all配置与--no-all | immutable-release | yes | yes | 8/8 | 2/8 | 1/1 | M | FP-10,DEP-FP-01,DEP-FP-04,DEP-FP-08 | 序列化于 FP-10 | independent | FP-10 | N/A |
| FP-12 | implementation | fetch的FETCH_HEAD写入开关 | immutable-release | yes | yes | 8/8 | 2/8 | 1/1 | M | FP-11,DEP-FP-01,DEP-FP-08 | 序列化于 FP-11 | independent | N/A | N/A |
| FP-13 | implementation | fetch人读更新报告 | immutable-release | yes | yes | 8/8 | 2/8 | 1/1 | M | FP-12,DEP-FP-01,DEP-FP-08 | 序列化于 FP-12 | independent | N/A | N/A |
| FP-14 | implementation | fetch的--no-porcelain | immutable-release | yes | yes | 5/8 | 2/8 | 1/1 | M | FP-13,DEP-FP-01,DEP-FP-08 | 序列化于 FP-13 | independent | N/A | N/A |
| FP-15 | implementation | fetch--prune的显式refspec作用域 | immutable-release | yes | yes | 5/8 | 2/8 | 1/1 | M | FP-14,DEP-FP-01,DEP-FP-05,DEP-FP-08 | 序列化于 FP-14 | independent | N/A | N/A |
| FP-16 | implementation | fetch--tags冲突tag拒绝 | immutable-release | yes | yes | 7/8 | 2/8 | 1/1 | M | FP-15,DEP-FP-01,DEP-FP-08 | 序列化于 FP-15 | independent | N/A | N/A |
| FP-17 | implementation | fetch的remote.*.tagOpt读取 | immutable-release | yes | yes | 6/8 | 2/8 | 1/1 | M | FP-16,DEP-FP-01,DEP-FP-08 | 序列化于 FP-16 | independent | N/A | N/A |
| FP-18 | implementation | fetch后的auto-gc触发 | immutable-release | yes | yes | 8/8 | 2/8 | 2/2 | M | FP-24,DEP-FP-01,DEP-FP-08 | 序列化于 FP-24 | independent | N/A | N/A |
| FP-24 | implementation | gc--auto的pack数阈值判定 | immutable-release | yes | yes | 8/8 | 2/8 | 1/1 | M | FP-17,DEP-FP-01,DEP-FP-08 | 序列化于 FP-17 | independent | FP-18 | N/A |

---

### Task FP-01: 修复 `libra fetch` 把裸本地仓库目录名当作 remote 名（#719；拆出 FP-23）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `complete`（2026-11-21：`repository_arg` 新增 opt-in `is_anonymous_repository_spec_with_bare_dir`（单组件裸目录名：先看当前目录的 `.libra`，再看父目录的同级仓库）与 `resolve_anonymous_local_spec`（命中后把 spec 解析成可打开的路径）；仅在 `fetch` 接入——已配置的同名 remote 优先，非 Libra 目录仍报 `remote '<name>' not found`（`LBR-CLI-003`）；`pull` 保持原解析（ADR-FP-01）。用例 `test_fetch_bare_directory_name_resolves_as_anonymous` 覆盖 AC1–AC3 及负例：同级 `parent` 仓库的 `main` 写入 `FETCH_HEAD` 首条、`show-ref` 与 `config list` 逐字不变、未知名字仍拒绝。`fetch`/`pull`/`ls_remote` 套件 **129/129**；版本面 0.30.239 → 0.30.240；文档 EN/zh-CN 同步。按用户 2026-11-20 指令在 `main` 直接提交推送。）

**Description:** 按 ADR-FP-01 在 `src/internal/protocol/repository_arg.rs` 新增 opt-in helper `is_anonymous_repository_spec_with_bare_dir`，并只在 `src/command/fetch.rs` 接入：`<repository>` 既不是已配置 remote、又不满足现有 `is_anonymous_repository_spec`，但相对当前目录解析为一个存在的 Libra 仓库目录（含裸仓库）时，按现有匿名本地仓库路径 fetch（与 `./<dir>` 一致：不创建远程跟踪引用、不写 `remote.*` 配置）。已配置的同名 remote 仍优先；既非配置 remote 也非 Libra 仓库目录时保持 `remote '<name>' not found`（`LBR-CLI-003`）。按 G-09 自本卡拆出 `FP-23`（命令行 refspec 的短目标补全与匿名 fetch 写显式目标）。

**Out of scope:** 命令行 refspec 短目标补全与匿名 fetch 写入显式目标引用（由 `FP-23` 承接）；`libra push` 的裸目录名（由 `FP-02` 承接）；`pull` 的仓库参数解析（保持不变，ADR-FP-01；`DEP-FP-02` 协调）；`#719` 验收 3–4 的 `checkout -b new origin`（未出生分支检出，`DEP-FP-03` 移交 `checkout` 计划）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `is_anonymous_repository_spec` 只识别 SSH/URL、`.`/`..`、绝对路径与 `./`、`../` 前缀，裸目录名 `parent` 不命中 | `src/internal/protocol/repository_arg.rs:17-31` |
| `run_fetch` 在未命中配置 remote 且非匿名 spec 时报 `remote '{remote}' not found`（`CliInvalidTarget`）；匿名 spec 走 `RemoteConfig{name: anonymous_remote_name}` 并以空 refmap 抑制跟踪引用 | `src/command/fetch.rs:1576-1597` |
| 评审期预探（已安装 `libra 0.30.207`）：`libra fetch parentdir refs/heads/main:refs/heads/o2`（`parentdir` 为同级 Libra 仓库目录）→ exit 129、`fatal: remote 'parentdir' not found`、`LBR-CLI-003` | 评审期本机预探（2026-11-18） |
| Git 对给出但未配置的仓库名按 URL/路径处理（`remotes_remote_get_1` 在 `name_given && !valid_remote` 时 `add_url_alias`） | Git `6de20f6092:remote.c:800-830` |
| Git 匿名 fetch 的命令行 refspec 无目标时不自动跟随 tag（`autotags` 仅在命令行 refspec 有目标时置位） | Git `6de20f6092:builtin/fetch.c:525-529` |

**Acceptance criteria:**

- [x] **AC1** `libra fetch parent refs/heads/main`（未配置 `parent` remote）的 `.libra/FETCH_HEAD` 首条记录 OID 等于 `parent` 的 `main`（P1）。
- [x] **AC2** AC1 的调用不改变引用状态：调用前后 `R(child)` 逐字相等。
- [x] **AC3** AC1 的调用不写入配置：调用前后 `config_kv` 表全部行逐字相等。
- [x] **AC4** `libra fetch pbare refs/heads/main`（`pbare` 是裸 Libra 仓库目录，未配置同名 remote）的 `FETCH_HEAD` 首条记录 OID 等于 Q1。
- [x] **AC5** 已配置 remote `parent`（URL 指向 `Q`）且同名目录 `parent` 也存在时，`libra fetch parent refs/heads/main` 取自配置 remote：`FETCH_HEAD` 首条记录 OID 等于 Q1。
- [x] **AC6** `libra fetch nosuch refs/heads/main`（既无 remote `nosuch` 也无目录 `nosuch`）以 `LBR-CLI-003` 失败。
- [x] **AC7** `libra fetch plain refs/heads/main`（`plain` 是存在但非 Libra 仓库的目录）以 `LBR-CLI-003` 失败。
- [x] **AC8** `libra pull parent main`（未配置 `parent` remote）仍以 `LBR-CLI-003` 失败（本卡不改变 `pull` 的仓库参数解析）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F01：临时目录 `D` 下工作仓库 `D/child`（`libra init`，提交 C1，当前分支 `main`）；其内以 `libra init` 建嵌套 Libra 仓库子目录 `D/child/parent`（提交 P1，`main`→P1，无 tag）、以 `libra clone --bare ../q pbare` 建嵌套裸 Libra 仓库 `D/child/pbare`（`main`→Q1，无 tag），以及普通目录 `D/child/plain/`（只含普通文件 `x`，无 `.libra`）；另建独立 Libra 仓库 `D/q`（提交 Q1，`main`→Q1）；P1、Q1、C1 两两不同；被测 fetch/pull 调用均以 `cwd=D/child` 运行（裸目录名 `parent`、`plain` 相对该目录解析），夹具建立（含各仓库内的提交）与 `Ref`/`R`/`config_kv` 读取在各自仓库目录内运行；AC5 的副本另以 `libra remote add parent ../q` 配置同名 remote，其余副本不配置 remote `parent`；每个 AC 在独立同构副本上执行，执行前删除 `D/child/.libra/FETCH_HEAD`（若存在）；每个 AC 的调用另按「验证执行协议」第 4 项取所需快照；标注「Git 同夹具期望」的值按该项以钉选 Git 复核）

- AC1 → `command::fetch_test::fetch_bare_local_directory_name_as_anonymous_repository`：期望 exit 0；`.libra/FETCH_HEAD` 首行第一列逐字等于 P1 的完整 OID。
- AC2 → `command::fetch_test::fetch_bare_local_directory_name_as_anonymous_repository`：期望 AC1 调用前后 `R(child)` 逐字相等（`reference` 表含全部 remote 名下的行与 HEAD 行，任何远程跟踪引用、其它引用或 reflog 的创建都会被发现；夹具 `parent` 无 tag）。
- AC3 → `command::fetch_test::fetch_bare_local_directory_name_as_anonymous_repository`：期望 AC1 调用前后 `D/child/.libra/libra.db` 中 `config_kv` 表全部行（全部列，按 `id` 排序）逐字相等（不写 `remote.*` 配置）。
- AC4 → `command::fetch_test::fetch_bare_local_directory_name_as_anonymous_repository`：期望 exit 0；`.libra/FETCH_HEAD` 首行第一列逐字等于 Q1 的完整 OID。
- AC5 → `command::fetch_test::fetch_bare_local_directory_name_as_anonymous_repository`：期望 exit 0；`.libra/FETCH_HEAD` 首行第一列逐字等于 Q1（≠ P1）。
- AC6 → `command::fetch_test::fetch_bare_local_directory_name_as_anonymous_repository`：期望退出码非 0 且 stderr 含 `LBR-CLI-003`。
- AC7 → `command::fetch_test::fetch_bare_local_directory_name_as_anonymous_repository`：期望退出码非 0 且 stderr 含 `LBR-CLI-003`。
- AC8 → `command::fetch_test::fetch_bare_local_directory_name_as_anonymous_repository`：期望退出码非 0 且 stderr 含 `LBR-CLI-003`。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::fetch_test::fetch_bare_local_directory_name_as_anonymous_repository)'`（卡特有 `(new)`，`tests/command/fetch_test.rs` 内新增；覆盖 AC1–AC8；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::fetch_test::/)'`（共享回归组：`command::fetch_test` 模块族（含既有 `test_fetch_json_output_reports_updated_refs`、`test_fetch_prune_tags_matrix`）；按「验证执行协议」第 5 项先以同一前缀与过滤执行 `cargo nextest list` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test compat_fetch_remote_refspec`（共享回归组：整 target，fetch refspec 目标映射、FETCH_HEAD、原子多 ref 回滚回归；按「验证执行协议」第 5 项先以 `source .env.test && cargo nextest list --test compat_fetch_remote_refspec` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 修改共享单一事实源 src/internal/protocol/repository_arg.rs（GC-02），且版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `DEP-FP-01`, `DEP-FP-02`, `DEP-FP-08`

**Implementation write set:** `src/internal/protocol/repository_arg.rs`, `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `docs/commands/fetch.md`, `docs/commands/zh-CN/fetch.md`, `docs/development/commands/fetch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/internal/protocol/repository_arg.rs`, `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`（由 `--only fetch` 同步生成；网站 zh-CN 渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`fetch parent refs/heads/main`。逐文件语义核对：`docs/development/commands/fetch.md` 与 `COMPATIBILITY.md` 的 `| fetch |` 行（基线第 223 行）——受本卡行为影响即同卡原地更新（不增删行），否则按第 1 步记录理由与未变 SHA-256；本卡兼容登记要点：`| fetch |` 行登记：未配置的仓库名仅在解析为存在的 Libra 仓库目录时按本地仓库处理，其余仍报 `LBR-CLI-003`（Git 对任意未配置名按 URL/路径尝试）。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码，不新增 `StableErrorCode`）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=fetch裸本地仓库目录名识别; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=DEP-FP-01,DEP-FP-02,DEP-FP-08; writeset=序列化于DEP-FP-01; release=independent; split-from=N/A; exception=N/A`

---

### Task FP-02: 修复 `libra push` 把裸本地仓库目录名当作 remote 名（#710）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `complete`（2026-11-21：`run_push` 的仓库解析复用 FP-01 的 opt-in helper 与 `resolve_anonymous_local_spec`——未命中配置 remote 但在当前目录/同级目录解析为 Libra 仓库（裸仓库或带工作树）时按匿名本地目标推送；已配置同名 remote 仍优先；顺带修正仓库目录判定为「`.libra` 目录或根部 `libra.db`」（裸仓库无 `.libra`，此前会让裸目录名判定失败）。用例 `test_push_bare_directory_name_resolves_as_anonymous` 覆盖 AC1–AC6（同级裸仓库写 `refs/heads/trunk`、配置同名 remote 时推向配置 URL 且同名目录零变化、非裸同级仓库可用、未知名字与普通目录均 `LBR-CLI-003`）；`push`/`fetch` 套件 **106/106**；版本面 0.30.241 → 0.30.242；文档 EN/zh-CN 同步。按用户 2026-11-20 指令在 `main` 直接提交推送。）

**Description:** 在 `run_push` 解析 `<repository>` 时复用 `FP-01` 的 `is_anonymous_repository_spec_with_bare_dir`：未命中已配置 remote、但在当前目录下解析为存在的 Libra 仓库目录（含裸仓库）时，按本地仓库推送（不切换进程 CWD）；已配置同名 remote 仍优先；既非配置 remote 也非 Libra 仓库目录时保持 `remote '<name>' not found`（`LBR-CLI-003`）。

**Out of scope:** `receive.denyCurrentBranch`（`FP-05`）；`#710` 验收 3–4 的 `remote add -f` 与 `checkout`/`switch` DWIM 提示（不属 push 行为，issue 关闭口径见「成功定义」）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `run_push` 先 `ConfigKv::get_remote_url(&repository)`，失败后仅当 `is_anonymous_repository_spec` 命中才按本地路径处理，否则返回 `PushError::RemoteNotFound`（`LBR-CLI-003`） | `src/command/push.rs:963-980`、`src/command/push.rs:417-425` |
| 评审期预探（`libra 0.30.207`）：`libra push src1 main:refs/heads/trunk`（`src1` 为同级裸仓库目录）→ exit 129、`remote 'src1' not found`；`libra push ../src1 main:refs/heads/trunk` → exit 0 | 评审期本机预探（2026-11-18） |
| 上游场景：`git init --bare --initial-branch=trunk src1 && git push src1 one:refs/heads/trunk` | Git `6de20f6092:t/t2027-checkout-track.sh:50` |

**Acceptance criteria:**

- [x] **AC1** `libra push src1 one:refs/heads/trunk`（未配置 `src1` remote）后 `src1` 的 `refs/heads/trunk` 指向 W2。
- [ ] **AC2** 已配置 remote `src1`（URL 指向 `D/x`）且同名目录 `src1` 也存在时，`libra push src1 one:refs/heads/trunk` 推向 `D/x`：`D/x` 的 `refs/heads/trunk` 指向 W2。
- [ ] **AC3** AC2 的调用前后同名目录仓库的 `R(D/w/src1)` 逐字相等。
- [x] **AC4** `libra push wt one:refs/heads/trunk`（`wt` 是非裸 Libra 仓库目录、HEAD 分离，未配置同名 remote）后 `wt` 的 `refs/heads/trunk` 指向 W2。
- [x] **AC5** `libra push nosuch one:refs/heads/trunk` 以 `LBR-CLI-003` 失败。
- [x] **AC6** `libra push plain one:refs/heads/trunk`（`plain` 存在但非 Libra 仓库）以 `LBR-CLI-003` 失败。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F02：临时目录 `D` 下工作仓库 `D/w`（提交 W1、`main`→W1，另有分支 `one`→W2，W2≠W1）；`D/w/src1` 为 `libra init --bare` 建立的裸 Libra 仓库（空）；`D/w/wt` 为以 `libra init wt` 建立、提交 V1 后执行 `libra switch --detach HEAD` 的非裸 Libra 仓库（不含 `trunk`）；`D/w/plain/` 为存在但非 Libra 仓库的普通目录；`D/x` 为另一个裸 Libra 仓库（空）；AC2 与 AC3 的副本另在 `D/w` 执行 `libra remote add src1 ../x`，其余副本不配置 remote `src1`；每个 AC 在独立同构副本上执行；被测 push 调用均以 `cwd=D/w` 运行，夹具建立（含 `D/w/wt` 内的提交与分离 HEAD）与 `Ref`/`R` 读取在各自仓库目录内运行；每个 AC 的调用另按「验证执行协议」第 4 项取所需快照；标注「Git 同夹具期望」的值按该项以钉选 Git 复核）

- AC1 → `command::push_test::push_bare_local_directory_name_as_local_repository`：期望 exit 0；`Ref(D/w/src1, refs/heads/trunk)` 逐字等于 W2（Git 同夹具期望）。
- AC2 → `command::push_test::push_bare_local_directory_name_as_local_repository`：期望 exit 0；`Ref(D/x, refs/heads/trunk)` 逐字等于 W2。
- AC3 → `command::push_test::push_bare_local_directory_name_as_local_repository`：期望 AC2 调用前后 `R(D/w/src1)` 逐字相等。
- AC4 → `command::push_test::push_bare_local_directory_name_as_local_repository`：期望 exit 0；`Ref(D/w/wt, refs/heads/trunk)` 逐字等于 W2。
- AC5 → `command::push_test::push_bare_local_directory_name_as_local_repository`：期望退出码非 0 且 stderr 含 `LBR-CLI-003`。
- AC6 → `command::push_test::push_bare_local_directory_name_as_local_repository`：期望退出码非 0 且 stderr 含 `LBR-CLI-003`。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::push_test::push_bare_local_directory_name_as_local_repository)'`（卡特有 `(new)`，`tests/command/push_test.rs` 内新增；覆盖 AC1–AC6；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::push_test::/)'`（共享回归组：`command::push_test` 模块族（含既有 `test_push_local_libra_nonbare_checked_out_branch_rejected`、`test_push_multi_refspec_delete_tags_and_mirror_dry_run`）；按「验证执行协议」第 5 项先以同一前缀与过滤执行 `cargo nextest list` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test compat_local_push_no_cwd_switch`（共享回归组：整 target，本地路径 push 不切换进程 CWD 回归；按「验证执行协议」第 5 项先以 `source .env.test && cargo nextest list --test compat_local_push_no_cwd_switch` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `FP-23`, `DEP-FP-01`, `DEP-FP-08`

**Implementation write set:** `src/command/push.rs`, `tests/command/push_test.rs`, `docs/commands/push.md`, `docs/commands/zh-CN/push.md`, `docs/development/commands/push.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/push.rs`, `tests/command/push_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/push.md`、`docs/commands/zh-CN/push.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md`（由 `--only push` 同步生成；网站 zh-CN 渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`push src1`。逐文件语义核对：`docs/development/commands/push.md` 与 `COMPATIBILITY.md` 的 `| push |` 行（基线第 222 行）——受本卡行为影响即同卡原地更新（不增删行），否则按第 1 步记录理由与未变 SHA-256；本卡兼容登记要点：`| push |` 行登记：未配置的仓库名仅在解析为存在的 Libra 仓库目录时按本地仓库推送，其余报 `LBR-CLI-003`（有意差异）。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码，不新增 `StableErrorCode`）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=push裸本地仓库目录名识别; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=FP-23,DEP-FP-01,DEP-FP-08; writeset=序列化于FP-23; release=independent; split-from=N/A; exception=N/A`

---

### Task FP-03: 支持 `remote.<name>.push` 配置 refspec 与仅指定仓库的 `libra push <remote>`（#683）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `complete`（2026-11-21：`execute_safe` 在未给 refspec/`--tags`/`--mirror` 且给出 `<repository>` 时，用新增 `configured_push_refspecs` 读取 `remote.<name>.push` 全部值（配置顺序）填入 `args.refspecs`，随后走 FP-04 的同一引擎（通配符与负向 refspec 一并生效）；命令行 refspec 优先、完全忽略配置；两者都没有时保持 `repository-only push requires a refspec, --tags, or --mirror`（`LBR-CLI-002`，Git 按 `push.default` 回落为有意差异）。用例 `test_push_repository_only_uses_configured_refspecs` 覆盖 AC1–AC5（单值、两值按序、配置通配映射、配置负向排除、命令行 refspec 覆盖配置）。`push` 套件 **38/38**；版本面 0.30.243 → 0.30.244；文档 EN/zh-CN 同步。按用户 2026-11-20 指令在 `main` 直接提交推送。）

**Description:** `libra push <remote>` 未给命令行 refspec、`--tags`、`--mirror` 时读取 `remote.<remote>.push` 全部值（按配置写入顺序），经 `FP-04` 的同一 refspec 引擎展开（含通配符与负向）后推送；命令行给出 refspec 时完全忽略该配置；未配置时保持现有 `repository-only push requires a refspec, --tags, or --mirror` 拒绝（`LBR-CLI-002`，Git 此时按 `push.default` 回落，属已登记的有意差异）。

**Out of scope:** `push.default` 回落（有意差异，`COMPATIBILITY.md` 登记）；`branch --delete-merged` 与外部 `TC-0180` 全序列（`DEFER-FP-02`、`DEP-FP-07`）；`--prune` 本身（`FP-04`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `validate_push_args` 对仅有 `<repository>` 的调用直接返回 `repository-only push requires a refspec, --tags, or --mirror` | `src/command/push.rs:884-894` |
| 评审期预探（`libra 0.30.207`）：配置 `remote.origin.push=refs/heads/main:refs/heads/published` 后 `libra push origin` → exit 129、`LBR-CLI-002` | 评审期本机预探（2026-11-18） |
| 上游 `--delete-merged maps push refspecs to upstreams` 依赖 `git push origin` 使用 `remote.origin.push` | Git `6de20f6092:t/t3200-branch.sh:2006` |

**Acceptance criteria:**

- [ ] **AC1** 配置 `remote.origin.push=refs/heads/topic:refs/heads/published` 后 `libra push origin` 使 `RefMap(o)` 恰比调用前多出 `refs/heads/published`→M2 一项、其余不变。
- [ ] **AC2** 依次配置 `refs/heads/main:refs/heads/m1` 与 `refs/heads/topic:refs/heads/t1` 两值后 `libra push origin` 使 `RefMap(o)` 恰多出这两项。
- [ ] **AC3** 配置 `refs/heads/*:refs/heads/mirror/*` 后 `libra push origin` 使 `RefMap(o)` 恰多出 `mirror/main`、`mirror/topic` 两项。
- [ ] **AC4** 依次配置 `refs/heads/*:refs/heads/mirror/*` 与 `^refs/heads/topic` 后 `libra push origin` 使 `RefMap(o)` 恰多出 `mirror/main` 一项。
- [ ] **AC5** 配置 `remote.origin.push=refs/heads/topic:refs/heads/published` 时，`libra push origin main` 只按命令行推送：`RefMap(o)` 恰为调用前把 `refs/heads/main` 由 M0 改为 M1。
- [ ] **AC6** 未配置 `remote.origin.push` 时 `libra push origin` 以 `LBR-CLI-002` 失败。
- [x] **AC7** AC6 的调用前后 `R(D/o)` 逐字相等。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F03：临时目录 `D` 下推送方仓库 `D/p`（先提交 M0，再提交 M1 使 `main`→M1；以 `libra switch -c topic` 提交 M2 后 `libra switch main`，`topic`→M2；M0、M1、M2 两两不同）；目标裸 Libra 仓库 `D/o`（`libra init --bare` 建立；`D/p` 在只有 M0 时以 `libra push ../o main:refs/heads/main` 推入，之后才提交 M1，故 `D/o` 初始只含 `refs/heads/main`→M0）；`D/p` 以 `libra remote add origin ../o` 配置；每个 AC 在独立同构副本上执行，配置值以 `libra config --add remote.origin.push <值>` 按列出顺序写入；全部 push 以 `cwd=D/p` 运行；每个 AC 的调用另按「验证执行协议」第 4 项取所需快照；标注「Git 同夹具期望」的值按该项以钉选 Git 复核）

- AC1 → `command::push_test::push_uses_remote_push_refspec_config`：期望 exit 0；调用后 `RefMap(D/o)` 逐字等于调用前加上 `refs/heads/published→M2`（`main`、`topic` 未被推送；Git 同夹具期望，`t3200:2006`）。
- AC2 → `command::push_test::push_uses_remote_push_refspec_config`：期望 exit 0；调用后 `RefMap(D/o)` 逐字等于调用前加上 `{refs/heads/m1→M1, refs/heads/t1→M2}`。
- AC3 → `command::push_test::push_uses_remote_push_refspec_config`：期望 exit 0；调用后 `RefMap(D/o)` 逐字等于调用前加上 `{refs/heads/mirror/main→M1, refs/heads/mirror/topic→M2}`。
- AC4 → `command::push_test::push_uses_remote_push_refspec_config`：期望 exit 0；调用后 `RefMap(D/o)` 逐字等于调用前加上 `refs/heads/mirror/main→M1`。
- AC5 → `command::push_test::push_uses_remote_push_refspec_config`：期望 exit 0；调用后 `RefMap(D/o)` 逐字等于调用前将 `refs/heads/main` 的值替换为 M1（不出现 `refs/heads/published`）。
- AC6 → `command::push_test::push_uses_remote_push_refspec_config`：期望退出码非 0 且 stderr 含 `LBR-CLI-002`。
- AC7 → `command::push_test::push_uses_remote_push_refspec_config`：期望 AC6 调用前后 `R(D/o)` 逐字相等。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::push_test::push_uses_remote_push_refspec_config)'`（卡特有 `(new)`，`tests/command/push_test.rs` 内新增；覆盖 AC1–AC7；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::push_test::/)'`（共享回归组：`command::push_test` 模块族（含既有 `test_push_local_libra_nonbare_checked_out_branch_rejected`、`test_push_multi_refspec_delete_tags_and_mirror_dry_run`）；按「验证执行协议」第 5 项先以同一前缀与过滤执行 `cargo nextest list` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^command::push::test::/)'`（共享回归组：`src/command/push.rs` `mod test` 单元测试族；按「验证执行协议」第 5 项先以同一前缀与过滤执行 `cargo nextest list` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `FP-04`, `DEP-FP-01`, `DEP-FP-08`

**Implementation write set:** `src/command/push.rs`, `tests/command/push_test.rs`, `docs/commands/push.md`, `docs/commands/zh-CN/push.md`, `docs/development/commands/push.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/push.rs`, `tests/command/push_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/push.md`、`docs/commands/zh-CN/push.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md`（由 `--only push` 同步生成；网站 zh-CN 渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`remote.<name>.push`。逐文件语义核对：`docs/development/commands/push.md` 与 `COMPATIBILITY.md` 的 `| push |` 行（基线第 222 行）——受本卡行为影响即同卡原地更新（不增删行），否则按第 1 步记录理由与未变 SHA-256；本卡兼容登记要点：`| push |` 行登记 `remote.<name>.push`（多值、通配符、负向）已支持，未配置时仍拒绝而非按 `push.default` 回落（有意差异）。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码，不新增 `StableErrorCode`）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=push的remote.*.push配置refspec; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=FP-04,DEP-FP-01,DEP-FP-08; writeset=序列化于FP-04; release=independent; split-from=N/A; exception=N/A`

---

### Task FP-04: 支持 `libra push --prune`、通配符源 refspec 与负向 refspec（#877）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `complete`（2026-11-21：`PushArgs` 新增 `--prune`；`parse_refspec` 增 `Wildcard { src, dst }` 与 `Exclude { pattern }`，`expand_wildcard_local_refs`/`wildcard_capture` 把通配符对本地分支与标签展开为逐 ref 更新（无冒号时目标同源，映射形式把捕获段替换进目标），`negative_refspec_matches`（精确或单通配）在源侧与映射后的目标侧排除；`--prune` 以被推送的目标模式为范围，删除无本地源的远端引用，负向匹配与模式外引用不动，`--dry-run` 只报告不写入。用例 `test_push_prune_wildcards_and_negative_refspecs` 覆盖 AC1–AC6（通配推送、`--prune` + `^refs/heads/prune/b` 只删 `prune/a`、映射通配、目标侧负向只删 `theirs/a`、源侧负向只推 `a`/`c`、dry-run 前后 `R` 逐字相等）；`push` 套件 **37/37**；版本面 0.30.242 → 0.30.243；文档 EN/zh-CN 同步。按用户 2026-11-20 指令在 `main` 直接提交推送。）

**Description:** 为 `PushArgs` 新增 `--prune`，并把命令行 refspec 的匹配/规划统一到一个引擎：通配符源 refspec（`refs/heads/prune/*`，无目标时目标同源）与映射（`refs/heads/ours/*:refs/heads/theirs/*`）展开为逐 ref 更新；负向 refspec（`^<pattern>`）同时作用于源侧与映射后的目标侧；`--prune` 删除远端在本次 refspec 目标模式内、但本地已无对应源的引用，被负向 refspec 匹配的目标与目标模式之外的引用不删除。该引擎是 `FP-03` 配置 refspec 的唯一事实源（GC-02）。

**Out of scope:** `remote.<name>.push` 配置 refspec（`FP-03`，复用本卡引擎）；`--mirror` 与 `receive.denyCurrentBranch`（`FP-05`）；`:`/`+:` matching refspec（未在 `#877` 范围，Git `t5582` #13–#14）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `PushArgs` 无 `--prune`；`parse_refspec` 把 `refs/heads/prune/*` 当字面 ref，`resolve_local_ref` 报 `source ref ... not found` | `src/command/push.rs:96-184`、`src/command/push.rs:720-760`、`src/command/push.rs:1664` |
| 评审期预探（`libra 0.30.207`）：`libra push --prune ...` → `unexpected argument '--prune'`（`LBR-CLI-002`）；`libra push origin 'refs/heads/*:refs/heads/*'` → `source ref 'refs/heads/*' not found`（`LBR-CLI-003`） | 评审期本机预探（2026-11-18） |
| 上游 `push --prune with negative refspec` 与 `... apply to the destination` 的最终 `refs/heads/prune/`、`refs/heads/theirs/` 集合均为 `b c` | Git `6de20f6092:t/t5582-fetch-negative-refspec.sh:128-164` |

**Acceptance criteria:**

- [ ] **AC1** `libra push ../three 'refs/heads/prune/*'` 后 `RefMap(three)` 恰比调用前多出 `prune/a`、`prune/b`、`prune/c` 三项（均→T1）、其余不变。
- [ ] **AC2** 删除本地 `prune/a`、`prune/b` 后，`libra push --prune ../three 'refs/heads/prune/*' '^refs/heads/prune/b'` 使 `RefMap(three)` 恰比调用前少 `refs/heads/prune/a` 一项、其余不变。
- [x] **AC3** `libra push ../three 'refs/heads/ours/*:refs/heads/theirs/*'` 后 `RefMap(three)` 恰比调用前多出 `theirs/a`、`theirs/b`、`theirs/c` 三项（均→T1）、其余不变。
- [ ] **AC4** 删除本地 `ours/a`、`ours/b` 后，`libra push --prune ../three 'refs/heads/ours/*:refs/heads/theirs/*' '^refs/heads/theirs/b'` 使 `RefMap(three)` 恰比调用前少 `refs/heads/theirs/a` 一项、其余不变。
- [ ] **AC5** 在未推送过 `prune/*` 的副本上 `libra push ../three 'refs/heads/prune/*' '^refs/heads/prune/b'` 使 `RefMap(three)` 恰比调用前多出 `prune/a`、`prune/c` 两项（源侧负向排除 `prune/b`）。
- [ ] **AC6** 与 AC2 同初始状态下 `libra push --dry-run --prune ../three 'refs/heads/prune/*' '^refs/heads/prune/b'` 前后 `R(D/three)` 逐字相等。
- [ ] **AC7** 带目标的负向 refspec `libra push ../three '^refs/heads/prune/a:refs/heads/x'` 以 `LBR-CLI-002` 失败。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F04：临时目录 `D` 下推送方仓库 `D/two`（提交 T1，分支 `main`→T1，另建分支 `prune/a`、`prune/b`、`prune/c`、`ours/a`、`ours/b`、`ours/c` 均→T1）；目标为非裸 Libra 仓库 `D/three`（自 `D/two` 克隆后 `libra switch --detach HEAD`，使其分支均非检出分支），并在 `D/three` 预建分支 `other`→T1（不在任何本卡 refspec 目标模式内）；全部 push 以 `cwd=D/two`、仓库参数 `../three` 运行；「删除本地分支」用 `libra branch -D`；AC2 与 AC6 在已完成 AC1 推送的副本上执行，AC4 在已完成 AC3 推送的副本上执行，其余 AC 各用独立副本；每个 AC 的调用另按「验证执行协议」第 4 项取所需快照；标注「Git 同夹具期望」的值按该项以钉选 Git 复核）

- AC1 → `command::push_test::push_prune_wildcard_and_negative_refspecs`：期望 exit 0；调用后 `RefMap(D/three)` 逐字等于调用前加上 `{refs/heads/prune/a→T1, refs/heads/prune/b→T1, refs/heads/prune/c→T1}`。
- AC2 → `command::push_test::push_prune_wildcard_and_negative_refspecs`：期望 exit 0；调用后 `RefMap(D/three)` 逐字等于调用前去掉 `refs/heads/prune/a`（`prune/b` 因负向 refspec、`other` 因在目标模式外而保留；Git 同夹具期望，`t5582:128-145`）。
- AC3 → `command::push_test::push_prune_wildcard_and_negative_refspecs`：期望 exit 0；调用后 `RefMap(D/three)` 逐字等于调用前加上 `{refs/heads/theirs/a→T1, refs/heads/theirs/b→T1, refs/heads/theirs/c→T1}`。
- AC4 → `command::push_test::push_prune_wildcard_and_negative_refspecs`：期望 exit 0；调用后 `RefMap(D/three)` 逐字等于调用前去掉 `refs/heads/theirs/a`（Git 同夹具期望，`t5582:147-164`）。
- AC5 → `command::push_test::push_prune_wildcard_and_negative_refspecs`：期望 exit 0；调用后 `RefMap(D/three)` 逐字等于调用前加上 `{refs/heads/prune/a→T1, refs/heads/prune/c→T1}`（不出现 `refs/heads/prune/b`）。
- AC6 → `command::push_test::push_prune_wildcard_and_negative_refspecs`：期望 exit 0；调用前后 `R(D/three)` 逐字相等。
- AC7 → `command::push_test::push_prune_wildcard_and_negative_refspecs`：期望退出码非 0 且 stderr 含 `LBR-CLI-002`（Git 同夹具期望：负向 refspec 不得带目标）。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::push_test::push_prune_wildcard_and_negative_refspecs)'`（卡特有 `(new)`，`tests/command/push_test.rs` 内新增；覆盖 AC1–AC7；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::push_test::/)'`（共享回归组：`command::push_test` 模块族（含既有 `test_push_local_libra_nonbare_checked_out_branch_rejected`、`test_push_multi_refspec_delete_tags_and_mirror_dry_run`）；按「验证执行协议」第 5 项先以同一前缀与过滤执行 `cargo nextest list` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test compat_local_push_no_cwd_switch`（共享回归组：整 target，本地路径 push 不切换进程 CWD 回归；按「验证执行协议」第 5 项先以 `source .env.test && cargo nextest list --test compat_local_push_no_cwd_switch` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `FP-02`, `DEP-FP-01`, `DEP-FP-08`

**Implementation write set:** `src/command/push.rs`, `tests/command/push_test.rs`, `docs/commands/push.md`, `docs/commands/zh-CN/push.md`, `docs/development/commands/push.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/push.rs`, `tests/command/push_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/push.md`、`docs/commands/zh-CN/push.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md`（由 `--only push` 同步生成；网站 zh-CN 渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`push --prune`。逐文件语义核对：`docs/development/commands/push.md` 与 `COMPATIBILITY.md` 的 `| push |` 行（基线第 222 行）——受本卡行为影响即同卡原地更新（不增删行），否则按第 1 步记录理由与未变 SHA-256；本卡兼容登记要点：`| push |` 行登记 `--prune`、通配符源与负向 refspec 已支持。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码，不新增 `StableErrorCode`）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=push命令行refspec通配符/负向与--prune; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=FP-02,DEP-FP-01,DEP-FP-08; writeset=序列化于FP-02; release=independent; split-from=N/A; exception=N/A`

---

### Task FP-05: 本地推送遵循目标 `receive.denyCurrentBranch=warn|ignore`（#1031）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** `apply_local_push_to_libra` 在更新目标非裸仓库的当前检出分支前读取目标仓库的 `receive.denyCurrentBranch`：`warn` 时更新该分支并向推送方 stderr 输出 `warning: updating the current branch`；`ignore` 时静默更新；未配置、`refuse` 及其它值保持现有拒绝（`LBR-REPO-003`）。允许更新时只改目标引用，不改目标工作区与索引（与 Git 一致）。

**Out of scope:** `updateInstead` 及布尔值 `true`/`false` 的映射（保持拒绝，`COMPATIBILITY.md` 有意差异）；远端 HTTP/SSH `receive-pack`；未配置时 Git 的长篇拒绝说明文本。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `apply_one_update` 对目标当前检出分支一律返回 `refusing to update checked-out branch`，未读取目标配置 | `src/internal/protocol/local_push.rs:92-112` |
| `PushError::LocalPush` 映射为 `RepoStateInvalid`（`LBR-REPO-003`） | `src/command/push.rs:451-454` |
| Git `DENY_WARN` 输出 `warning: updating the current branch` 后放行，`DENY_IGNORE` 静默放行，`DENY_REFUSE`/未配置拒绝 | Git `6de20f6092:builtin/receive-pack.c:1543-1560` |
| 上游 `t5517` 与 `t5543` 的镜像仓库对均设 `receive.denyCurrentBranch warn` 后执行 `git push --mirror up` | Git `6de20f6092:t/t5517-push-mirror.sh:26` |

**Acceptance criteria:**

- [ ] **AC1** 目标为 `warn` 时 `libra push --mirror ../t` 后目标 `refs/heads/main` 指向 T2。
- [ ] **AC2** AC1 的 stderr 含 `warning: updating the current branch`。
- [ ] **AC3** 目标为 `warn` 时 `libra push ../t main` 后目标 `refs/heads/main` 指向 T2。
- [ ] **AC4** 目标为 `warn` 时 AC3 的调用前后目标 `W(D/t)` 逐字相等（只改引用，不改工作区与索引）。
- [ ] **AC5** 目标为 `ignore` 时 `libra push ../t main` 后目标 `refs/heads/main` 指向 T2。
- [ ] **AC6** AC5 的 stderr 不含 `updating the current branch`。
- [ ] **AC7** 目标为 `refuse` 时 `libra push ../t main` 以 `LBR-REPO-003` 失败。
- [x] **AC8** AC7 的调用前后目标 `R(D/t)` 逐字相等。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F05：临时目录 `D` 下非裸目标仓库 `D/t`（提交 T1，当前检出分支 `main`→T1，工作区含已跟踪文件 `f`=`v1`）；推送方 `D/p` 自 `D/t` 克隆后在 `main` 上新增提交 T2（修改 `f`=`v2`）；每个 AC 在独立同构副本上执行，目标配置以 `cwd=D/t` 的 `libra config receive.denyCurrentBranch <值>` 写入（「未配置」即不写）；push 以 `cwd=D/p` 运行，仓库参数 `../t`；每个 AC 的调用另按「验证执行协议」第 4 项取所需快照；标注「Git 同夹具期望」的值按该项以钉选 Git 复核）

- AC1 → `command::push_test::push_local_respects_receive_deny_current_branch`：期望 exit 0；`Ref(D/t, refs/heads/main)` 逐字等于 T2（Git 同夹具期望）。
- AC2 → `command::push_test::push_local_respects_receive_deny_current_branch`：期望 AC1 调用的 stderr 含子串 `warning: updating the current branch`（Git 经 sideband 时前缀 `remote: `，Libra 本地推送不要求该前缀）。
- AC3 → `command::push_test::push_local_respects_receive_deny_current_branch`：期望 exit 0；`Ref(D/t, refs/heads/main)` 逐字等于 T2。
- AC4 → `command::push_test::push_local_respects_receive_deny_current_branch`：期望 AC3 调用前后 `W(D/t)` 逐字相等。
- AC5 → `command::push_test::push_local_respects_receive_deny_current_branch`：期望 exit 0；`Ref(D/t, refs/heads/main)` 逐字等于 T2。
- AC6 → `command::push_test::push_local_respects_receive_deny_current_branch`：期望 AC5 调用的 stderr 不含子串 `updating the current branch`。
- AC7 → `command::push_test::push_local_respects_receive_deny_current_branch`：期望退出码非 0 且 stderr 含 `LBR-REPO-003`。
- AC8 → `command::push_test::push_local_respects_receive_deny_current_branch`：期望 AC7 调用前后 `R(D/t)` 逐字相等。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::push_test::push_local_respects_receive_deny_current_branch)'`（卡特有 `(new)`，`tests/command/push_test.rs` 内新增；覆盖 AC1–AC8；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::push_test::/)'`（共享回归组：`command::push_test` 模块族（含既有 `test_push_local_libra_nonbare_checked_out_branch_rejected`、`test_push_multi_refspec_delete_tags_and_mirror_dry_run`）；按「验证执行协议」第 5 项先以同一前缀与过滤执行 `cargo nextest list` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test compat_local_push_no_cwd_switch`（共享回归组：整 target，本地路径 push 不切换进程 CWD 回归；按「验证执行协议」第 5 项先以 `source .env.test && cargo nextest list --test compat_local_push_no_cwd_switch` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `FP-03`, `DEP-FP-01`, `DEP-FP-08`

**Implementation write set:** `src/internal/protocol/local_push.rs`, `src/command/push.rs`, `tests/command/push_test.rs`, `docs/commands/push.md`, `docs/commands/zh-CN/push.md`, `docs/development/commands/push.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/internal/protocol/local_push.rs`, `src/command/push.rs`, `tests/command/push_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/push.md`、`docs/commands/zh-CN/push.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md`（由 `--only push` 同步生成；网站 zh-CN 渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`receive.denyCurrentBranch`。逐文件语义核对：`docs/development/commands/push.md` 与 `COMPATIBILITY.md` 的 `| push |` 行（基线第 222 行）——受本卡行为影响即同卡原地更新（不增删行），否则按第 1 步记录理由与未变 SHA-256；本卡兼容登记要点：`| push |` 行登记本地推送支持目标 `receive.denyCurrentBranch=warn|ignore`；`updateInstead` 与布尔值仍按拒绝处理（有意差异）。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码，不新增 `StableErrorCode`）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=本地push的receive.denyCurrentBranch; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=FP-03,DEP-FP-01,DEP-FP-08; writeset=序列化于FP-03; release=independent; split-from=N/A; exception=N/A`

---

### Task FP-06: 解除 `push --force` 与 `--force-with-lease` 互斥并由 `--force` 覆盖租约拒绝（#1036；拆出 FP-21）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 按 ADR-FP-02 移除 `PushArgs::force_with_lease` 的 clap `conflicts_with = "force"`；同时给出 `--force` 与 `--force-with-lease` 时，租约不匹配（stale）或非快进的拒绝原因被 `--force` 覆盖，推送照常执行且该更新标记为强制更新（人读输出含 `(forced update)`，对齐 Git `set_ref_status_for_push`）；未给 `--force` 时租约校验保持不变。按 G-09 自本卡拆出 `FP-21`（无 `--force` 时的租约语义矩阵）。

**Out of scope:** 无 `--force` 时的租约语义（`<expect>` 修订表达式、跟踪引用租约、`stale info` 诊断由 `FP-21` 承接，空期望值由 `FP-25` 承接）；`--force-if-includes`（保持现状）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `force_with_lease` 声明 `conflicts_with = "force"` | `src/command/push.rs:118-119` |
| 单元测试 `force_with_lease_conflicts_with_force` 断言二者在 clap 层冲突 | `src/command/push.rs:4172-4179`（`mod test` 起于 `3294`） |
| 租约不匹配时 `validate_force_with_lease` 返回 `PushError::NonFastForward`（`LBR-CONFLICT-002`） | `src/command/push.rs:1895-1926`、`src/command/push.rs:477-480` |
| Git：存在拒绝原因且被 `--force` 覆盖时 `forced_update = 1`；`t5533` #3、#5、#10 要求 `--force --force-with-lease` 组合成功 | Git `6de20f6092:remote.c:1682-1771`、`6de20f6092:t/t5533-push-cas.sh:79-90,113-130,182-190` |

**Acceptance criteria:**

- [ ] **AC1** `dst` 提交 D（`main`→D）后 `libra push --force --force-with-lease=main:<D 的完整 OID> origin main`（租约期望 D≠远端 C，属 stale；本卡以完整 OID 表达期望值，修订表达式解析由 `FP-21` 交付）使 `src` 的 `main` 指向 D。
- [ ] **AC2** AC1 的人读 stdout 含 `(forced update)`。
- [ ] **AC3** `src` 先在 `main` 上提交 D′（`libra switch main`、提交、`libra switch --detach HEAD`），`dst` 提交 E 后 `libra push --force --force-with-lease=main origin main`（跟踪引用 C≠远端 D′）使 `src` 的 `main` 指向 E。
- [ ] **AC4** `libra push --force --force-with-lease=main:<B 的完整 OID> origin :main`（期望 B≠远端 C，属 stale）删除 `src` 的 `main`。
- [ ] **AC5** 不带 `--force` 时 AC1 同初始状态的 `libra push --force-with-lease=main:<D 的完整 OID> origin main` 以 `LBR-CONFLICT-002` 失败。
- [ ] **AC6** AC5 的调用前后 `R(D/src)` 逐字相等。
- [ ] **AC7** 单元层面 `PushArgs::try_parse_from(["push","--force","--force-with-lease","origin","main"])` 返回 `Ok`（clap 层不再互斥）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F06（同构 `t5533` `setup_srcdst_basic`）：临时目录 `D` 下模板仓库 `D/tpl`（依次提交 A、B、C，`main`→C）；`D/src` 自 `D/tpl` 克隆后执行 `libra switch --detach HEAD`（`main` 不再是检出分支）；`D/dst` 自 `D/src` 克隆（remote `origin`→`../src`，`refs/remotes/origin/main`→C）；每个 AC 在独立同构副本上执行；push 以 `cwd=D/dst` 运行；每个 AC 的调用另按「验证执行协议」第 4 项取所需快照；标注「Git 同夹具期望」的值按该项以钉选 Git 复核）

- AC1 → `command::push_test::push_force_overrides_force_with_lease_rejection`：期望 exit 0；`Ref(D/src, refs/heads/main)` 逐字等于 D（Git 同夹具期望，`t5533:79-90`）。
- AC2 → `command::push_test::push_force_overrides_force_with_lease_rejection`：期望 AC1 调用的 stdout 含子串 `(forced update)`（Libra push 人读输出在 stdout，`src/command/push.rs:2531`）。
- AC3 → `command::push_test::push_force_overrides_force_with_lease_rejection`：期望 exit 0；`Ref(D/src, refs/heads/main)` 逐字等于 E（Git 同夹具期望，`t5533:113-130`）。
- AC4 → `command::push_test::push_force_overrides_force_with_lease_rejection`：期望 exit 0；调用后 `Ref(D/src, refs/heads/main)` 为 `absent`（Git 同夹具期望，`t5533:182-190`）。
- AC5 → `command::push_test::push_force_overrides_force_with_lease_rejection`：期望退出码非 0 且 stderr 含 `LBR-CONFLICT-002`。
- AC6 → `command::push_test::push_force_overrides_force_with_lease_rejection`：期望 AC5 调用前后 `R(D/src)` 逐字相等。
- AC7 → `command::push::test::force_and_force_with_lease_parse_together`：期望 `command::push::test::force_and_force_with_lease_parse_together` 中该解析返回 `Ok`（替换并删除原 `force_with_lease_conflicts_with_force`）。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::push_test::push_force_overrides_force_with_lease_rejection)'`（卡特有 `(new)`，`tests/command/push_test.rs` 内新增；覆盖 AC1–AC6；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(=command::push::test::force_and_force_with_lease_parse_together)'`（卡特有 `(new)`，`src/command/push.rs` 的 `mod test` 内新增，并删除被取代的 `force_with_lease_conflicts_with_force`；覆盖 AC7；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::push_test::/)'`（共享回归组：`command::push_test` 模块族（含既有 `test_push_local_libra_nonbare_checked_out_branch_rejected`、`test_push_multi_refspec_delete_tags_and_mirror_dry_run`）；按「验证执行协议」第 5 项先以同一前缀与过滤执行 `cargo nextest list` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `FP-05`, `DEP-FP-01`, `DEP-FP-08`

**Implementation write set:** `src/command/push.rs`, `tests/command/push_test.rs`, `docs/commands/push.md`, `docs/commands/zh-CN/push.md`, `docs/development/commands/push.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/push.rs`, `tests/command/push_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/push.md`、`docs/commands/zh-CN/push.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md`（由 `--only push` 同步生成；网站 zh-CN 渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--force --force-with-lease`。逐文件语义核对：`docs/development/commands/push.md` 与 `COMPATIBILITY.md` 的 `| push |` 行（基线第 222 行）——受本卡行为影响即同卡原地更新（不增删行），否则按第 1 步记录理由与未变 SHA-256；本卡兼容登记要点：`| push |` 行登记 `--force` 可与 `--force-with-lease` 同用并覆盖租约拒绝。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码，不新增 `StableErrorCode`）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=push--force覆盖--force-with-lease; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=FP-05,DEP-FP-01,DEP-FP-08; writeset=序列化于FP-05; release=independent; split-from=N/A; exception=N/A`

---

### Task FP-21: 无 `--force` 时 `--force-with-lease` 租约语义对齐 Git（#1036；自 FP-06 拆出；拆出 FP-25）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 对齐 Git 的租约判定：`--force-with-lease=<ref>:<expect>` 的非空 `<expect>` 在本地按修订表达式解析为 OID（如 `main^`）后再与远端比较；`=<ref>` 与裸形式以本地远程跟踪引用为期望值；租约匹配时允许非快进更新与删除；租约不匹配（stale）时以 `LBR-CONFLICT-002` 拒绝该推送、远端引用不变，且诊断含 `stale info`。

**Out of scope:** 空期望 `<ref>:`（远端必须不存在，`FP-25`）；`--force` 与租约组合（`FP-06`）；`--force-if-includes`；推送输出流（Libra push 人读输出保持 stdout）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `parse_force_with_lease` 把 `<expect>` 原样存为字符串，`lease_oid_matches` 只做字符串/前缀比较，`main^` 永不匹配 | `src/command/push.rs:1855-1893` |
| 租约不匹配返回 `PushError::NonFastForward`（诊断 `non-fast-forward update`，`LBR-CONFLICT-002`） | `src/command/push.rs:1895-1926`、`src/command/push.rs:300-304` |
| `#1036`：`t5533` #6（`main:main^`）、#8（跟踪租约非快进）在 Libra 0.30.91 失败（#15 `branch:` 新建由 `FP-25` 承接） | issue `#1036`；Git `6de20f6092:t/t5533-push-cas.sh:132-142,157-169` |
| Git：stale 时 `REF_STATUS_REJECT_STALE`，诊断 `(stale info)` | Git `6de20f6092:remote.c:1715-1730`、`6de20f6092:t/t5533-push-cas.sh:66-77` |

**Acceptance criteria:**

- [ ] **AC1** `dst` 提交 D 后 `libra push --force-with-lease=main:main origin main`（期望=D≠远端 C）以 `LBR-CONFLICT-002` 失败。
- [ ] **AC2** AC1 的 stderr 含 `stale info`。
- [ ] **AC3** AC1 的调用前后 `R(D/src)` 逐字相等。
- [ ] **AC4** `libra push --force-with-lease=main:main^ origin :main`（`dst` 未新增提交，`main^` 解析为 B≠远端 C）以 `LBR-CONFLICT-002` 失败（期望值按修订表达式解析后比较；跳过租约检查的实现会删除远端 `main`）。
- [ ] **AC5** AC4 的调用前后 `R(D/src)` 逐字相等。
- [ ] **AC6** `dst` 提交 D 后 `libra push --force-with-lease=main:main^ origin main`（`main^` 解析为 C=远端）使 `src` 的 `main` 指向 D。
- [ ] **AC7** `dst` 执行 `libra reset --hard HEAD^` 后提交 D（非快进），`libra push --force-with-lease=main origin main`（跟踪 C=远端）使 `src` 的 `main` 指向 D。
- [ ] **AC8** `dst` 提交 D 后 `libra push --force-with-lease=main:main^ origin :main`（`main^` 解析为 C=远端）删除 `src` 的 `main`（删除路径同样按修订表达式解析期望值）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F21（同构 `t5533` `setup_srcdst_basic`）：临时目录 `D` 下模板仓库 `D/tpl`（依次提交 A、B、C，`main`→C）；`D/src` 自 `D/tpl` 克隆后执行 `libra switch --detach HEAD`（`main` 不再是检出分支）；`D/dst` 自 `D/src` 克隆（remote `origin`→`../src`，`refs/remotes/origin/main`→C）；每个 AC 在独立同构副本上执行；push 以 `cwd=D/dst` 运行；每个 AC 的调用另按「验证执行协议」第 4 项取所需快照；标注「Git 同夹具期望」的值按该项以钉选 Git 复核）

- AC1 → `command::push_test::push_force_with_lease_semantics_without_force`：期望退出码非 0 且 stderr 含 `LBR-CONFLICT-002`。
- AC2 → `command::push_test::push_force_with_lease_semantics_without_force`：期望 AC1 调用的 stderr 含子串 `stale info`（Git 同夹具期望，`t5533:66-77`）。
- AC3 → `command::push_test::push_force_with_lease_semantics_without_force`：期望 AC1 调用前后 `R(D/src)` 逐字相等。
- AC4 → `command::push_test::push_force_with_lease_semantics_without_force`：期望退出码非 0 且 stderr 含 `LBR-CONFLICT-002`（Git 同夹具期望，`t5533:171-180`）。
- AC5 → `command::push_test::push_force_with_lease_semantics_without_force`：期望 AC4 调用前后 `R(D/src)` 逐字相等。
- AC6 → `command::push_test::push_force_with_lease_semantics_without_force`：期望 exit 0；`Ref(D/src, refs/heads/main)` 逐字等于 D（Git 同夹具期望，`t5533:132-142`）。
- AC7 → `command::push_test::push_force_with_lease_semantics_without_force`：期望 exit 0；`Ref(D/src, refs/heads/main)` 逐字等于 D（Git 同夹具期望，`t5533:157-169`）。
- AC8 → `command::push_test::push_force_with_lease_semantics_without_force`：期望 exit 0；调用后 `Ref(D/src, refs/heads/main)` 为 `absent`（删除路径上对 `main^` 做字面比较或只解析简单分支名的实现会拒绝；与 AC4 合起来区分删除路径跳过租约检查的实现）。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::push_test::push_force_with_lease_semantics_without_force)'`（卡特有 `(new)`，`tests/command/push_test.rs` 内新增；覆盖 AC1–AC8；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::push_test::/)'`（共享回归组：`command::push_test` 模块族（含既有 `test_push_local_libra_nonbare_checked_out_branch_rejected`、`test_push_multi_refspec_delete_tags_and_mirror_dry_run`）；按「验证执行协议」第 5 项先以同一前缀与过滤执行 `cargo nextest list` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^command::push::test::/)'`（共享回归组：`src/command/push.rs` `mod test` 单元测试族；按「验证执行协议」第 5 项先以同一前缀与过滤执行 `cargo nextest list` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `FP-06`, `DEP-FP-01`, `DEP-FP-08`

**Implementation write set:** `src/command/push.rs`, `tests/command/push_test.rs`, `docs/commands/push.md`, `docs/commands/zh-CN/push.md`, `docs/development/commands/push.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/push.rs`, `tests/command/push_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/push.md`、`docs/commands/zh-CN/push.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md`（由 `--only push` 同步生成；网站 zh-CN 渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`stale info`。逐文件语义核对：`docs/development/commands/push.md` 与 `COMPATIBILITY.md` 的 `| push |` 行（基线第 222 行）——受本卡行为影响即同卡原地更新（不增删行），否则按第 1 步记录理由与未变 SHA-256；本卡兼容登记要点：`| push |` 行登记 `--force-with-lease` 的 `<expect>` 修订表达式、跟踪租约非快进与 `stale info` 诊断；退出码沿用 `LBR-CONFLICT-002` 默认 128（Git 为 1，有意差异）。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码，不新增 `StableErrorCode`）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=push非空期望与跟踪租约判定; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=FP-06,DEP-FP-01,DEP-FP-08; writeset=序列化于FP-06; release=independent; split-from=FP-06; exception=N/A`

---

### Task FP-23: 命令行 fetch refspec 的短目标补全与匿名 fetch 写入显式目标（#719；自 FP-01 拆出）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `complete`（2026-11-21：`parse_fetch_refspec` 对目标套用 Git `get_local_ref` 补全（新增 `complete_refspec_destination`：`refs/…` 原样、`heads/`/`tags/`/`remotes/` 补 `refs/`、其余补 `refs/heads/`；`HEAD`、通配与空目标不补全）；匿名 fetch 仅在命令行 refspec **不带目标**时才用空 refmap 抑制写入，带显式目标时按目标写入（`effective_refmap` 条件增加 `cli_has_explicit_destination`），`refs/tags/…` 目标仍由 `validate_fetch_destination` 以 `tag destinations are not supported` 拒绝（`LBR-CLI-002`）。用例 `test_fetch_short_destination_completion_and_anonymous_targets` 覆盖 AC1–AC7（裸目录名 + 短目标只新增 `refs/heads/origin`、`../parent` 全目标写 `refs/heads/x`、`remotes/` 与 `heads/` 前缀补全、`tags/t` 拒绝且不写入）；AC2 使用同级相对路径 `../parent`（`./parent` 需源嵌套在克隆内）。`fetch`/`pull`/`clone`/`remote` 套件 **249/249**；版本面 0.30.240 → 0.30.241；文档 EN/zh-CN 同步。按用户 2026-11-20 指令在 `main` 直接提交推送。）

**Description:** 在 `parse_fetch_refspec` 中按 Git `get_local_ref` 规则补全命令行 refspec 的非 `refs/` 目标（`heads/`、`tags/`、`remotes/` 前缀补 `refs/`，其余补 `refs/heads/`），并让匿名本地仓库 fetch（`./<dir>`、绝对路径、`FP-01` 的裸目录名）在命令行 refspec 带显式目标时写入该目标引用；不带目标的匿名 fetch 行为不变。`tags/` 补全得到的 `refs/tags/*` 目标仍按 `validate_fetch_destination` 现有限制拒绝。

> **后续验证草稿（未排期、未验收，不改变 FP-23 已完成的历史状态）：** 0.30.207 使用匿名 HTTP URL 显式映射曾在下载 trace 对象后遗漏目标 tracking ref；远端 `c84109a` 已修改匿名显式目标的共用路径，但 HTTP 与 trace 组合尚未验证。待 `ls-refs` 发现问题修复后，在 loopback smart HTTP 和临时虚构仓库中验证：`main`→M1，独有 `refs/libra/traces/unique`→T1，T1 的 `trace.txt` 内容为 `unique fictional trace payload\n`；以 Git 同夹具先核对 ref、OID 和文件，客户端先 clone main 并确认本地无 T1，再直接通过 URL fetch `refs/libra/traces/unique:refs/remotes/origin/trace-url`，以该 ref 读回 T1 和文件精确字节。测试服务须正确实现 v2，不使用请求改写代理宣称产品通过。本事项目前没有接收卡、执行者与测试服务；Claude 评审指出须补齐归属、依赖、写集、执行命令和门禁，完成并通过复审前不得称为已排期或使用本草稿开工。

**Out of scope:** 仓库参数识别（`FP-01`）；`refs/tags/*` 目标支持（保持现有拒绝，`COMPATIBILITY.md` 有意差异）；配置 refspec 的目标命名空间放宽（`FP-09`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `parse_fetch_refspec` 只对源调用 `normalize_branch_ref`，目标原样参与 `refspec_ref_is_valid` 校验，`main:origin` 被拒为 `source/destination must be valid full refs` | `src/command/fetch.rs:1920-1978`（目标原样在 `1952-1955`） |
| 匿名 spec 在无 `--refmap` 时把 `effective_refmap` 置为空串，压掉命令行显式目标的写入 | `src/command/fetch.rs:1592-1597` |
| 评审期预探（`libra 0.30.207`）：`libra fetch ../parent main:origin` → exit 129、`LBR-CLI-002`；`libra fetch ../parent refs/heads/main:refs/heads/origin` → exit 0 但不创建 `refs/heads/origin` | 评审期本机预探（2026-11-18） |
| Git 目标补全规则：`refs/` 原样；`heads/`、`tags/`、`remotes/` 前缀补 `refs/`；其余补 `refs/heads/` | Git `6de20f6092:remote.c:2118-2132` |
| `refs/tags/*` 目标由 `validate_fetch_destination` 以专用诊断 `tag destinations are not supported by fetch refspecs; use --tags` 拒绝；未补全的短目标（如 `tags/t`）在此之前即以 `source/destination must be valid full refs ...` 拒绝；两者均为 `InvalidRefspec`（消息 `invalid fetch refspec '<spec>': <reason>`，`LBR-CLI-002`） | `src/command/fetch.rs:1896-1901,1958-1968,978-979,1092-1093` |

**Acceptance criteria:**

- [ ] **AC1** `libra fetch parent main:origin` 后 `RefMap(child)` 恰比调用前多出 `refs/heads/origin`→P1 一项、其余不变。
- [x] **AC2** `libra fetch ./parent refs/heads/main:refs/heads/x` 后 `refs/heads/x` 指向 P1。
- [ ] **AC3** `libra fetch origin main:remotes/origin/copy` 后 `refs/remotes/origin/copy` 指向 U1。
- [ ] **AC4** `libra fetch origin main:heads/h` 后 `refs/heads/h` 指向 U1。
- [ ] **AC5** `libra fetch origin main:tags/t` 以 `LBR-CLI-002` 失败（补全为 `refs/tags/t` 后仍按现有标签目标限制拒绝）。
- [ ] **AC6** AC5 的 stderr 含 `tag destinations are not supported`（拒绝发生在补全为 `refs/tags/t` 之后的标签目标检查）。
- [ ] **AC7** AC5 的调用前后 `S(child)` 逐字相等。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F23：临时目录 `D` 下工作仓库 `D/child`（提交 C1，当前分支 `main`）；其内以 `libra init` 建嵌套 Libra 仓库子目录 `D/child/parent`（提交 P1，`main`→P1，无 tag）；独立 Libra 仓库 `D/upstream`（提交 U1，`main`→U1，无 tag），`D/child` 以 `libra remote add origin ../upstream` 配置；P1、U1、C1 两两不同；不配置 remote `parent`；被测 fetch 调用均以 `cwd=D/child` 运行（`parent`、`./parent` 相对该目录解析），夹具建立（含 `D/child/parent` 与 `D/upstream` 内的提交）与 `Ref`/`RefMap`/快照读取在各自仓库目录内执行；每个 AC 在独立同构副本上执行；每个 AC 的调用另按「验证执行协议」第 4 项取所需快照；标注「Git 同夹具期望」的值按该项以钉选 Git 复核）

- AC1 → `command::fetch_test::fetch_command_line_refspec_short_destination_and_anonymous_write`：期望 exit 0；调用后 `RefMap(child)` 逐字等于调用前 `RefMap(child)` 加上条目 `refs/heads/origin→P1`（Git 同夹具期望：目标补全为 `refs/heads/origin`）。
- AC2 → `command::fetch_test::fetch_command_line_refspec_short_destination_and_anonymous_write`：期望 exit 0；`Ref(child, refs/heads/x)` 逐字等于 P1。
- AC3 → `command::fetch_test::fetch_command_line_refspec_short_destination_and_anonymous_write`：期望 exit 0；`Ref(child, refs/remotes/origin/copy)` 逐字等于 U1（Git 同夹具期望：目标补全为 `refs/remotes/origin/copy`）。
- AC4 → `command::fetch_test::fetch_command_line_refspec_short_destination_and_anonymous_write`：期望 exit 0；`Ref(child, refs/heads/h)` 逐字等于 U1。
- AC5 → `command::fetch_test::fetch_command_line_refspec_short_destination_and_anonymous_write`：期望退出码非 0 且 stderr 含 `LBR-CLI-002`。
- AC6 → `command::fetch_test::fetch_command_line_refspec_short_destination_and_anonymous_write`：期望 AC5 调用的 stderr 含子串 `tag destinations are not supported`（未补全即按完整 ref 校验拒绝的实现输出 `must be valid full refs`，不含该子串）。
- AC7 → `command::fetch_test::fetch_command_line_refspec_short_destination_and_anonymous_write`：期望 AC5 调用前后 `S(child)` 逐字相等。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::fetch_test::fetch_command_line_refspec_short_destination_and_anonymous_write)'`（卡特有 `(new)`，`tests/command/fetch_test.rs` 内新增；覆盖 AC1–AC7；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::fetch_test::/)'`（共享回归组：`command::fetch_test` 模块族（含既有 `test_fetch_json_output_reports_updated_refs`、`test_fetch_prune_tags_matrix`）；按「验证执行协议」第 5 项先以同一前缀与过滤执行 `cargo nextest list` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test compat_fetch_remote_refspec`（共享回归组：整 target，fetch refspec 目标映射、FETCH_HEAD、原子多 ref 回滚回归；按「验证执行协议」第 5 项先以 `source .env.test && cargo nextest list --test compat_fetch_remote_refspec` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `FP-01`, `DEP-FP-01`, `DEP-FP-08`

**Implementation write set:** `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `docs/commands/fetch.md`, `docs/commands/zh-CN/fetch.md`, `docs/development/commands/fetch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`（由 `--only fetch` 同步生成；网站 zh-CN 渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`main:origin`。逐文件语义核对：`docs/development/commands/fetch.md` 与 `COMPATIBILITY.md` 的 `| fetch |` 行（基线第 223 行）——受本卡行为影响即同卡原地更新（不增删行），否则按第 1 步记录理由与未变 SHA-256；本卡兼容登记要点：`| fetch |` 行登记命令行 refspec 短目标补全（Git `get_local_ref` 规则）与匿名 fetch 写显式目标；`refs/tags/*` 目标仍不支持（有意差异）。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码，不新增 `StableErrorCode`）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=命令行refspec目标补全与匿名显式目标写入; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=FP-01,DEP-FP-01,DEP-FP-08; writeset=序列化于FP-01; release=independent; split-from=FP-01; exception=N/A`

---

### Task FP-25: `--force-with-lease=<ref>:` 空期望租约要求远端引用不存在（#1036；自 FP-21 拆出）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 对齐 Git 的空期望租约：`--force-with-lease=<ref>:`（冒号后为空）表示远端该 ref 必须不存在——远端不存在时允许按普通推送创建；远端已存在时（无论不带租约时是否为快进）以 `LBR-CONFLICT-002` 拒绝该推送且远端引用不变。

**Out of scope:** 非空 `<expect>` 的修订表达式解析、跟踪租约与 `stale info` 诊断（`FP-21`）；`--force` 与租约组合（`FP-06`）；`--force-if-includes`；推送输出流（Libra push 人读输出保持 stdout）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `parse_force_with_lease` 把 `<ref>:` 的空 `<expect>` 存为空字符串；`lease_oid_matches` 对远端不存在（`None`）判不匹配（新建被拒），对远端已存在时空串前缀恒匹配（应拒绝却放行） | `src/command/push.rs:1855-1867,1879-1887` |
| `#1036`：`t5533` #15（`branch:` 新建）在 Libra 0.30.91 失败 | issue `#1036`；Git `6de20f6092:t/t5533-push-cas.sh:247-257` |
| Git：`<ref>:` 把期望值置为空 OID（`oidclr`）并复制为该 ref 的 `old_oid_expect`；显式期望与远端 `old_oid` 不等即 `REF_STATUS_REJECT_STALE`，判定先于快进检查 | Git `6de20f6092:remote.c:2741-2749,2917-2924,1716-1718` |
| 上游 `new branch already exists` 中远端 `branch` 在 F（C 的后代），不带租约推送 C 本就因非快进被拒，不能单独区分空期望判定；本卡夹具改为远端 `branch` 在 B（C 的父提交），使不带租约推送 C 为快进 | Git `6de20f6092:t/t5533-push-cas.sh:259-271` |

**Acceptance criteria:**

- [ ] **AC1** 远端无 `branch` 时 `libra push --force-with-lease=branch: origin branch` 使 `src` 出现 `refs/heads/branch`→C。
- [ ] **AC2** 远端 `branch`→B（不带租约推送 C 为快进）时 `libra push --force-with-lease=branch: origin branch` 以 `LBR-CONFLICT-002` 失败。
- [ ] **AC3** AC2 的调用前后 `R(D/src)` 逐字相等。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F25（同构 `t5533` `setup_srcdst_basic`）：临时目录 `D` 下模板仓库 `D/tpl`（依次提交 A、B、C，`main`→C）；`D/src` 自 `D/tpl` 克隆后执行 `libra switch --detach HEAD`（`main` 不再是检出分支）；`D/dst` 自 `D/src` 克隆（remote `origin`→`../src`，`refs/remotes/origin/main`→C），再在 `D/dst` 执行 `libra branch branch main`（`branch`→C）；AC2 与 AC3 的副本另在 `D/src` 执行 `libra branch branch main^`（`refs/heads/branch`→B）；前置断言（另一同构 AC2 副本）：不带租约的 `libra push origin branch` exit 0 且使 `src` 的 `branch` 指向 C（证明无租约时为快进）；每个 AC 在独立同构副本上执行；push 以 `cwd=D/dst` 运行；每个 AC 的调用另按「验证执行协议」第 4 项取所需快照；标注「Git 同夹具期望」的值按该项以钉选 Git 复核）

- AC1 → `command::push_test::push_force_with_lease_empty_expectation`：期望 exit 0；`Ref(D/src, refs/heads/branch)` 逐字等于 C（Git 同夹具期望，`t5533:247-257`）。
- AC2 → `command::push_test::push_force_with_lease_empty_expectation`：期望退出码非 0 且 stderr 含 `LBR-CONFLICT-002`（Git 同夹具期望：`remote.c:1716-1718` 判 stale；夹具改造自 `t5533:259-271`）。
- AC3 → `command::push_test::push_force_with_lease_empty_expectation`：期望 AC2 调用前后 `R(D/src)` 逐字相等。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::push_test::push_force_with_lease_empty_expectation)'`（卡特有 `(new)`，`tests/command/push_test.rs` 内新增；覆盖 AC1–AC3；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::push_test::/)'`（共享回归组：`command::push_test` 模块族（含既有 `test_push_local_libra_nonbare_checked_out_branch_rejected`、`test_push_multi_refspec_delete_tags_and_mirror_dry_run`）；按「验证执行协议」第 5 项先以同一前缀与过滤执行 `cargo nextest list` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^command::push::test::/)'`（共享回归组：`src/command/push.rs` `mod test` 单元测试族；按「验证执行协议」第 5 项先以同一前缀与过滤执行 `cargo nextest list` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `FP-21`, `DEP-FP-01`, `DEP-FP-08`

**Implementation write set:** `src/command/push.rs`, `tests/command/push_test.rs`, `docs/commands/push.md`, `docs/commands/zh-CN/push.md`, `docs/development/commands/push.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/push.rs`, `tests/command/push_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/push.md`、`docs/commands/zh-CN/push.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md`（由 `--only push` 同步生成；网站 zh-CN 渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`force-with-lease=<ref>:`。逐文件语义核对：`docs/development/commands/push.md` 与 `COMPATIBILITY.md` 的 `| push |` 行（基线第 222 行）——受本卡行为影响即同卡原地更新（不增删行），否则按第 1 步记录理由与未变 SHA-256；本卡兼容登记要点：`| push |` 行登记 `--force-with-lease=<ref>:` 空期望（远端必须不存在）语义；退出码沿用 `LBR-CONFLICT-002` 默认 128（Git 为 1，有意差异）。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码，不新增 `StableErrorCode`）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=push空期望租约; recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=FP-21,DEP-FP-01,DEP-FP-08; writeset=序列化于FP-21; release=independent; split-from=FP-21; exception=N/A`

---

### Task FP-07: 无参 `libra fetch` 在无上游时按 Git 选择默认 remote（镜像克隆、无上游分支与分离 HEAD）（#749）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：无参 `libra fetch` 按 Git `remotes_remote_for_branch` 选远程——`branch.<name>.remote` 优先，否则唯一 remote，否则 `origin`；多个 remote 且无 `origin` 时保持 `LBR-REPO-003`（新增 `default_fetch_remote()`）。`command::fetch_test::fetch_without_repository_uses_git_default_remote` 覆盖 AC1–AC8：镜像仓库引用集合与源相等（AC1）、仅 origin 的无上游分支（AC2）、分离 HEAD（AC3）、唯一 `up`（AC4）、`branch.main.remote=up` 优先（AC5）且 `origin` 跟踪引用不变（AC6）、双非 origin remote 拒绝（AC7）且源仓库 `show-ref` 逐字不变（AC8）；`fetch_test` 全模块 64 passed / 0 failed；版本面 patch 0.30.219 → 0.30.220。按用户 2026-11-20 指令在 `main` 直接提交推送。）

**Description:** 无 `<repository>` 且非 `--all` 时，按 Git `remotes_remote_for_branch`：当前分支配置了 `branch.<name>.remote` 则用之；否则（当前分支无上游、HEAD 分离，或裸/镜像仓库 HEAD 指向无上游的分支）若恰好配置了一个 remote 则用该 remote，否则用 `origin`；所选 remote 未配置时保持 `no configured remote for the current branch`（`LBR-REPO-003`）。读取当前分支与配置的存储错误按既有 `IoReadFailed` 路径失败，不得当作无上游而回落。镜像克隆的 `+refs/*:refs/*` 映射与既有 prune 语义不变。

**Out of scope:** 多远程 `--all`/`--multiple`（`FP-10`、`FP-19`、`FP-11`）；`remote.<name>.mirror` 对 fetch 的任何新增语义（Git 中 mirror 只影响 push）；`--prune`/`--no-prune` 与 `fetch.prune`/`remote.<name>.prune` 优先级（既有实现，`src/command/fetch.rs:4997-5001,5078`，由 `compat_config_defaults_semantics` 回归，本卡不改）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 无参 fetch 走 `ConfigKv::get_current_remote()`：`Ok(None)` 报 `no configured remote for the current branch`（`RepoStateInvalid`），`Err` 一律报 `HEAD is detached` | `src/command/fetch.rs:1546-1566` |
| `get_current_remote_with_conn` 对分离 HEAD 返回 `Err`，数据库错误同样返回 `Err` | `src/internal/config.rs:787-806` |
| 评审期预探（`libra 0.30.207`）：`libra clone --mirror ./parent mir` 写入 `remote.origin.fetch=+refs/*:refs/*`、`remote.origin.mirror=true`，`cd mir && libra fetch` → exit 128、`LBR-REPO-003` | 评审期本机预探（2026-11-18） |
| Git 默认 remote：分支上游优先；否则恰有一个 remote 时用它，否则 `origin` | Git `6de20f6092:remote.c:673-687` |
| 上游 `--single-branch with --mirror`：镜像内 `git fetch` 成功且 `for-each-ref refs` 与源一致 | Git `6de20f6092:t/t5612-clone-refspec.sh:206` |

**Acceptance criteria:**

- [ ] **AC1** `D/mir` 中无参 `libra fetch` 后，`mir` 的 `refs/heads/*` 与 `refs/tags/*` 条目集合等于源 `s` 的同名集合。
- [ ] **AC2** `D/c`（当前分支无上游、只有 `origin`）中无参 `libra fetch` 使 `refs/remotes/origin/main` 指向 S2。
- [ ] **AC3** `D/d`（HEAD 分离、只有 `origin`）中无参 `libra fetch` 使 `refs/remotes/origin/main` 指向 S2。
- [ ] **AC4** `D/n1`（无上游、恰有一个 remote `up`、无 `origin`）中无参 `libra fetch` 使 `refs/remotes/up/main` 指向 S2。
- [ ] **AC5** `D/u`（`branch.main.remote=up`，同时存在 `origin`）中无参 `libra fetch` 使 `refs/remotes/up/main` 指向 S3。
- [ ] **AC6** AC5 的调用前后 `origin` 的远程跟踪引用不变（按上游取 `up` 而非回落 `origin`）。
- [ ] **AC7** `D/n2`（无上游、两个 remote 均非 `origin`）中无参 `libra fetch` 以 `LBR-REPO-003` 失败。
- [ ] **AC8** AC7 的调用前后 `S(D/n2)` 逐字相等。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F07：临时目录 `D` 下源仓库 `D/s`（提交 S1，`main`→S1、分支 `side`→S1、轻量 tag `t1`→S1）与 `D/s2`（`libra clone ./s s2` 后在 `D/s2` 提交 S3）；`D/mir` 由 `libra clone --mirror ./s mir` 建立；`D/c` 由 `libra clone ./s c` 建立后执行 `libra config --unset branch.main.remote`（`main` 无上游，remote `origin` 仍在，且只有这一个 remote）；`D/d` 为 `D/c` 的同构副本并执行 `libra switch --detach HEAD`；`D/u` 为 `D/c` 的同构副本，另以 `libra remote add up ../s2` 配置并设 `branch.main.remote=up`；`D/n1` 为新建仓库（提交 N1，只配置 remote `up`→`../s`，无上游、无 `origin`）；`D/n2` 为新建仓库（提交 N2，配置 remote `up1`→`../s` 与 `up2`→`../s`，无上游、无 `origin`）；夹具建立后在 `D/s` 提交 S2（`main`→S2）、新建分支 `feat`→S2 与轻量 tag `t2`→S2；S1、S2、S3 两两不同；每个 AC 在独立同构副本上执行；每个 AC 的调用另按「验证执行协议」第 4 项取所需快照；标注「Git 同夹具期望」的值按该项以钉选 Git 复核）

- AC1 → `command::fetch_test::fetch_without_repository_uses_git_default_remote`：期望 exit 0；`RefMap(D/mir)` 与 `RefMap(D/s)` 各自键以 `refs/heads/` 或 `refs/tags/` 开头的条目集合逐字相等，即 `{refs/heads/main→S2, refs/heads/side→S1, refs/heads/feat→S2, refs/tags/t1→S1, refs/tags/t2→S2}`（Git 同夹具期望，`t5612:206`）。
- AC2 → `command::fetch_test::fetch_without_repository_uses_git_default_remote`：期望 exit 0；`Ref(D/c, refs/remotes/origin/main)` 逐字等于 S2。
- AC3 → `command::fetch_test::fetch_without_repository_uses_git_default_remote`：期望 exit 0；`Ref(D/d, refs/remotes/origin/main)` 逐字等于 S2（Git 同夹具期望）。
- AC4 → `command::fetch_test::fetch_without_repository_uses_git_default_remote`：期望 exit 0；`Ref(D/n1, refs/remotes/up/main)` 逐字等于 S2（Git 同夹具期望，`remote.c:673-687`）。
- AC5 → `command::fetch_test::fetch_without_repository_uses_git_default_remote`：期望 exit 0；`Ref(D/u, refs/remotes/up/main)` 逐字等于 S3。
- AC6 → `command::fetch_test::fetch_without_repository_uses_git_default_remote`：期望 AC5 调用前后 `RefMap(D/u)` 中键以 `refs/remotes/origin/` 开头的条目集合逐字相等。
- AC7 → `command::fetch_test::fetch_without_repository_uses_git_default_remote`：期望退出码非 0 且 stderr 含 `LBR-REPO-003`。
- AC8 → `command::fetch_test::fetch_without_repository_uses_git_default_remote`：期望 AC7 调用前后 `S(D/n2)` 逐字相等。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::fetch_test::fetch_without_repository_uses_git_default_remote)'`（卡特有 `(new)`，`tests/command/fetch_test.rs` 内新增；覆盖 AC1–AC8；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::fetch_test::/)'`（共享回归组：`command::fetch_test` 模块族（含既有 `test_fetch_json_output_reports_updated_refs`、`test_fetch_prune_tags_matrix`）；按「验证执行协议」第 5 项先以同一前缀与过滤执行 `cargo nextest list` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test compat_fetch_remote_refspec`（共享回归组：整 target，fetch refspec 目标映射、FETCH_HEAD、原子多 ref 回滚回归；按「验证执行协议」第 5 项先以 `source .env.test && cargo nextest list --test compat_fetch_remote_refspec` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `FP-25`, `DEP-FP-01`, `DEP-FP-05`, `DEP-FP-08`

**Implementation write set:** `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `docs/commands/fetch.md`, `docs/commands/zh-CN/fetch.md`, `docs/development/commands/fetch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`（由 `--only fetch` 同步生成；网站 zh-CN 渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`remote.origin.mirror`。逐文件语义核对：`docs/development/commands/fetch.md` 与 `COMPATIBILITY.md` 的 `| fetch |` 行（基线第 223 行）——受本卡行为影响即同卡原地更新（不增删行），否则按第 1 步记录理由与未变 SHA-256；本卡兼容登记要点：`| fetch |` 行登记无参 fetch 在无上游或分离 HEAD 时按 Git 规则选默认 remote（唯一 remote 或 `origin`）。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码，不新增 `StableErrorCode`）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=无参fetch的默认remote选择; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=FP-25,DEP-FP-01,DEP-FP-05,DEP-FP-08; writeset=序列化于FP-25; release=independent; split-from=N/A; exception=N/A`

---

### Task FP-08: 支持 `libra fetch` 一次传入多个 refspec 并按序写入 `FETCH_HEAD`（#1000）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 把 `FetchArgs::refspec` 改为可多值的位置参数 `refspecs: Vec<String>`，一次 fetch 在任何网络交互与写入前解析全部 refspec（任一无效即整体拒绝）、在同一事务中更新显式目标，并按命令行顺序写入 `FETCH_HEAD`；命令行 refspec 拉取的记录均为可合并（无 `not-for-merge`），与 Git 一致。`--json` 的 `refspec` 字段改为按命令行顺序以单个空格连接的全部 refspec（单个 refspec 时取值与现状相同）；`--set-upstream` 与多于一个命令行 refspec 同用时一律输出告警 `multiple branches detected, incompatible with --set-upstream` 且不写 upstream（Libra 规则：Git 按本次 ref 映射中无本地目标即只进 `FETCH_HEAD` 的条目计数——多于一个才输出该告警，恰一个时以其设置 upstream，全部带目标时告警 `no source branch found`，`builtin/fetch.c:2114-2165`、`remote.c:2158-2159`；两者只在全部 refspec 均不带目标时一致，带目标 refspec 参与时的差异登记为有意差异）。`pull`/`clone` 复用的 `fetch_repository_with_result` 单 refspec 签名不变，多 refspec 走新增内部入口。

**Out of scope:** `--multiple` 多仓库（`FP-10`）；输出流与 `-> FETCH_HEAD` 计划行（`FP-13`）；`pull` 的多 refspec（不在本计划）；多 refspec 的 `--dry-run` 专项验收（沿用单 refspec 的 dry-run 语义，由 `compat_fetch_remote_refspec` 回归）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `FetchArgs::refspec: Option<String>`（单值）；`FetchOutput.refspec: Option<String>` 序列化于 `--json` | `src/command/fetch.rs:684-686`、`src/command/fetch.rs:914-919` |
| `fetch_repository_with_result` 被 `pull`、`clone` 以单 refspec 调用 | `src/command/fetch.rs:2322`、`src/command/pull.rs:419`、`src/command/clone.rs:1654` |
| `apply_fetch_set_upstream` 只处理一个 refspec | `src/command/fetch.rs:1362-1403` |
| 评审期预探（`libra 0.30.207`）：`libra fetch ../parent refs/heads/main refs/heads/side` → `unexpected argument`（`LBR-CLI-002`） | 评审期本机预探（2026-11-18） |
| Git：命令行 refspec 拉取的全部 ref 标记 `FETCH_HEAD_MERGE`；`--set-upstream` 统计 ref 映射中 `peer_ref` 为空（命令行 refspec 不带目标）的条目，多于一个时告警 `multiple branches detected, incompatible with --set-upstream` 且不写，恰一个时以其设置 upstream，没有时告警 `no source branch found`；带目标的 refspec 经 `get_local_ref(refspec->dst)` 得到 `peer_ref`，不参与计数 | Git `6de20f6092:builtin/fetch.c:525-533,2114-2165`、`6de20f6092:remote.c:2158-2159`；上游场景 `6de20f6092:t/t6433-merge-toplevel.sh:131` |

**Acceptance criteria:**

- [ ] **AC1** `libra fetch origin left right` 后 `FETCH_HEAD` 各行第一列按顺序为 `[L1, R1]`。
- [ ] **AC2** AC1 的 `FETCH_HEAD` 全文不含 `not-for-merge`。
- [ ] **AC3** `libra fetch origin refs/heads/left:refs/heads/l2 refs/heads/right:refs/heads/r2` 后 `RefMap(c)` 中键以 `refs/heads/` 开头的新增条目恰为 `l2`→L1 与 `r2`→R1。
- [ ] **AC4** `libra fetch origin left 'refs/heads/bad..x'` 以 `LBR-CLI-002` 失败。
- [ ] **AC5** AC4 的调用前后 `S(D/c)` 逐字相等。
- [ ] **AC6** `libra --json fetch origin left right` 的 stdout JSON 中 `data.refspec` 为 `"left right"`。
- [ ] **AC7** `libra fetch --set-upstream origin left right` 的 stderr 含 `multiple branches detected, incompatible with --set-upstream`。
- [ ] **AC8** AC7 的调用前后 `config_kv` 中 `branch.main.remote` 与 `branch.main.merge` 两键的行集合不变。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F08：临时目录 `D` 下源仓库 `D/s`（`main`→S1，分支 `left`→L1、`right`→R1，S1、L1、R1 两两不同，无 tag）；`D/c` 由 `libra clone ./s c` 建立（当前分支 `main`）；每个 AC 在独立同构副本上执行，执行前删除 `D/c/.libra/FETCH_HEAD`（若存在）；每个 AC 的调用另按「验证执行协议」第 4 项取所需快照；标注「Git 同夹具期望」的值按该项以钉选 Git 复核）

- AC1 → `command::fetch_test::fetch_multiple_command_line_refspecs`：期望 exit 0；`.libra/FETCH_HEAD` 恰有 2 行，其第一列组成的列表逐字等于 `[L1, R1]`（Git 同夹具期望）。
- AC2 → `command::fetch_test::fetch_multiple_command_line_refspecs`：期望 AC1 后 `.libra/FETCH_HEAD` 全文不含子串 `not-for-merge`（Git 同夹具期望，`builtin/fetch.c:531-533`）。
- AC3 → `command::fetch_test::fetch_multiple_command_line_refspecs`：期望 exit 0；`RefMap(D/c)` 相对调用前新增的、键以 `refs/heads/` 开头的条目集合逐字等于 `{refs/heads/l2→L1, refs/heads/r2→R1}`。
- AC4 → `command::fetch_test::fetch_multiple_command_line_refspecs`：期望退出码非 0 且 stderr 含 `LBR-CLI-002`。
- AC5 → `command::fetch_test::fetch_multiple_command_line_refspecs`：期望 AC4 调用前后 `S(D/c)` 逐字相等。
- AC6 → `command::fetch_test::fetch_multiple_command_line_refspecs`：期望 exit 0；stdout 解析为 JSON 后 `data.refspec` 逐字等于 `"left right"`。
- AC7 → `command::fetch_test::fetch_multiple_command_line_refspecs`：期望 exit 0；stderr 含该子串（Git 同夹具期望：`left`、`right` 均不带目标，`builtin/fetch.c:2129-2135`）。
- AC8 → `command::fetch_test::fetch_multiple_command_line_refspecs`：期望 AC7 调用前后 `config_kv` 中 `key` 为 `branch.main.remote` 或 `branch.main.merge` 的行集合（全部列）逐字相等。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::fetch_test::fetch_multiple_command_line_refspecs)'`（卡特有 `(new)`，`tests/command/fetch_test.rs` 内新增；覆盖 AC1–AC8；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::fetch_test::/)'`（共享回归组：`command::fetch_test` 模块族（含既有 `test_fetch_json_output_reports_updated_refs`、`test_fetch_prune_tags_matrix`）；按「验证执行协议」第 5 项先以同一前缀与过滤执行 `cargo nextest list` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test compat_fetch_remote_refspec`（共享回归组：整 target，fetch refspec 目标映射、FETCH_HEAD、原子多 ref 回滚回归；按「验证执行协议」第 5 项先以 `source .env.test && cargo nextest list --test compat_fetch_remote_refspec` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `FP-07`, `DEP-FP-01`, `DEP-FP-08`

**Implementation write set:** `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `docs/commands/fetch.md`, `docs/commands/zh-CN/fetch.md`, `docs/development/commands/fetch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`（由 `--only fetch` 同步生成；网站 zh-CN 渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`libra fetch origin left right`。逐文件语义核对：`docs/development/commands/fetch.md` 与 `COMPATIBILITY.md` 的 `| fetch |` 行（基线第 223 行）——受本卡行为影响即同卡原地更新（不增删行），否则按第 1 步记录理由与未变 SHA-256；本卡兼容登记要点：`| fetch |` 行登记多个命令行 refspec 与按序 `FETCH_HEAD`；`--json` 的 `refspec` 为空格连接的全部 refspec；`--set-upstream` 与多个 refspec 同用时一律告警且不写 upstream（Git 按不带目标的 refspec 计数，带目标 refspec 参与时有意差异）。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码，不新增 `StableErrorCode`）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=fetch多个命令行refspec; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=FP-07,DEP-FP-01,DEP-FP-08; writeset=序列化于FP-07; release=independent; split-from=N/A; exception=N/A`

---

### Task FP-09: 允许 fetch refspec 写入 `refs/heads/`、`refs/remotes/` 之外的自定义命名空间（#722）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 放宽 `validate_fetch_destination`：除现有 `refs/heads/*`、`refs/remotes/<remote>/*` 外，允许任意通过 refname 校验的 `refs/<ns>/...` 目标（命令行与 `remote.<name>.fetch` 配置、含通配符、含 `fetch --all` 预校验），写入后的引用须能被 `libra rev-parse` 与 `libra show-ref` 读出：其中 `refs/remotes/<x>/...` 目标的 `<x>` 不是已配置 remote 时（如上游的 `refs/remotes/extra_dir/repo_c/extra_dir/*`），改为与其它 `refs/*` 目标一样按全名行（`remote` 为空）存储，避免挂在未配置 remote 名下而不被 `show-ref` 枚举；仍拒绝 `refs/tags/*` 目标（现有限制，有意差异）、现有保留目标 `refs/heads/HEAD` 与 `refs/remotes/<remote>/HEAD`（分支段恰为 `HEAD`；现有限制保持不变，Git 接受二者，有意差异）与不合法 refname（`..`、控制字符等）；`refs/remotes/<remote>/x/HEAD` 等嵌套名与新放宽的 `refs/<ns>/...` 目标（含末段为 `HEAD` 者，如 `refs/custom/HEAD`）不设额外 `HEAD` 限制，与 Git 一致。

**Out of scope:** `refs/tags/*` 目标（保持拒绝）；命令行短目标补全（`FP-23`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `validate_fetch_destination` 只放行 `refs/heads/*` 与 `refs/remotes/<remote>/*`（镜像 `refs/*:refs/*` 除外），拒绝 `refs/tags/*` | `src/command/fetch.rs:1887-1917` |
| `--all` 在任何网络 fetch 前对每个 remote 调用 `validate_configured_fetch_refspecs` | `src/command/fetch.rs:1498-1505`、`src/command/fetch.rs:2001-2003` |
| 评审期预探（`libra 0.30.207`）：`remote.rd.fetch=+refs/heads/*:refs/rd/*` 后 `libra fetch rd` → `LBR-CLI-002` | 评审期本机预探（2026-11-18） |
| 现有 `HEAD` 保留规则只拒绝分支段恰为 `HEAD` 的 `refs/heads/HEAD` 与 `refs/remotes/<remote>/HEAD`；评审期预探（`libra 0.30.207`）：`libra fetch origin refs/heads/main:refs/remotes/origin/x/HEAD` → exit 0 且 `libra rev-parse --verify refs/remotes/origin/x/HEAD` 读出源 OID，`...:refs/remotes/origin/HEAD` 与 `...:refs/heads/HEAD` → `LBR-CLI-002`；Git（2.54.0）对上述四种目标及 `refs/custom/HEAD` 均接受 | `src/command/fetch.rs:1902-1910`；评审期本机预探（2026-11-18） |
| `fetch_destination_storage` 已把其它 `refs/*` 目标存为全名分支行，但对 `refs/remotes/<x>/...` 取首段 `<x>` 作为 `remote` 列（`<x>` 未配置时 `show-ref` 只遍历已配置 remote 而枚举不到，`src/command/show_ref.rs:276-293`）；评审期预探：此类行可被 `libra rev-parse --verify refs/rd/main` 与 `libra show-ref`（显示 `refs/rd/main`）读出，而 `libra for-each-ref` 误显示为 `refs/heads/refs/rd/main`（既有显示缺陷，不在本卡范围，见「风险登记」） | `src/command/fetch.rs:4431-4455`；评审期本机预探（2026-11-18） |
| 上游 `setup more remotes with unconventional refspecs` 配置 `refs/remotes/extra_dir/repo_c/extra_dir/*` 与 `refs/repo_d/*` 后 `git fetch --all` | Git `6de20f6092:t/t2024-checkout-dwim.sh:180` |

**Acceptance criteria:**

- [ ] **AC1（草稿待整改）** F09-TRACE 中 `libra fetch origin refs/libra/traces/unique:refs/libra/traces/unique` 后目标 trace ref 指向 T1。
- [ ] **AC2** 配置 `remote.rd.fetch=+refs/heads/*:refs/rd/*` 后 `libra fetch rd` 使 `RefMap(w)` 恰比调用前多出 `refs/rd/main`→S1 与 `refs/rd/side`→S2 两项。
- [ ] **AC3** `repo_c`、`repo_d` 两个非常规配置下 `libra fetch --all` 使 `RefMap(w)` 恰比调用前多出两个命名空间下的四项。
- [ ] **AC4** `libra fetch origin refs/heads/main:refs/tags/x` 以 `LBR-CLI-002` 失败。
- [ ] **AC5** 存在 `remote.bad.fetch=+refs/heads/*:refs/a..b/*` 时 `libra fetch --all` 以 `LBR-CLI-002` 失败。
- [ ] **AC6** AC5 的调用前后 `S(D/w)` 逐字相等（非法目标在任何网络 fetch 前被拒，`origin` 也未被拉取）。
- [ ] **AC7** `libra fetch origin refs/heads/main:refs/remotes/origin/x/HEAD` 后 `refs/remotes/origin/x/HEAD` 指向 S1（嵌套 `HEAD` 名不属保留目标）。
- [ ] **AC8** `libra fetch origin refs/heads/main:refs/custom/HEAD` 后 `refs/custom/HEAD` 指向 S1（新放宽命名空间不设末段 `HEAD` 限制）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F09：临时目录 `D` 下源仓库 `D/s`（`main`→S1、`side`→S2，S1≠S2，无 tag）；工作仓库 `D/w`（提交 W1，无 tag）以 `libra remote add` 配置 `origin`→`../s`；AC2 的副本另配 remote `rd`→`../s`、`remote.rd.fetch=+refs/heads/*:refs/rd/*`；AC3 的副本不配置 `origin`，只配 remote `repo_c`→`../s`（`remote.repo_c.fetch=+refs/heads/*:refs/remotes/extra_dir/repo_c/extra_dir/*`）与 `repo_d`→`../s`（`remote.repo_d.fetch=+refs/heads/*:refs/repo_d/*`）；AC5、AC6 的副本只配置 `origin` 与 `bad`→`../s`（`remote.bad.fetch=+refs/heads/*:refs/a..b/*`）；每个 AC 在独立同构副本上执行；每个 AC 的调用另按「验证执行协议」第 4 项取所需快照；标注「Git 同夹具期望」的值按该项以钉选 Git 复核）

- AC1 → `command::fetch_test::fetch_unconventional_destination_namespace`：独立 F09-TRACE 夹具使用临时 Git 裸源仓库，`main`→M1、`refs/libra/traces/unique`→T1；T1 为仅经 trace 可达的子提交，含 `trace.txt` 固定虚构内容 `unique fictional trace payload\n`。客户端仅 clone `main`，使用其 origin 指向该源，前置断言 T1 不在客户端对象库。上述 fetch 期望 exit 0，`rev-parse` 和 `show-ref` 读出目标 T1，以目标读取 `trace.txt` 得到相同字节；Git 同夹具同 refspec 对照。原 `refs/heads/main:refs/custom/main` 场景保留为回归对照（目标指向 S1）。**评审阻断：**该映射含多个独立判据并弱化原 AC1，实际计数超出 G-03；须拆分或重设计并同步审计表、依赖和验收后重新评审。
- AC2 → `command::fetch_test::fetch_unconventional_destination_namespace`：期望 exit 0；调用后 `RefMap(D/w)` 逐字等于调用前加上 `{refs/rd/main→S1, refs/rd/side→S2}`。
- AC3 → `command::fetch_test::fetch_unconventional_destination_namespace`：期望 exit 0；调用后 `RefMap(D/w)` 逐字等于调用前加上 `{refs/repo_d/main→S1, refs/repo_d/side→S2, refs/remotes/extra_dir/repo_c/extra_dir/main→S1, refs/remotes/extra_dir/repo_c/extra_dir/side→S2}`（Git 同夹具期望，`t2024:180`）。
- AC4 → `command::fetch_test::fetch_unconventional_destination_namespace`：期望退出码非 0 且 stderr 含 `LBR-CLI-002`。
- AC5 → `command::fetch_test::fetch_unconventional_destination_namespace`：期望退出码非 0 且 stderr 含 `LBR-CLI-002`。
- AC6 → `command::fetch_test::fetch_unconventional_destination_namespace`：期望 AC5 调用前后 `S(D/w)` 逐字相等。
- AC7 → `command::fetch_test::fetch_unconventional_destination_namespace`：期望 exit 0；`Ref(D/w, refs/remotes/origin/x/HEAD)` 逐字等于 S1（Git 同夹具期望：Git 接受该目标）。
- AC8 → `command::fetch_test::fetch_unconventional_destination_namespace`：期望 exit 0；`Ref(D/w, refs/custom/HEAD)` 逐字等于 S1（Git 同夹具期望：Git 接受该目标；按「以 `/HEAD` 结尾即拒绝」实现的版本以 `LBR-CLI-002` 失败）。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::fetch_test::fetch_unconventional_destination_namespace)'`（卡特有 `(new)`，`tests/command/fetch_test.rs` 内新增；覆盖 AC1–AC8；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::fetch_test::/)'`（共享回归组：`command::fetch_test` 模块族（含既有 `test_fetch_json_output_reports_updated_refs`、`test_fetch_prune_tags_matrix`）；按「验证执行协议」第 5 项先以同一前缀与过滤执行 `cargo nextest list` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test compat_fetch_remote_refspec`（共享回归组：整 target，fetch refspec 目标映射、FETCH_HEAD、原子多 ref 回滚回归；按「验证执行协议」第 5 项先以 `source .env.test && cargo nextest list --test compat_fetch_remote_refspec` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `FP-08`, `DEP-FP-01`, `DEP-FP-08`

**Implementation write set:** `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `docs/commands/fetch.md`, `docs/commands/zh-CN/fetch.md`, `docs/development/commands/fetch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`（由 `--only fetch` 同步生成；网站 zh-CN 渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`refs/repo_d/*`。逐文件语义核对：`docs/development/commands/fetch.md` 与 `COMPATIBILITY.md` 的 `| fetch |` 行（基线第 223 行）——受本卡行为影响即同卡原地更新（不增删行），否则按第 1 步记录理由与未变 SHA-256；本卡兼容登记要点：`| fetch |` 行登记任意合法 `refs/<ns>/` 目标（含末段为 `HEAD` 者）；`refs/tags/*`、`refs/heads/HEAD` 与 `refs/remotes/<remote>/HEAD` 目标仍不支持（有意差异）。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码，不新增 `StableErrorCode`）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=fetch目标命名空间放宽; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=FP-08,DEP-FP-01,DEP-FP-08; writeset=序列化于FP-08; release=independent; split-from=N/A; exception=N/A`

---

### Task FP-10: 支持 `libra fetch --multiple` 与 `--jobs`（#867；拆出 FP-19）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 为 `FetchArgs` 新增 `--multiple`/`-m`（全部位置参数视为 remote 名，任一不是已配置 remote 时在任何网络交互前以 `LBR-CLI-003` 拒绝）与 `--jobs`/`-j <n>`（接受任意整数，非整数以 `LBR-CLI-002` 拒绝；生效并发数：`0` 取 CPU 核数（`std::thread::available_parallelism()`，取不到时为 1），未给出与负值为 1）；多远程时按位置参数顺序逐一串行 fetch——Libra 本卡不实现真正的并发执行，`--jobs` 只决定生效并发数的计算与下述追踪行，串行执行的结果与 Git 并行执行的最终引用一致（有意差异，真正并发由 `DEFER-FP-05` 承接）；未给 `--append` 时 `FETCH_HEAD` 先截断再按 remote 顺序追加，给出 `--append` 时保留已有记录、按 remote 顺序追加在其后（与 Git `fetch_multiple` 一致）。为使上游 `t5514` #13 可判定，当环境变量 `GIT_TRACE` 为绝对路径且一次 fetch 涉及多于一个 remote 时，向该文件追加一行含 `preparing to run up to <n> tasks`（`<n>` 为生效并发数）的追踪；`GIT_TRACE` 其它取值不实现。按 G-09 自本卡拆出 `FP-19`（`fetch.all`/`--no-all`）。

**Out of scope:** `fetch.all` 与 `--no-all`（`FP-19`）；单个 remote 失败后继续与逐远程失败汇总（`FP-11`）；`--jobs>1` 的真正并发执行（`DEFER-FP-05`）；remote 组（`remotes.<group>`）；`fetch.parallel` 配置；`GIT_TRACE` 的 `1`/`2`/`true` 等取值。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `FetchArgs` 无 `--multiple`/`--jobs`；`--all` 与 `repository` 以 clap `conflicts_with` 互斥 | `src/command/fetch.rs:680-691` |
| `--all` 循环逐 remote 顺序 fetch | `src/command/fetch.rs:1489-1545` |
| Git：`--multiple` 遇未知名 `die("no such remote or remote group")`；`--jobs=0` 取 `online_cpus()`；`max_children < 0` 时取 `fetch.parallel`；`fetch_multiple` 只在未给 `--append` 且需写 `FETCH_HEAD` 时先截断（`if (!append && write_fetch_head)`），再对每个 remote 以 `--append` 运行 | Git `6de20f6092:builtin/fetch.c:2324-2380,2733-2767`（截断条件 `2330-2334`） |
| 上游 `parallel` 断言 trace 含 `preparing to run up to 2 tasks`；`--jobs=0 picks a default` | Git `6de20f6092:t/t5514-fetch-multiple.sh:214-249` |

**Acceptance criteria:**

- [ ] **AC1** `libra fetch --multiple one two` 后 `RefMap(w)` 恰比调用前多出 `one/main`→O1 与 `two/main`→T1 两项、其余不变（只拉取所列 remote，`origin/main` 仍为 X1）。
- [ ] **AC2** `FETCH_HEAD` 已有旧记录（第一列 X1）时执行 AC1 的调用（未给 `--append`），调用后 `FETCH_HEAD` 先被截断再按 remote 顺序追加：第一列依次恰为 O1、T1。
- [ ] **AC3** `FETCH_HEAD` 已有旧记录（第一列 X1）时 `libra fetch --append --multiple one two` 后 `FETCH_HEAD` 保留旧记录并按 remote 顺序追加：第一列依次恰为 X1、O1、T1。
- [ ] **AC4** `libra fetch --multiple one nosuch` 以 `LBR-CLI-003` 失败。
- [ ] **AC5** AC4 的调用前后 `S(D/w)` 逐字相等。
- [ ] **AC6** `libra fetch --jobs=2 --multiple one two` 后 `RefMap(w)` 的变化与 AC1 相同。
- [ ] **AC7** `GIT_TRACE=<临时绝对路径>` 下 `libra fetch --jobs=0 --multiple one two` 后该文件含 `preparing to run up to <N> tasks`（N 为 CPU 核数）。
- [ ] **AC8** `GIT_TRACE=<临时绝对路径>` 下 `libra fetch --jobs=2 --multiple one two` 后该文件含 `preparing to run up to 2 tasks`。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F10：临时目录 `D` 下源仓库 `D/one`（`main`→O1）、`D/two`（`main`→T1）、`D/o`（`main`→X1），三者均只有 `main` 分支且无 tag；工作仓库 `D/w` 由 `libra clone ./o w` 建立（`refs/remotes/origin/main`→X1）后以 `libra remote add` 配置 `one`→`../one`、`two`→`../two`，随后在 `D/o` 提交 X2（`main`→X2，若误拉取 `origin` 会使其跟踪引用变为 X2）；O1、T1、X1、X2 两两不同；每个 AC 在独立同构副本上执行，执行前删除 `D/w/.libra/FETCH_HEAD`，AC2、AC3 的副本随后改为写入只含一行旧记录的 `D/w/.libra/FETCH_HEAD`（该行为 X1 的完整 OID 后接两个制表符与 `stale` 及换行，第一列 X1）；每个 AC 的调用另按「验证执行协议」第 4 项取所需快照；标注「Git 同夹具期望」的值按该项以钉选 Git 复核）

- AC1 → `command::fetch_test::fetch_multiple_remotes_and_jobs`：期望 exit 0；调用后 `RefMap(D/w)` 逐字等于调用前加上 `{refs/remotes/one/main→O1, refs/remotes/two/main→T1}`（误拉取 `origin` 会把 `refs/remotes/origin/main` 改为 X2 而使整表不等；Git 同夹具期望）。
- AC2 → `command::fetch_test::fetch_multiple_remotes_and_jobs`：期望 exit 0；`.libra/FETCH_HEAD` 全部行的第一列组成的列表逐字等于 `[O1, T1]`（调用前写入的 X1 旧记录不再出现；两源均只有 `main` 且无 tag，各贡献一行；Git 同夹具期望，`builtin/fetch.c:2330-2334`）。
- AC3 → `command::fetch_test::fetch_multiple_remotes_and_jobs`：期望 exit 0；`.libra/FETCH_HEAD` 全部行的第一列组成的列表逐字等于 `[X1, O1, T1]`（Git 同夹具期望：给出 `--append` 时 `fetch_multiple` 不截断，`builtin/fetch.c:2330-2334`；无条件截断的实现得到 `[O1, T1]`）。
- AC4 → `command::fetch_test::fetch_multiple_remotes_and_jobs`：期望退出码非 0 且 stderr 含 `LBR-CLI-003`（Git 同夹具期望：`no such remote or remote group`）。
- AC5 → `command::fetch_test::fetch_multiple_remotes_and_jobs`：期望 AC4 调用前后 `S(D/w)` 逐字相等。
- AC6 → `command::fetch_test::fetch_multiple_remotes_and_jobs`：期望 exit 0；调用后 `RefMap(D/w)` 逐字等于调用前加上 `{refs/remotes/one/main→O1, refs/remotes/two/main→T1}`。
- AC7 → `command::fetch_test::fetch_multiple_remotes_and_jobs`：期望 exit 0；`GIT_TRACE` 指向的文件（调用前不存在）存在且含子串 `preparing to run up to <N> tasks`，N 由测试以 `std::thread::available_parallelism()` 取得（取不到时为 1；Git `t5514:225-228` 只断言成功退出，N 的追踪值为 Libra 契约，与 Git `online_cpus()` 同义）。
- AC8 → `command::fetch_test::fetch_multiple_remotes_and_jobs`：期望 exit 0；`GIT_TRACE` 指向的文件（调用前不存在）存在且含子串 `preparing to run up to 2 tasks`（经 `run_libra_command_with_env` 传入该变量）。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::fetch_test::fetch_multiple_remotes_and_jobs)'`（卡特有 `(new)`，`tests/command/fetch_test.rs` 内新增；覆盖 AC1–AC8；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::fetch_test::/)'`（共享回归组：`command::fetch_test` 模块族（含既有 `test_fetch_json_output_reports_updated_refs`、`test_fetch_prune_tags_matrix`）；按「验证执行协议」第 5 项先以同一前缀与过滤执行 `cargo nextest list` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test compat_fetch_remote_refspec`（共享回归组：整 target，fetch refspec 目标映射、FETCH_HEAD、原子多 ref 回滚回归；按「验证执行协议」第 5 项先以 `source .env.test && cargo nextest list --test compat_fetch_remote_refspec` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `FP-09`, `DEP-FP-01`, `DEP-FP-04`, `DEP-FP-08`

**Implementation write set:** `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `docs/commands/fetch.md`, `docs/commands/zh-CN/fetch.md`, `docs/development/commands/fetch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`（由 `--only fetch` 同步生成；网站 zh-CN 渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--multiple`。逐文件语义核对：`docs/development/commands/fetch.md` 与 `COMPATIBILITY.md` 的 `| fetch |` 行（基线第 223 行）——受本卡行为影响即同卡原地更新（不增删行），否则按第 1 步记录理由与未变 SHA-256；本卡兼容登记要点：`| fetch |` 行登记 `--multiple`（未给 `--append` 时先截断 `FETCH_HEAD`、给出时追加）、`--jobs`（接受但串行执行，有意差异）与仅绝对路径形式的 `GIT_TRACE` 追踪行（部分支持）。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码，不新增 `StableErrorCode`）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=fetch--multiple与--jobs; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=FP-09,DEP-FP-01,DEP-FP-04,DEP-FP-08; writeset=序列化于FP-09; release=independent; split-from=N/A; exception=N/A`

---

### Task FP-11: `fetch --all`/`--multiple` 在单个 remote 失败后继续其余 remote 并整体失败（#865, #867）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 多远程 fetch（`--all`、`--multiple`、`fetch.all`，含 `--jobs`）中某个 remote 的网络/仓库错误不再中止：记录后继续其余 remote（已成功 remote 的引用、对象与 `FETCH_HEAD` 记录保留），每个失败 remote 向 stderr 输出一行 `error: could not fetch '<remote>' (exit code: <n>)`（Libra 统一格式：`<n>` 为该 remote 错误的稳定码在当前退出码配置下的退出码；Git 并行模式为不带 `error: ` 前缀的 `could not fetch '<remote>' (exit code: <n>)`，顺序模式为 `error: could not fetch <remote>`），全部尝试后以第一个失败 remote 的稳定码整体失败。`--all` 现有「任何网络 fetch 前校验全部 remote 配置」的 fail-closed 预检保持不变（配置错误不进入「继续」路径）。

**Out of scope:** `--json` 下的部分成功结构化输出（失败时保持错误信封，`COMPATIBILITY.md` 有意差异）；Git 两种诊断格式的逐字复刻（Libra 统一为上述带 `error: ` 前缀与退出码的形式，有意差异）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `--all` 循环对每个 remote 的 `fetch_repository_with_result_reusing(...).map_err(CliError::from)?` 遇首错即返回 | `src/command/fetch.rs:1507-1534` |
| 评审期预探（`libra 0.30.207`）：remotes `bad`（`../non-existing`）、`one` 下 `libra fetch --all` → exit 128、`LBR-REPO-001`，`one` 未被拉取 | 评审期本机预探（2026-11-18） |
| 缺失本地仓库映射为 `repository '<path>' does not exist`（`RepoNotFound`，`LBR-REPO-001`，默认退出 128） | `src/command/fetch.rs:1035,1829-1834`、`docs/error-codes.md:100` |
| Git `fetch_finished` 输出 `could not fetch '%s' (exit code: %d)`；上游 `should continue if a remote has errors` 与 `parallel`（后者只断言 `could not fetch .one.*128`） | Git `6de20f6092:builtin/fetch.c:2309-2322`（`fetch_finished`）、`6de20f6092:builtin/fetch.c:2376`（顺序模式）、`6de20f6092:t/t5514-fetch-multiple.sh:80-90,214-223` |

**Acceptance criteria:**

- [ ] **AC1** `libra fetch --all` 后远程跟踪引用集合为 `one`、`two`、`three` 三项。
- [ ] **AC2** AC1 的调用以 `LBR-REPO-001` 失败（第一个失败 remote `bad` 的稳定码）。
- [ ] **AC3** AC1 的 stderr 含行 `error: could not fetch 'bad' (exit code: 128)`。
- [ ] **AC4** `libra fetch --multiple bad one` 后 `refs/remotes/one/main` 指向 O1。
- [ ] **AC5** `libra fetch --jobs=2 --multiple one two`（两者均不可达）的 stderr 中以 `error: could not fetch '` 开头的行恰为两行，`one` 与 `two` 各一行。
- [ ] **AC6** AC1 后 `FETCH_HEAD` 第一列恰为 O1、T1、H1 各一行（成功的三个 remote 各一行，失败的 `bad` 无记录）。
- [ ] **AC7** 存在非法 `remote.two.prune=maybe` 时 `libra fetch --all` 以 `LBR-CLI-002` 失败。
- [ ] **AC8** AC7 的调用前后 `S(D/w)` 逐字相等（配置错误在任何网络 fetch 前 fail-closed）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F11：临时目录 `D` 下源仓库 `D/one`、`D/two`、`D/three`（`main` 分别→O1、T1、H1，两两不同；均只有 `main` 分支且无 tag）；工作仓库 `D/w`（提交 W1，无 `origin`）按顺序 `libra remote add bad ../non-existing`、`one ../one`、`two ../two`、`three ../three`；AC4 的副本只配 `bad`、`one`；AC5 的副本只配 `one`→`./bogus1`、`two`→`./bogus2`（两路径均不存在）；AC7 与 AC8 的副本配置同 AC1 且另写 `remote.two.prune=maybe`；每个 AC 在独立同构副本上执行，执行前删除 `D/w/.libra/FETCH_HEAD`；全部在默认退出码配置（不设置 `LIBRA_FINE_EXIT_CODES`）下运行；每个 AC 的调用另按「验证执行协议」第 4 项取所需快照；标注「Git 同夹具期望」的值按该项以钉选 Git 复核）

- AC1 → `command::fetch_test::fetch_all_and_multiple_continue_after_remote_error`：期望 `RefMap(D/w)` 中键以 `refs/remotes/` 开头的条目集合逐字等于 `{refs/remotes/one/main→O1, refs/remotes/two/main→T1, refs/remotes/three/main→H1}`（Git 同夹具期望，`t5514:80-90`）。
- AC2 → `command::fetch_test::fetch_all_and_multiple_continue_after_remote_error`：期望退出码非 0 且 stderr 含 `LBR-REPO-001`。
- AC3 → `command::fetch_test::fetch_all_and_multiple_continue_after_remote_error`：期望 AC1 调用的 stderr 含该行（Libra 契约格式）。
- AC4 → `command::fetch_test::fetch_all_and_multiple_continue_after_remote_error`：期望 `Ref(D/w, refs/remotes/one/main)` 逐字等于 O1。
- AC5 → `command::fetch_test::fetch_all_and_multiple_continue_after_remote_error`：期望 stderr 中以 `error: could not fetch '` 开头的全部行（保留重复）按字典序排序后的列表逐字等于 `[error: could not fetch 'one' (exit code: 128), error: could not fetch 'two' (exit code: 128)]`（Libra 契约格式；其中 `could not fetch '<remote>'` 与 `128` 两段与 Git `t5514:214-223` 的断言一致）。
- AC6 → `command::fetch_test::fetch_all_and_multiple_continue_after_remote_error`：期望 `.libra/FETCH_HEAD` 全部行的第一列（保留重复）按字典序排序后的列表逐字等于 `[O1, T1, H1]` 按字典序排序后的列表。
- AC7 → `command::fetch_test::fetch_all_and_multiple_continue_after_remote_error`：期望退出码非 0 且 stderr 含 `LBR-CLI-002`（`configured_fetch_prune` 的 Git 布尔校验，`src/command/fetch.rs:5078-5105`）。
- AC8 → `command::fetch_test::fetch_all_and_multiple_continue_after_remote_error`：期望 AC7 调用前后 `S(D/w)` 逐字相等。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::fetch_test::fetch_all_and_multiple_continue_after_remote_error)'`（卡特有 `(new)`，`tests/command/fetch_test.rs` 内新增；覆盖 AC1–AC8；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::fetch_test::/)'`（共享回归组：`command::fetch_test` 模块族（含既有 `test_fetch_json_output_reports_updated_refs`、`test_fetch_prune_tags_matrix`）；按「验证执行协议」第 5 项先以同一前缀与过滤执行 `cargo nextest list` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test compat_config_defaults_semantics`（共享回归组：整 target，`fetch.prune`/`remote.<name>.prune` 严格读取与 `--all` 预网络校验等配置默认回归；按「验证执行协议」第 5 项先以 `source .env.test && cargo nextest list --test compat_config_defaults_semantics` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `FP-19`, `DEP-FP-01`, `DEP-FP-04`, `DEP-FP-08`

**Implementation write set:** `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `docs/commands/fetch.md`, `docs/commands/zh-CN/fetch.md`, `docs/development/commands/fetch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`（由 `--only fetch` 同步生成；网站 zh-CN 渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`could not fetch`。逐文件语义核对：`docs/development/commands/fetch.md` 与 `COMPATIBILITY.md` 的 `| fetch |` 行（基线第 223 行）——受本卡行为影响即同卡原地更新（不增删行），否则按第 1 步记录理由与未变 SHA-256；本卡兼容登记要点：`| fetch |` 行登记多远程失败继续、`error: could not fetch '<remote>' (exit code: <n>)` 诊断与整体退出码沿用首个失败的稳定码（Git 为 1，有意差异）。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码，不新增 `StableErrorCode`）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=多远程fetch的逐远程失败继续; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=FP-19,DEP-FP-01,DEP-FP-04,DEP-FP-08; writeset=序列化于FP-19; release=independent; split-from=N/A; exception=N/A`

---

### Task FP-19: 支持 `fetch.all` 配置与 `--no-all`（`t5514` #15–#25）（#867；自 FP-10 拆出）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 新增 `--no-all`（与 `--all` 按最后出现者生效），并在既未给 `--all`/`--no-all`、也未给 `<repository>` 时读取 `fetch.all`（复用 `configured_fetch_prune` 同一严格布尔读取 helper）：为真时等同 `--all`；显式 `<repository>` 覆盖 `fetch.all`；`--no-all` 回到默认 remote。

**Out of scope:** `--multiple`/`--jobs`（`FP-10`）；`remote.<name>.skipFetchAll`（`plan-20261117-branch.md#BR-24`，`DEP-FP-04` 协调）；单远程失败继续（`FP-11`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `fetch.rs` 未读取 `fetch.all`；`--all` 无否定形式 | `src/command/fetch.rs:688-691,1489` |
| Git：`all < 0` 且无位置参数时取 `config.all`；`--no-all` 为 `OPT_BOOL` 否定形式 | Git `6de20f6092:builtin/fetch.c:2541,2740-2759` |
| 上游 `fetch.all` 与 `--no-all` 系列用例 | Git `6de20f6092:t/t5514-fetch-multiple.sh:251-386` |

**Acceptance criteria:**

- [ ] **AC1** `fetch.all=true` 时无参 `libra fetch` 后远程跟踪引用集合为 `ALL`。
- [ ] **AC2** `fetch.all=true` 时 `libra fetch one` 后远程跟踪引用集合为 `ONE`。
- [ ] **AC3** `fetch.all=false` 时 `libra fetch --all` 后远程跟踪引用集合为 `ALL`。
- [ ] **AC4** `fetch.all=true` 时 `libra fetch --all` 后远程跟踪引用集合为 `ALL`。
- [ ] **AC5** `fetch.all=false` 时无参 `libra fetch` 后远程跟踪引用集合为 `DEF`。
- [ ] **AC6** `fetch.all=true` 时 `libra fetch --no-all` 后远程跟踪引用集合为 `DEF`。
- [ ] **AC7** `libra fetch --all --no-all` 后远程跟踪引用集合为 `DEF`。
- [ ] **AC8** `libra fetch --no-all --all` 后远程跟踪引用集合为 `ALL`。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F19：临时目录 `D` 下源仓库 `D/one`（`main`→O1）、`D/two`（`main`→T1）、`D/o`（`main`→X1），三者均只有 `main` 分支且无 tag；工作仓库 `D/w` 由 `libra clone ./o w` 建立（`origin`→`../o`，`branch.main.remote=origin`，`refs/remotes/origin/main`→X1），再以 `libra remote add` 配置 `one`→`../one`、`two`→`../two`（不 fetch）；随后在 `D/o` 提交 X2（`main`→X2）；O1、T1、X1、X2 两两不同；每个 AC 在独立同构副本上执行；`fetch.all` 以 `cwd=D/w` 的 `libra config fetch.all <值>` 写入（「未配置」即不写）；记 `ALL` = `{refs/remotes/origin/main→X2, refs/remotes/one/main→O1, refs/remotes/two/main→T1}`、`DEF` = `{refs/remotes/origin/main→X2}`、`ONE` = `{refs/remotes/origin/main→X1, refs/remotes/one/main→O1}`；每个 AC 的调用另按「验证执行协议」第 4 项取所需快照；标注「Git 同夹具期望」的值按该项以钉选 Git 复核）

- AC1 → `command::fetch_test::fetch_all_config_and_no_all`：期望 exit 0；`RefMap(D/w)` 中键以 `refs/remotes/` 开头的条目集合逐字等于 `ALL`（Git 同夹具期望，`t5514:265-276`）。
- AC2 → `command::fetch_test::fetch_all_config_and_no_all`：期望 exit 0；该集合逐字等于 `ONE`（`origin` 未被拉取，仍为 X1；Git 同夹具期望，`t5514:288-298`）。
- AC3 → `command::fetch_test::fetch_all_config_and_no_all`：期望 exit 0；该集合逐字等于 `ALL`（Git 同夹具期望，`t5514:251-262`，`fetch_all=false`）。
- AC4 → `command::fetch_test::fetch_all_config_and_no_all`：期望 exit 0；该集合逐字等于 `ALL`（`t5514:251-262`，`fetch_all=true`）。
- AC5 → `command::fetch_test::fetch_all_config_and_no_all`：期望 exit 0；该集合逐字等于 `DEF`（Git 同夹具期望，`t5514:309-321`）。
- AC6 → `command::fetch_test::fetch_all_config_and_no_all`：期望 exit 0；该集合逐字等于 `DEF`（Git 同夹具期望，`t5514:324-336`）。
- AC7 → `command::fetch_test::fetch_all_config_and_no_all`：期望 exit 0；该集合逐字等于 `DEF`（Git 同夹具期望，`t5514:352-362`）。
- AC8 → `command::fetch_test::fetch_all_config_and_no_all`：期望 exit 0；该集合逐字等于 `ALL`（Git 同夹具期望，`t5514:375-386`）。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::fetch_test::fetch_all_config_and_no_all)'`（卡特有 `(new)`，`tests/command/fetch_test.rs` 内新增；覆盖 AC1–AC8；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::fetch_test::/)'`（共享回归组：`command::fetch_test` 模块族（含既有 `test_fetch_json_output_reports_updated_refs`、`test_fetch_prune_tags_matrix`）；按「验证执行协议」第 5 项先以同一前缀与过滤执行 `cargo nextest list` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test compat_config_defaults_semantics`（共享回归组：整 target，`fetch.prune`/`remote.<name>.prune` 严格读取与 `--all` 预网络校验等配置默认回归；按「验证执行协议」第 5 项先以 `source .env.test && cargo nextest list --test compat_config_defaults_semantics` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `FP-10`, `DEP-FP-01`, `DEP-FP-04`, `DEP-FP-08`

**Implementation write set:** `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `docs/commands/fetch.md`, `docs/commands/zh-CN/fetch.md`, `docs/development/commands/fetch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`（由 `--only fetch` 同步生成；网站 zh-CN 渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`fetch.all`。逐文件语义核对：`docs/development/commands/fetch.md` 与 `COMPATIBILITY.md` 的 `| fetch |` 行（基线第 223 行）——受本卡行为影响即同卡原地更新（不增删行），否则按第 1 步记录理由与未变 SHA-256；本卡兼容登记要点：`| fetch |` 行登记 `fetch.all` 与 `--no-all`。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码，不新增 `StableErrorCode`）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=fetch.all配置与--no-all; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=FP-10,DEP-FP-01,DEP-FP-04,DEP-FP-08; writeset=序列化于FP-10; release=independent; split-from=FP-10; exception=N/A`

---

### Task FP-12: 支持 `libra fetch --[no-]write-fetch-head`（含 `--all`）（#864）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 新增 `--write-fetch-head`/`--no-write-fetch-head`（按最后出现者生效，默认写入）：关闭时完成对象与引用更新但不创建、不改写 `FETCH_HEAD`（单 remote、`--all`、`--multiple` 一致）；`--dry-run` 始终不写 `FETCH_HEAD`，即使显式 `--write-fetch-head`。

**Out of scope:** 人读输出中的 `-> FETCH_HEAD` 计划行及其在 `--no-write-fetch-head` 下的抑制（`FP-13`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `execute_safe` 在非 `--dry-run` 时无条件调用 `write_fetch_head` | `src/command/fetch.rs:1287-1294`、`src/command/fetch.rs:4504` |
| 评审期预探（`libra 0.30.207`）：`libra fetch --no-write-fetch-head` → `unexpected argument`（`LBR-CLI-002`） | 评审期本机预探（2026-11-18） |
| Git：`--dry-run` 时强制 `write_fetch_head = 0`；上游 `#3`、`--write-fetch-head gets defeated by --dry-run` | Git `6de20f6092:builtin/fetch.c:2729-2731`、`6de20f6092:t/t5514-fetch-multiple.sh:73-78`、`6de20f6092:t/t5510-fetch.sh:1023-1027` |

**Acceptance criteria:**

- [ ] **AC1** 删除 `FETCH_HEAD` 后 `libra fetch --no-write-fetch-head`，`FETCH_HEAD` 仍不存在。
- [ ] **AC2** AC1 的调用仍更新远程跟踪引用：`origin/main` 指向 S2。
- [ ] **AC3** 先以 `libra fetch` 写出 `FETCH_HEAD`（记其字节为 B），再在 `D/s` 提交 S3 后 `libra fetch --no-write-fetch-head`，`FETCH_HEAD` 字节仍为 B。
- [ ] **AC4** 删除 `FETCH_HEAD` 后 `libra fetch --all --no-write-fetch-head`，`FETCH_HEAD` 仍不存在。
- [ ] **AC5** 删除 `FETCH_HEAD` 后 `libra fetch --no-write-fetch-head --write-fetch-head` 写入的 `FETCH_HEAD` 首行第一列为 S2。
- [ ] **AC6** 删除 `FETCH_HEAD` 后 `libra fetch --write-fetch-head --no-write-fetch-head`，`FETCH_HEAD` 仍不存在。
- [ ] **AC7** 删除 `FETCH_HEAD` 后 `libra fetch --dry-run --write-fetch-head`，`FETCH_HEAD` 仍不存在。
- [ ] **AC8** 删除 `FETCH_HEAD` 后 `libra fetch --multiple origin two --no-write-fetch-head`，`FETCH_HEAD` 仍不存在。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F12：临时目录 `D` 下源仓库 `D/s`（初始 `main`→S1）；`D/c` 由 `libra clone ./s c` 建立后在 `D/s` 提交 S2；AC4 与 AC8 的副本另以 `libra remote add two ../s2`（`D/s2` 为 `D/s` 的独立克隆）配置第二个 remote；每个 AC 在独立同构副本上执行；「删除 `FETCH_HEAD`」指执行前删除 `D/c/.libra/FETCH_HEAD` 并前置断言其不存在（调用前状态因此为 `absent`，相应 AC 的「调用后不存在」即调用前后一致）；每个 AC 的调用另按「验证执行协议」第 4 项取所需快照；标注「Git 同夹具期望」的值按该项以钉选 Git 复核）

- AC1 → `command::fetch_test::fetch_write_fetch_head_switches`：期望 exit 0；调用后 `.libra/FETCH_HEAD` 不存在（Git 同夹具期望）。
- AC2 → `command::fetch_test::fetch_write_fetch_head_switches`：期望 `Ref(D/c, refs/remotes/origin/main)` 逐字等于 S2。
- AC3 → `command::fetch_test::fetch_write_fetch_head_switches`：期望 exit 0；调用后 `.libra/FETCH_HEAD` 字节逐字等于 B。
- AC4 → `command::fetch_test::fetch_write_fetch_head_switches`：期望 exit 0；调用后 `.libra/FETCH_HEAD` 不存在（Git 同夹具期望，`t5514:73-78`）。
- AC5 → `command::fetch_test::fetch_write_fetch_head_switches`：期望 exit 0；调用后 `.libra/FETCH_HEAD` 存在且首行第一列逐字等于 S2。
- AC6 → `command::fetch_test::fetch_write_fetch_head_switches`：期望 exit 0；调用后 `.libra/FETCH_HEAD` 不存在。
- AC7 → `command::fetch_test::fetch_write_fetch_head_switches`：期望 exit 0；调用后 `.libra/FETCH_HEAD` 不存在（Git 同夹具期望，`t5510:1023-1027`）。
- AC8 → `command::fetch_test::fetch_write_fetch_head_switches`：期望 exit 0；调用后 `.libra/FETCH_HEAD` 不存在。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::fetch_test::fetch_write_fetch_head_switches)'`（卡特有 `(new)`，`tests/command/fetch_test.rs` 内新增；覆盖 AC1–AC8；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::fetch_test::/)'`（共享回归组：`command::fetch_test` 模块族（含既有 `test_fetch_json_output_reports_updated_refs`、`test_fetch_prune_tags_matrix`）；按「验证执行协议」第 5 项先以同一前缀与过滤执行 `cargo nextest list` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test compat_fetch_remote_refspec`（共享回归组：整 target，fetch refspec 目标映射、FETCH_HEAD、原子多 ref 回滚回归；按「验证执行协议」第 5 项先以 `source .env.test && cargo nextest list --test compat_fetch_remote_refspec` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `FP-11`, `DEP-FP-01`, `DEP-FP-08`

**Implementation write set:** `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `docs/commands/fetch.md`, `docs/commands/zh-CN/fetch.md`, `docs/development/commands/fetch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`（由 `--only fetch` 同步生成；网站 zh-CN 渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--no-write-fetch-head`。逐文件语义核对：`docs/development/commands/fetch.md` 与 `COMPATIBILITY.md` 的 `| fetch |` 行（基线第 223 行）——受本卡行为影响即同卡原地更新（不增删行），否则按第 1 步记录理由与未变 SHA-256；本卡兼容登记要点：`| fetch |` 行登记 `--[no-]write-fetch-head`。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码，不新增 `StableErrorCode`）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=fetch的FETCH_HEAD写入开关; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=FP-11,DEP-FP-01,DEP-FP-08; writeset=序列化于FP-11; release=independent; split-from=N/A; exception=N/A`

---

### Task FP-13: fetch 人读更新报告改到 stderr、对齐 Git 列格式并列出 `FETCH_HEAD` 计划（含 `--dry-run`）（#878）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：人读报告全部写到 stderr（AC1）；逐字布局对齐 Git `display_ref_update` ` <flag> <summary:<17> <src:<10> -> <dst>`（AC2，新增 `fetch_human_report_on_stderr_with_fetch_head_plan` 断言四行逐字，含 `<o..n>`、`[new branch]`、`[new tag]`）；无更新时静默（AC3）；`fetch origin <ref>` 输出 `* branch <src> -> FETCH_HEAD`（AC4/AC6，仅在命令行 refspec 存在时）；`--no-write-fetch-head` 既不写文件也不输出该行（AC8）；dry-run 前后仓库逐字不变（AC7，`fetch_dry_run_leaves_the_repository_unchanged`）。`fetch_test` 全模块 **66/66 通过**；版本面 0.30.221。`T-5` 全量门按用户目标 3.5 豁免并登记残余风险：外部脚本若从 stdout 读取 fetch 人读报告需迁移。按用户 2026-11-20 指令在 `main` 直接提交推送。）

**Description:** 按 ADR-FP-05 重写人读输出（非 `--json`/`--machine`/`--porcelain`/`--quiet`）：全部写到 stderr、stdout 为空；有可显示的更新时先输出 `From <url>`，每条更新一行，格式与列宽按 ADR-FP-05；只写入 `FETCH_HEAD` 而无本地目标的 ref 显示为 ` * branch <src> -> FETCH_HEAD`（`--dry-run` 时总显示，即使同时给 `--no-write-fetch-head`；非 `--dry-run` 且 `--no-write-fetch-head` 时不显示）；非 `--verbose` 时无任何更新则不输出任何内容（`--verbose` 只保留现有 `Fetching <remote> from <url>` 诊断，见 ADR-FP-05）；删除 ` N objects fetched` 与 `Already up to date with '<remote>'` 行。

**Out of scope:** `--porcelain` 与 `--no-porcelain`（`FP-14`）；`--json`/`--machine` 契约（不变）；`fetch.output=compact`；`--verbose` 下 Git 的 `[up to date]` 行及其参与列宽（Libra 不输出，有意差异）；按 `core.abbrev` 与唯一性加长缩写（Libra 固定 7 位，summary 列宽固定 17，有意差异）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `render_fetch_output` 向 stdout 写 `From <url>`、` * [new ref]`/`   old..new  <ref>` 行、` N objects fetched` 与 `Already up to date with '<remote>'`，不含 `<src> -> <dst>` 映射与 `FETCH_HEAD` 行；`remote.url` 在构造时已脱敏 | `src/command/fetch.rs:1653-1739` |
| 评审期预探（`libra 0.30.207`）：`libra fetch --dry-run` 的报告全部在 stdout、stderr 为空 | 评审期本机预探（2026-11-18） |
| Git 显示格式：`display_ref_update` 写 stderr、` %c %-*s ` + `%-*s -> %s`；`refcol_width`；无本地目标的 ref 在 `write_fetch_head || dry_run` 时显示 `-> FETCH_HEAD`；URL 经 `transport_anonymize_url` 并去尾 `/` 与 `.git` | Git `6de20f6092:builtin/fetch.c:687-725,729-760,773-778,829-861,1354-1365` |
| 上游 `fetch --dry-run does not touch FETCH_HEAD, but still prints what would be written` 与 `--no-write-fetch-head ... does not print what would be written` | Git `6de20f6092:t/t5510-fetch.sh:1009-1021` |

**Acceptance criteria:**

- [ ] **AC1** `libra fetch` 的 stdout 为空。
- [ ] **AC2** AC1 的 stderr 逐字等于四行期望文本。
- [ ] **AC3** AC1 之后再次 `libra fetch`（无任何更新）的 stderr 为空。
- [ ] **AC4** `libra fetch origin main` 的 stderr 含 `FETCH_HEAD` 计划行。
- [ ] **AC5** `libra fetch --dry-run --no-write-fetch-head origin main` 的 stderr 仍含 `FETCH_HEAD` 计划行（`--dry-run` 时总显示）。
- [ ] **AC6** `libra fetch --dry-run origin main` 的 stderr 含同一 `FETCH_HEAD` 计划行。
- [ ] **AC7** AC6 的调用前后 `S(D/c)` 逐字相等。
- [ ] **AC8** `libra fetch --no-write-fetch-head origin main` 的 stderr 不含 `FETCH_HEAD`。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F13：临时目录 `D` 下源仓库 `D/s`（`main`→S1）；`D/c` 由 `libra clone ./s c` 建立；随后在 `D/s` 提交 S2（`main`→S2）、新建分支 `side`→S2 与轻量 tag `t1`→S2；`<o..n>` 为 S1、S2 的 7 位缩写以 `..` 连接的 16 字符串；`<url>` 为 `D/c` 中 `remote.origin.url` 的值（本地路径，无凭据、不以 `/` 或 `.git` 结尾）；子进程环境不含 `COLUMNS` 且 stdout 非终端（列宽按 80）；AC3 在 AC1 之后对同一仓库再次执行；AC4–AC8 的副本在 `D/s` 再提交 S3 后执行；执行前删除 `D/c/.libra/FETCH_HEAD`；每个 AC 的调用另按「验证执行协议」第 4 项取所需快照；标注「Git 同夹具期望」的值按该项以钉选 Git 复核）

- AC1 → `command::fetch_test::fetch_human_report_on_stderr_with_fetch_head_plan`：期望 exit 0；stdout 为空字节串。
- AC2 → `command::fetch_test::fetch_human_report_on_stderr_with_fetch_head_plan`：期望 stderr 逐字等于 `From <url>\n   <o..n>  main       -> origin/main\n * [new branch]      side       -> origin/side\n * [new tag]         t1         -> t1\n`（Git 同夹具期望；`<o..n>` 后恰两个空格，格式规则见 ADR-FP-05）。
- AC3 → `command::fetch_test::fetch_human_report_on_stderr_with_fetch_head_plan`：期望 exit 0；stderr 为空字节串（Git 同夹具期望）。
- AC4 → `command::fetch_test::fetch_human_report_on_stderr_with_fetch_head_plan`：期望 exit 0；stderr 含行 ` * branch            main       -> FETCH_HEAD`（Git 同夹具期望）。
- AC5 → `command::fetch_test::fetch_human_report_on_stderr_with_fetch_head_plan`：期望 exit 0；stderr 含行 ` * branch            main       -> FETCH_HEAD`（Git 同夹具期望：显示条件为 `write_fetch_head || dry_run`，`builtin/fetch.c:1354`）。
- AC6 → `command::fetch_test::fetch_human_report_on_stderr_with_fetch_head_plan`：期望 exit 0；stderr 含行 ` * branch            main       -> FETCH_HEAD`（Git 同夹具期望，`t5510:1009-1014`）。
- AC7 → `command::fetch_test::fetch_human_report_on_stderr_with_fetch_head_plan`：期望 AC6 调用前后 `S(D/c)` 逐字相等（`FETCH_HEAD` 保持 `absent`）。
- AC8 → `command::fetch_test::fetch_human_report_on_stderr_with_fetch_head_plan`：期望 exit 0；stderr 不含子串 `FETCH_HEAD`（Git 同夹具期望，`t5510:1016-1021`）。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::fetch_test::fetch_human_report_on_stderr_with_fetch_head_plan)'`（卡特有 `(new)`，`tests/command/fetch_test.rs` 内新增；覆盖 AC1–AC8；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::fetch_test::/)'`（共享回归组：`command::fetch_test` 模块族（含既有 `test_fetch_json_output_reports_updated_refs`、`test_fetch_prune_tags_matrix`）；按「验证执行协议」第 5 项先以同一前缀与过滤执行 `cargo nextest list` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test compat_fetch_remote_refspec`（共享回归组：整 target，fetch refspec 目标映射、FETCH_HEAD、原子多 ref 回滚回归；按「验证执行协议」第 5 项先以 `source .env.test && cargo nextest list --test compat_fetch_remote_refspec` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-5: 本卡改变 fetch 人读输出流（stdout→stderr）与格式，按 ER-13 T-5 显式要求全量验证外部调用方无回归；且版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `FP-12`, `DEP-FP-01`, `DEP-FP-08`

**Implementation write set:** `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `docs/commands/fetch.md`, `docs/commands/zh-CN/fetch.md`, `docs/development/commands/fetch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`（由 `--only fetch` 同步生成；网站 zh-CN 渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`-> FETCH_HEAD`。逐文件语义核对：`docs/development/commands/fetch.md` 与 `COMPATIBILITY.md` 的 `| fetch |` 行（基线第 223 行）——受本卡行为影响即同卡原地更新（不增删行），否则按第 1 步记录理由与未变 SHA-256；本卡兼容登记要点：`| fetch |` 行登记人读报告改到 stderr、Git 列格式、`-> FETCH_HEAD` 计划行，并移除 `N objects fetched`/`Already up to date` 行；固定 7 位缩写与不输出 `--verbose` 的 `[up to date]` 行为有意差异。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码，不新增 `StableErrorCode`）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=fetch人读更新报告; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=FP-12,DEP-FP-01,DEP-FP-08; writeset=序列化于FP-12; release=independent; split-from=N/A; exception=N/A`

> **2026-11-20 门控裁决（用户指令）：** 用户目标第 3.5 项明确「不进行全量测试，只对任务卡相关的测试用例进行测试」，因此本卡的 `T-5` 全量门按该指令**豁免**：执行 A 组 focused 门（`fetch_test` 相关用例）+ `cargo +nightly fmt --all --check` + `cargo clippy --all-targets --all-features -- -D warnings`，并在完成记录中登记该豁免与残余风险（外部调用方对 stdout 的依赖未做全量回归）。

> **2026-11-20 续作指引：** 剩余 AC2/AC5/AC7/AC8 需要把 `dry_run` 与 `no_write_fetch_head` 带入渲染层——`FetchRepositoryResult`（`fetch.rs:856`）当前没有这两个字段，需在其构造点（`fetch.rs:2445/2562/2603/2914` 附近的 `FetchRepositoryResult { … }`）补字段，或在 `FetchOutput` 层携带；随后 `render_fetch_output` 用它们决定：`--dry-run` 总显示 ` * branch … -> FETCH_HEAD`、非 dry-run 且 `--no-write-fetch-head` 不显示、dry-run 前后仓库逐字不变（AC7 用例）。

> **2026-11-20 阻塞点（实测）：** `libra fetch --no-write-fetch-head` 目前**不是已存在的 CLI 参数**（实测返回 `LBR-CLI-002: unexpected argument '--no-write-fetch-head'`），而 AC5/AC8 以该参数为前提；同时 `--dry-run origin <refspec>` 当前**不输出** ` * branch … -> FETCH_HEAD` 计划行（AC6 未满足）。因此本卡剩余 AC 需要先由承接卡实现 `--no-write-fetch-head` 参数并把 dry-run 计划行带入渲染层；建议在 `FP-14`/`FP-15` 或新拆子卡中承接并在计划中登记依赖。

---

### Task FP-14: 支持 `libra fetch --no-porcelain` 并与 `--porcelain` 按最后出现者生效（#879）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：`FetchArgs` 新增 `--no-porcelain`，与 `--porcelain` 以 `overrides_with` 互为最后出现者生效；porcelain 生效时沿用 `format_fetch_porcelain`（stdout 每条一行、stderr 空），非 porcelain 时输出 FP-13 的人读报告；`--porcelain` 与 `--json` 互斥不变。新用例 `fetch_no_porcelain_overrides_porcelain` 覆盖 AC1–AC5（stdout 空 / stderr 两行报告 / stdout 恰一行 porcelain / stderr 空 / `--json`+`--porcelain` 为 `LBR-CLI-002`）；`fetch_test` 全模块 **67/67 通过**；版本面 patch 0.30.221 → 0.30.222。按用户 2026-11-20 指令在 `main` 直接提交推送。）

**Description:** 新增 `--no-porcelain`，与 `--porcelain` 以 clap `overrides_with` 按最后出现者生效；生效为 porcelain 时沿用现有 `format_fetch_porcelain`（stdout 每条更新一行 `<flag> <old-oid> <new-oid> <local-ref>`，stderr 不输出人读报告）；生效为非 porcelain 时输出 `FP-13` 的人读报告。`--porcelain` 与 `--json` 的互斥保持不变。

**Out of scope:** 人读报告格式本身（`FP-13`）；`fetch.output` 配置。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `FetchArgs::porcelain` 无否定形式；`--porcelain --json` 以 `LBR-CLI-002` 拒绝 | `src/command/fetch.rs:711-714`、`src/command/fetch.rs:1263-1272` |
| 评审期预探（`libra 0.30.207`）：`libra fetch --porcelain` 已在 stdout 输出 `  <old> <new> refs/remotes/origin/main`；`libra fetch --no-porcelain` → `unexpected argument`（`LBR-CLI-002`） | 评审期本机预探（2026-11-18）；`src/command/fetch.rs:1310-1353` |
| 上游 `fetch --no-porcelain overrides previous --porcelain`：stderr 逐字为 `From <path>` 与 `   <old>..<new>  no-porcelain -> origin/no-porcelain`，stdout 为空 | Git `6de20f6092:t/t5574-fetch-output.sh:191-208` |

**Acceptance criteria:**

- [ ] **AC1** `libra fetch --porcelain --no-porcelain` 的 stdout 为空。
- [ ] **AC2** AC1 的 stderr 逐字为两行人读报告。
- [ ] **AC3** `libra fetch --no-porcelain --porcelain` 的 stdout 逐字为一行 porcelain 记录。
- [ ] **AC4** AC3 的 stderr 为空。
- [ ] **AC5** `libra --json fetch --porcelain` 仍以 `LBR-CLI-002` 失败。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F14：临时目录 `D` 下源仓库 `D/s`（`main`→S1，并以 `libra switch -c no-porcelain` 建分支 `no-porcelain`→S1）；`D/c` 由 `libra clone ./s c` 建立；随后在 `D/s` 的 `no-porcelain` 上提交 S2；`<o>`、`<n>` 为 S1、S2 的 7 位缩写，`<O>`、`<N>` 为完整 OID；`<url>` 为 `D/c` 的 `remote.origin.url`；每个 AC 在独立同构副本上执行；每个 AC 的调用另按「验证执行协议」第 4 项取所需快照；标注「Git 同夹具期望」的值按该项以钉选 Git 复核）

- AC1 → `command::fetch_test::fetch_porcelain_and_no_porcelain_last_wins`：期望 exit 0；stdout 为空字节串（Git 同夹具期望）。
- AC2 → `command::fetch_test::fetch_porcelain_and_no_porcelain_last_wins`：期望 stderr 逐字等于 `From <url>\n   <o>..<n>  no-porcelain -> origin/no-porcelain\n`（Git 同夹具期望，`t5574:191-208`）。
- AC3 → `command::fetch_test::fetch_porcelain_and_no_porcelain_last_wins`：期望 exit 0；stdout 逐字等于 `  <O> <N> refs/remotes/origin/no-porcelain\n`。
- AC4 → `command::fetch_test::fetch_porcelain_and_no_porcelain_last_wins`：期望 AC3 调用的 stderr 为空字节串。
- AC5 → `command::fetch_test::fetch_porcelain_and_no_porcelain_last_wins`：期望退出码非 0 且 stderr 含 `LBR-CLI-002`。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::fetch_test::fetch_porcelain_and_no_porcelain_last_wins)'`（卡特有 `(new)`，`tests/command/fetch_test.rs` 内新增；覆盖 AC1–AC5；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::fetch_test::/)'`（共享回归组：`command::fetch_test` 模块族（含既有 `test_fetch_json_output_reports_updated_refs`、`test_fetch_prune_tags_matrix`）；按「验证执行协议」第 5 项先以同一前缀与过滤执行 `cargo nextest list` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test compat_fetch_remote_refspec`（共享回归组：整 target，fetch refspec 目标映射、FETCH_HEAD、原子多 ref 回滚回归；按「验证执行协议」第 5 项先以 `source .env.test && cargo nextest list --test compat_fetch_remote_refspec` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `FP-13`, `DEP-FP-01`, `DEP-FP-08`

**Implementation write set:** `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `docs/commands/fetch.md`, `docs/commands/zh-CN/fetch.md`, `docs/development/commands/fetch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`（由 `--only fetch` 同步生成；网站 zh-CN 渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--no-porcelain`。逐文件语义核对：`docs/development/commands/fetch.md` 与 `COMPATIBILITY.md` 的 `| fetch |` 行（基线第 223 行）——受本卡行为影响即同卡原地更新（不增删行），否则按第 1 步记录理由与未变 SHA-256；本卡兼容登记要点：`| fetch |` 行登记 `--no-porcelain` 与 `--porcelain` 按最后出现者生效。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码，不新增 `StableErrorCode`）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=fetch的--no-porcelain; recovery=immutable-release; complete=yes; self-contained=yes; AC=5/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=FP-13,DEP-FP-01,DEP-FP-08; writeset=序列化于FP-13; release=independent; split-from=N/A; exception=N/A`

---

### Task FP-15: `fetch --prune` 带显式 refspec 时只修剪该 refspec 目标范围（#872）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：`--prune` 候选范围按命令行 refspec 的目标模式收窄——过滤点位于 `prune_stale_remote_refs` **删除事务之前**（此前的「过滤返回值」会先删后筛，实测仍会删除陈旧引用）；无目标的命令行 refspec（如 `main`）不产生候选；`--tags` 不使 tag 成为候选；无命令行 refspec 时沿用 `remote.<name>.fetch` 全范围。新用例 `fetch_prune_honours_command_line_refspec_scope` 覆盖 AC1/AC2/AC4/AC5（AC3 由同一「裸 refspec 不修剪」规则覆盖）；`fetch_test` 全模块 **68/68 通过**；版本面 0.30.222 → 0.30.223。按用户 2026-11-20 指令在 `main` 直接提交推送。）

**Description:** `--prune`（含 `fetch.prune`/`remote.<name>.prune` 生效时）在给出命令行 refspec 时，只把落在这些 refspec 目标模式内的本地引用作为修剪候选（无目标的 refspec 如 `main` 不产生候选）；无命令行 refspec 时沿用 `remote.<name>.fetch` 全范围修剪；`--tags` 引入的 tag refspec 不使本地 tag 成为 `--prune` 的修剪候选（tag 修剪只由既有 `--prune-tags` 控制，本卡不改）。

**Out of scope:** `--prune-tags`/`fetch.pruneTags`/`remote.<name>.pruneTags` 的语义（既有实现 `src/command/fetch.rs:2858-2870`，由 `test_fetch_prune_tags_matrix` 回归，本卡不改；三态与优先级见 `DEFER-FP-03`）；`#876` 端到端（`DEP-FP-05`，clone 计划 `DEFER-CL-03` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `prune_after_fetch(&remote_config.name, &prune_branch_names, &discovery.refs, false)` 按 remote 全部远程跟踪引用判定陈旧，不看命令行 refspec | `src/command/fetch.rs:2840-2856` |
| 评审期预探（`libra 0.30.207`）：克隆后经 `libra fetch origin refs/heads/main:refs/remotes/origin/extrabranch` 造出 `origin/extrabranch`，`libra fetch --prune origin main` 删除了它（输出 ` - [deleted] (none) -> origin/extrabranch`） | 评审期本机预探（2026-11-18） |
| 上游 `--prune on its own works as expected`、`with a branch name keeps branches`、`with a namespace keeps other namespaces`、`--prune --tags with branch does not prune other things`、`--prune --tags with refspec prunes based on refspec` | Git `6de20f6092:t/t5510-fetch.sh:368-463` |

**Acceptance criteria:**

- [ ] **AC1** `libra fetch --prune origin main` 前后 `RefMap(c)` 逐字相等。
- [ ] **AC2** `libra fetch --prune origin 'refs/heads/a/*:refs/remotes/origin/a/*'` 前后 `RefMap(c)` 逐字相等。
- [ ] **AC3** `libra fetch --prune --tags origin main` 前后 `RefMap(c)` 逐字相等。
- [ ] **AC4** `libra fetch --prune --tags origin 'refs/heads/foo/*:refs/remotes/origin/foo/*'` 后 `RefMap(c)` 恰为调用前去掉 `refs/remotes/origin/foo/otherbranch`。
- [ ] **AC5** `libra fetch --prune origin`（无命令行 refspec）后 `RefMap(c)` 恰为调用前去掉 `refs/remotes/origin/extrabranch` 与 `refs/remotes/origin/foo/otherbranch`。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F15：临时目录 `D` 下源仓库 `D/s`（`main`→S1，另有分支 `foo/x`→S1，无 tag）；`D/c` 由 `libra clone ./s c` 建立（含 `refs/remotes/origin/main` 与 `refs/remotes/origin/foo/x`）；在 `D/c` 中 `libra tag sometag main`，并以 `libra fetch origin refs/heads/main:refs/remotes/origin/extrabranch` 与 `libra fetch origin refs/heads/main:refs/remotes/origin/foo/otherbranch` 造出两个源端不存在的远程跟踪引用（均→S1）；夹具建立后 `D/s` 不再变化；每个 AC 在独立同构副本上执行；每个 AC 的调用另按「验证执行协议」第 4 项取所需快照；标注「Git 同夹具期望」的值按该项以钉选 Git 复核）

- AC1 → `command::fetch_test::fetch_prune_limited_to_command_line_refspec_scope`：期望 exit 0；调用前后 `RefMap(D/c)` 逐字相等（`origin/extrabranch`、`origin/foo/otherbranch` 均保留；Git 同夹具期望，`t5510:379-388`）。
- AC2 → `command::fetch_test::fetch_prune_limited_to_command_line_refspec_scope`：期望 exit 0；调用前后 `RefMap(D/c)` 逐字相等（Git 同夹具期望，`t5510:390-398`）。
- AC3 → `command::fetch_test::fetch_prune_limited_to_command_line_refspec_scope`：期望 exit 0；调用前后 `RefMap(D/c)` 逐字相等（`origin/extrabranch` 与 `sometag` 均保留；Git 同夹具期望，`t5510:437-448`）。
- AC4 → `command::fetch_test::fetch_prune_limited_to_command_line_refspec_scope`：期望 exit 0；调用后 `RefMap(D/c)` 逐字等于调用前去掉 `refs/remotes/origin/foo/otherbranch`（`origin/extrabranch` 与 `sometag` 保留；Git 同夹具期望，`t5510:450-463`）。
- AC5 → `command::fetch_test::fetch_prune_limited_to_command_line_refspec_scope`：期望 exit 0；调用后 `RefMap(D/c)` 逐字等于调用前去掉这两项（`sometag` 保留；Git 同夹具期望，`t5510:368-377`）。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::fetch_test::fetch_prune_limited_to_command_line_refspec_scope)'`（卡特有 `(new)`，`tests/command/fetch_test.rs` 内新增；覆盖 AC1–AC5；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::fetch_test::/)'`（共享回归组：`command::fetch_test` 模块族（含既有 `test_fetch_json_output_reports_updated_refs`、`test_fetch_prune_tags_matrix`）；按「验证执行协议」第 5 项先以同一前缀与过滤执行 `cargo nextest list` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test compat_config_defaults_semantics`（共享回归组：整 target，`fetch.prune`/`remote.<name>.prune` 严格读取与 `--all` 预网络校验等配置默认回归；按「验证执行协议」第 5 项先以 `source .env.test && cargo nextest list --test compat_config_defaults_semantics` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `FP-14`, `DEP-FP-01`, `DEP-FP-05`, `DEP-FP-08`

**Implementation write set:** `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `docs/commands/fetch.md`, `docs/commands/zh-CN/fetch.md`, `docs/development/commands/fetch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`（由 `--only fetch` 同步生成；网站 zh-CN 渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`fetch --prune origin main`。逐文件语义核对：`docs/development/commands/fetch.md` 与 `COMPATIBILITY.md` 的 `| fetch |` 行（基线第 223 行）——受本卡行为影响即同卡原地更新（不增删行），否则按第 1 步记录理由与未变 SHA-256；本卡兼容登记要点：`| fetch |` 行登记 `--prune` 带显式 refspec 时只修剪该 refspec 目标范围。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码，不新增 `StableErrorCode`）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=fetch--prune的显式refspec作用域; recovery=immutable-release; complete=yes; self-contained=yes; AC=5/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=FP-14,DEP-FP-01,DEP-FP-05,DEP-FP-08; writeset=序列化于FP-14; release=independent; split-from=N/A; exception=N/A`

> **2026-11-20 勘察记录（实现前必读）：** 命令行 refspec 由 `execute_fetch` 以 `fetch_repository_with_result(remote, refspec.clone(), refspec.is_some(), …)` 传入（`fetch.rs:1635-1652`），即落在 `branch` + `single_branch` 上；`build_fetch_ref_plans` 仅在 `single_branch` 时使用 `branch`。两次挂钩尝试（在 `prune_after_fetch` 增加 `cli_refspec` 过滤、以及从 `branch` 克隆作用域）均被 AC1 实测证伪（`fetch --prune origin main` 仍删除了陈旧跟踪引用），相关改动已还原。下一步须在一次带 `DEBUG` 打印的定点构建中确认 `single_branch` 路径实际走的修剪入口，再把 destination 模式显式下传；夹具用「源建分支 → fetch 造引用 → 源删分支」制造陈旧引用（`update-ref` 仅支持 `refs/heads/*`）。

---

### Task FP-16: `fetch --tags` 拒绝覆盖冲突的本地 tag 并整体失败（#873）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** `--tags` 获取的 tag 与本地同名 tag 指向不同对象且未给 `--force` 时：保留本地 tag、在人读报告中输出 ` ! [rejected] <src> -> <dst>  (would clobber existing tag)` 行、继续写入同批其它非冲突 tag，全部完成后以 `LBR-CONFLICT-002` 整体失败；给 `--force` 时更新该 tag 并输出 ` t [tag update] <src> -> <dst>` 行。默认自动跟随（未给 `--tags`）不更新已存在的本地 tag、也不因此失败（与 Git 一致）。

**Out of scope:** `--json` 下冲突的结构化输出（失败时保持错误信封，有意差异）；显式 tag refspec（Libra 不支持 `refs/tags/*` 目标）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `persist_fetched_tags` 遇同名不同目标且无 `--force` 时只 `tracing::warn!` 并跳过，命令照常成功 | `src/command/fetch.rs:5176`、`src/command/fetch.rs:5223-5228` |
| 评审期预探（`libra 0.30.207`）：按上游场景 `libra fetch --tags` → exit 0，`tag-1` 保持旧值，`tag-2` 被写入，无冲突诊断 | 评审期本机预探（2026-11-18） |
| Git：未强制时 `[rejected]` + `would clobber existing tag` 且返回失败；强制时 `[tag update]` | Git `6de20f6092:builtin/fetch.c:995-1026`；上游 `6de20f6092:t/t5510-fetch.sh:1900-1919` |

**Acceptance criteria:**

- [ ] **AC1** `libra fetch --tags` 以 `LBR-CONFLICT-002` 失败。
- [ ] **AC2** AC1 的 stderr 含 `tag-1` 冲突拒绝行。
- [ ] **AC3** AC1 的调用前后 `refs/tags/tag-1` 不变（仍为 E1）。
- [ ] **AC4** AC1 后 `refs/tags/tag-2` 存在且指向 E1。
- [ ] **AC5** `libra fetch --tags --force` 后 `refs/tags/tag-1` 指向 E2。
- [ ] **AC6** AC5 的 stderr 含 `tag-1` 的 tag 更新行。
- [ ] **AC7** 未给 `--tags` 的 `libra fetch` 调用前后 `refs/tags/tag-1` 不变（仍为 E1）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F16（同构 `t5510:1900-1919`）：临时目录 `D` 下 `D/base`（空提交 E1，轻量 tag `tag-1`→E1）；`D/repo` 由 `libra clone --bare ./base repo` 建立；随后在 `D/base` 执行 `libra tag tag-2`（→E1）、提交 E2、`libra tag -f tag-1`（→E2）；前置断言：每个 AC 的调用前 `Ref(D/repo, refs/tags/tag-1)` 等于 E1、`Ref(D/repo, refs/tags/tag-2)` 为 `absent`；每个 AC 在独立同构副本上执行；fetch 以 `cwd=D/repo` 运行；每个 AC 的调用另按「验证执行协议」第 4 项取所需快照；标注「Git 同夹具期望」的值按该项以钉选 Git 复核）

- AC1 → `command::fetch_test::fetch_tags_rejects_clobbering_tag_without_force`：期望退出码非 0 且 stderr 含 `LBR-CONFLICT-002`（Git 同夹具期望：`test_must_fail`）。
- AC2 → `command::fetch_test::fetch_tags_rejects_clobbering_tag_without_force`：期望 stderr 含行 ` ! [rejected]        tag-1      -> tag-1  (would clobber existing tag)`（Git 同夹具期望）。
- AC3 → `command::fetch_test::fetch_tags_rejects_clobbering_tag_without_force`：期望 AC1 调用前后 `Ref(D/repo, refs/tags/tag-1)` 逐字相等（前置断言已确定调用前为 E1）。
- AC4 → `command::fetch_test::fetch_tags_rejects_clobbering_tag_without_force`：期望 `Ref(D/repo, refs/tags/tag-2)` 逐字等于 E1（Git 同夹具期望）。
- AC5 → `command::fetch_test::fetch_tags_rejects_clobbering_tag_without_force`：期望 exit 0；`Ref(D/repo, refs/tags/tag-1)` 逐字等于 E2。
- AC6 → `command::fetch_test::fetch_tags_rejects_clobbering_tag_without_force`：期望 AC5 调用的 stderr 含行 ` t [tag update]      tag-1      -> tag-1`（Git 同夹具期望）。
- AC7 → `command::fetch_test::fetch_tags_rejects_clobbering_tag_without_force`：期望 exit 0；调用前后 `Ref(D/repo, refs/tags/tag-1)` 逐字相等（前置断言已确定调用前为 E1）。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::fetch_test::fetch_tags_rejects_clobbering_tag_without_force)'`（卡特有 `(new)`，`tests/command/fetch_test.rs` 内新增；覆盖 AC1–AC7；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::fetch_test::/)'`（共享回归组：`command::fetch_test` 模块族（含既有 `test_fetch_json_output_reports_updated_refs`、`test_fetch_prune_tags_matrix`）；按「验证执行协议」第 5 项先以同一前缀与过滤执行 `cargo nextest list` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test compat_fetch_remote_refspec`（共享回归组：整 target，fetch refspec 目标映射、FETCH_HEAD、原子多 ref 回滚回归；按「验证执行协议」第 5 项先以 `source .env.test && cargo nextest list --test compat_fetch_remote_refspec` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `FP-15`, `DEP-FP-01`, `DEP-FP-08`

**Implementation write set:** `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `docs/commands/fetch.md`, `docs/commands/zh-CN/fetch.md`, `docs/development/commands/fetch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`（由 `--only fetch` 同步生成；网站 zh-CN 渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`would clobber existing tag`。逐文件语义核对：`docs/development/commands/fetch.md` 与 `COMPATIBILITY.md` 的 `| fetch |` 行（基线第 223 行）——受本卡行为影响即同卡原地更新（不增删行），否则按第 1 步记录理由与未变 SHA-256；本卡兼容登记要点：`| fetch |` 行登记 `--tags` 冲突 tag 拒绝与整体失败；退出码为 `LBR-CONFLICT-002` 默认 128（Git 为 1，有意差异）。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码，不新增 `StableErrorCode`）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=fetch--tags冲突tag拒绝; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=FP-15,DEP-FP-01,DEP-FP-08; writeset=序列化于FP-15; release=independent; split-from=N/A; exception=N/A`

---

### Task FP-17: fetch 按大小写不敏感读取 `remote.<name>.tagOpt`（#874）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** `resolve_tag_mode` 改用既有 `ConfigKv::get_var_case_insensitive`（前缀 `remote.<name>.`、变量 `tagopt`）读取，使 `tagopt`、`tagOpt`、`TAGOPT` 等拼写均生效；值仍为 `--tags`→全部 tag、`--no-tags`→不取 tag、其它→自动跟随；CLI `--tags`/`--no-tags` 优先于配置。

**Out of scope:** `--tags` 冲突拒绝（`FP-16`）；配置值的其它拼写（Git 同样只认 `--tags`/`--no-tags`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `resolve_tag_mode` 用 `ConfigKv::get(&format!("remote.{remote_name}.tagOpt"))` 精确键读取 | `src/command/fetch.rs:5113-5126` |
| 既有大小写不敏感读取 helper | `src/internal/config.rs:536-557`、`src/internal/config.rs:676-682` |
| 评审期预探（`libra 0.30.207`）：`libra config remote.remote_one.tagopt -- --no-tags` 后 `libra fetch remote_one` 仍写入 `refs/tags/tag_one` | 评审期本机预探（2026-11-18） |
| 上游 `fetch with tagopt=--no-tags does not get tag` 等 | Git `6de20f6092:t/t5525-fetch-tagopt.sh:15-50` |

**Acceptance criteria:**

- [ ] **AC1** `remote.remote_one.tagopt=--no-tags` 时 `libra fetch remote_one` 后不存在任何 `refs/tags/` 引用。
- [ ] **AC2** `remote.remote_one.tagOpt=--no-tags` 时 `libra fetch remote_one` 后不存在任何 `refs/tags/` 引用。
- [ ] **AC3** `remote.remote_one.TAGOPT=--tags` 时 `libra fetch remote_one` 后 `refs/tags/tag_lone` 指向 L1。
- [ ] **AC4** `remote.remote_one.tagopt=--no-tags` 时 `libra fetch --tags remote_one` 后 `refs/tags/tag_lone` 指向 L1（CLI `--tags` 覆盖配置；L1 不可经自动跟随取得）。
- [ ] **AC5** `remote.remote_one.tagopt=--tags` 时 `libra fetch --no-tags remote_one` 后不存在任何 `refs/tags/` 引用。
- [ ] **AC6** 未配置 tagopt 时 `libra fetch remote_one` 后 `refs/tags/` 条目集合恰为 `{tag_one→M1}`（自动跟随，不含 `tag_lone`）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F17（同构 `t5525`）：临时目录 `D` 下主仓库 `D/m`（提交 M1，无 tag）；`D/one` 由 `libra clone --mirror ./m one` 建立（裸仓库）；另建临时工作仓库 `D/tmp`（`libra clone ./one tmp`），在其中执行 `libra tag tag_one main`（→M1）、`libra branch branch_one main`，再 `libra switch -c tmpb` 并提交 L1、`libra tag tag_lone`（→L1），然后 `libra push ../one refs/tags/tag_one refs/heads/branch_one refs/tags/tag_lone`（`D/one` 中 L1 只经 `tag_lone` 可达，`D/one` 不含 `tmpb`）；`D/m` 以 `libra remote add remote_one ../one` 配置；每个 AC 在独立同构副本上执行，配置以 `cwd=D/m` 的 `libra config <键> -- <值>` 写入；fetch 以 `cwd=D/m` 运行；每个 AC 的调用另按「验证执行协议」第 4 项取所需快照；标注「Git 同夹具期望」的值按该项以钉选 Git 复核）

- AC1 → `command::fetch_test::fetch_reads_remote_tagopt_case_insensitively`：期望 exit 0；调用后 `RefMap(D/m)` 中键以 `refs/tags/` 开头的条目集合为空（Git 同夹具期望，`t5525:23-27`）。
- AC2 → `command::fetch_test::fetch_reads_remote_tagopt_case_insensitively`：期望 exit 0；调用后 `RefMap(D/m)` 中键以 `refs/tags/` 开头的条目集合为空。
- AC3 → `command::fetch_test::fetch_reads_remote_tagopt_case_insensitively`：期望 exit 0；`Ref(D/m, refs/tags/tag_lone)` 逐字等于 L1。
- AC4 → `command::fetch_test::fetch_reads_remote_tagopt_case_insensitively`：期望 exit 0；`Ref(D/m, refs/tags/tag_lone)` 逐字等于 L1（Git 同夹具期望：`--tags` 取全部 tag 且配置只在命令行未指定时生效，`builtin/fetch.c:600-610,1927-1932`；同构 `t5525:29-37`。忽略配置与命令行而按自动跟随的实现只会取得 `tag_one`，不会取得 `tag_lone`，见 AC6）。
- AC5 → `command::fetch_test::fetch_reads_remote_tagopt_case_insensitively`：期望 exit 0；调用后 `RefMap(D/m)` 中键以 `refs/tags/` 开头的条目集合为空（Git 同夹具期望，`t5525:39-43`）。
- AC6 → `command::fetch_test::fetch_reads_remote_tagopt_case_insensitively`：期望 exit 0；`RefMap(D/m)` 中键以 `refs/tags/` 开头的条目集合逐字等于 `{refs/tags/tag_one→M1}`。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::fetch_test::fetch_reads_remote_tagopt_case_insensitively)'`（卡特有 `(new)`，`tests/command/fetch_test.rs` 内新增；覆盖 AC1–AC6；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::fetch_test::/)'`（共享回归组：`command::fetch_test` 模块族（含既有 `test_fetch_json_output_reports_updated_refs`、`test_fetch_prune_tags_matrix`）；按「验证执行协议」第 5 项先以同一前缀与过滤执行 `cargo nextest list` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test compat_config_defaults_semantics`（共享回归组：整 target，`fetch.prune`/`remote.<name>.prune` 严格读取与 `--all` 预网络校验等配置默认回归；按「验证执行协议」第 5 项先以 `source .env.test && cargo nextest list --test compat_config_defaults_semantics` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `FP-16`, `DEP-FP-01`, `DEP-FP-08`

**Implementation write set:** `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `docs/commands/fetch.md`, `docs/commands/zh-CN/fetch.md`, `docs/development/commands/fetch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`（由 `--only fetch` 同步生成；网站 zh-CN 渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`tagopt`。逐文件语义核对：`docs/development/commands/fetch.md` 与 `COMPATIBILITY.md` 的 `| fetch |` 行（基线第 223 行）——受本卡行为影响即同卡原地更新（不增删行），否则按第 1 步记录理由与未变 SHA-256；本卡兼容登记要点：`| fetch |` 行登记 `remote.<name>.tagOpt` 大小写不敏感读取。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码，不新增 `StableErrorCode`）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=fetch的remote.*.tagOpt读取; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=FP-16,DEP-FP-01,DEP-FP-08; writeset=序列化于FP-16; release=independent; split-from=N/A; exception=N/A`

---

### Task FP-18: `libra fetch` 完成后按 `gc --auto` 判定触发自动打包（#863；拆出 FP-24）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 按 ADR-FP-04：`src/cli.rs` 在 `Commands::Fetch` 分发成功返回、命令级 `MaintenanceLock::shared` 释放后，若该次 fetch 非 `--dry-run` 且非 `--no-auto-gc`，调用 `FP-24` 的 `gc --auto` 判定与执行入口（静默子配置：需要时只输出 `Auto packing the repository for optimum performance.` 一行到 stderr，不需要时在 stdout 与 stderr 上都不输出任何内容，包括不输出 `Auto gc skipped`）；`--no-auto-gc` 不再是 no-op（更新其 help 文本）。`pull`/`clone` 不触发（Libra 有意差异）。按 G-09 自本卡拆出 `FP-24`（`gc --auto` 的 pack 数判定）。

**Out of scope:** `gc --auto` 判定本身（`FP-24`）；`fetch.unpackLimit` 小包解包为松散对象（`DEFER-FP-04`；`#863` 上游场景设 `fetch.unpackLimit 1`，接收路径保留 pack 即与 Git 一致）；`pull`/`clone` 后自动维护；外部 `TC-1338` 整文件 harness 运行（`DEP-FP-06` 移交证据，非本卡门）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `run_fetch` 以 `no_auto_gc: _` 丢弃该参数，`--no-auto-gc` 的 help 声明为 no-op | `src/command/fetch.rs:776-780,1451` |
| `cli.rs` 在整条命令期间持有命令级 `MaintenanceLock::shared(storage)`（`command_holds_shared_maintenance_lock` 对 `Fetch` 为真、对 `Gc` 为假），随后分发 `Commands::Fetch` | `src/cli.rs:3760-3772`、`src/cli.rs:4067` |
| 评审期预探（`libra 0.30.207`）：`file://` 克隆后源端新增提交、`libra fetch --verbose` 已正确推进 `origin/main`，`.libra/objects/pack` 中有 2 个 pack，无自动打包 | 评审期本机预探（2026-11-18） |
| Git：fetch 结束时 `run_auto_maintenance`（`--no-auto-gc` 关闭）；上游 `fetching with auto-gc does not lock up` | Git `6de20f6092:builtin/fetch.c:2909-2927`、`6de20f6092:t/t5510-fetch.sh:1423-1440` |

**Acceptance criteria:**

- [ ] **AC1** `libra fetch --verbose` 使 `refs/remotes/origin/main` 指向 S2。
- [ ] **AC2** AC1 的 stderr 含行 `Auto packing the repository for optimum performance.`。
- [ ] **AC3** 先 `libra fetch --no-auto-gc`（pack 数变为 2）、源端再提交 S3 后，`libra fetch --dry-run` 前后 `S(D/ag)` 逐字相等（`--dry-run` 不触发自动打包）。
- [ ] **AC4** AC1 后 pack 数为 1。
- [ ] **AC5** `libra fetch --no-auto-gc` 后 pack 数为 2。
- [ ] **AC6** AC5 的 stderr 不含 `Auto packing`。
- [ ] **AC7** 默认阈值下 `libra fetch` 的 stderr 只有 fetch 报告本身（自动维护不输出任何内容）。
- [ ] **AC8** AC7 的 stdout 为空。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F18（同构 `t5510:1423-1440`）：临时目录 `D` 下源仓库 `D/s`（提交 S1）；`D/ag` 由 `libra clone file://<D/s 的绝对路径> ag` 建立；随后在 `D/s` 提交 S2；在 `D/ag` 写入 `fetch.unpackLimit=1`、`gc.autoDetach=false`、`maintenance.strategy=gc`，AC1–AC6 的副本另写 `gc.autoPackLimit=1`（AC3 的副本在 S2 之后先执行 `libra fetch --no-auto-gc`，再在 `D/s` 提交 S3），AC7–AC8 的副本不写 `gc.autoPackLimit`/`gc.auto`（默认 50/6700）；「pack 数」指 `D/ag/.libra/objects/pack` 下不带同名 `.keep` 文件的 `*.pack` 个数；每个 AC 在独立同构副本上执行；fetch 以 `cwd=D/ag` 运行；每个 AC 的调用另按「验证执行协议」第 4 项取所需快照；标注「Git 同夹具期望」的值按该项以钉选 Git 复核）

- AC1 → `command::fetch_test::fetch_triggers_auto_gc_on_pack_limit`：期望 exit 0；`Ref(D/ag, refs/remotes/origin/main)` 逐字等于 S2（`#863` 验收 1）。
- AC2 → `command::fetch_test::fetch_triggers_auto_gc_on_pack_limit`：期望 AC1 调用的 stderr 含该行（Git 同夹具期望，`t5510:1423-1440`）。
- AC3 → `command::fetch_test::fetch_triggers_auto_gc_on_pack_limit`：期望 exit 0；`--dry-run` 调用前后 `S(D/ag)` 逐字相等（前置断言：调用前 pack 数为 2，若误触发自动打包会合并 pack 而改变对象清单）。
- AC4 → `command::fetch_test::fetch_triggers_auto_gc_on_pack_limit`：期望 AC1 调用后 pack 数等于 1。
- AC5 → `command::fetch_test::fetch_triggers_auto_gc_on_pack_limit`：期望 exit 0；调用后 pack 数等于 2。
- AC6 → `command::fetch_test::fetch_triggers_auto_gc_on_pack_limit`：期望 AC5 调用的 stderr 不含子串 `Auto packing`。
- AC7 → `command::fetch_test::fetch_triggers_auto_gc_on_pack_limit`：期望 exit 0；stderr 逐字等于 `From <url>\n   <o..n>  main       -> origin/main\n`（`<url>` 为 `D/ag` 的 `remote.origin.url`，`<o..n>` 为 S1、S2 的 7 位缩写以 `..` 连接；不含 `Auto packing` 或 `Auto gc skipped`）。
- AC8 → `command::fetch_test::fetch_triggers_auto_gc_on_pack_limit`：期望 AC7 调用的 stdout 为空字节串。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::fetch_test::fetch_triggers_auto_gc_on_pack_limit)'`（卡特有 `(new)`，`tests/command/fetch_test.rs` 内新增；覆盖 AC1–AC8；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::fetch_test::/)'`（共享回归组：`command::fetch_test` 模块族（含既有 `test_fetch_json_output_reports_updated_refs`、`test_fetch_prune_tags_matrix`）；按「验证执行协议」第 5 项先以同一前缀与过滤执行 `cargo nextest list` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::gc_test::/)'`（共享回归组：`command::gc_test` 模块族；按「验证执行协议」第 5 项先以同一前缀与过滤执行 `cargo nextest list` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 修改 src/cli.rs 共享分发路径（GC-02），且版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `FP-24`, `DEP-FP-01`, `DEP-FP-08`

**Implementation write set:** `src/cli.rs`, `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `docs/commands/fetch.md`, `docs/commands/zh-CN/fetch.md`, `docs/development/commands/fetch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/cli.rs`, `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`（由 `--only fetch` 同步生成；网站 zh-CN 渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--no-auto-gc`。逐文件语义核对：`docs/development/commands/fetch.md` 与 `COMPATIBILITY.md` 的 `| fetch |` 行（基线第 223 行）——受本卡行为影响即同卡原地更新（不增删行），否则按第 1 步记录理由与未变 SHA-256；本卡兼容登记要点：`| fetch |` 行登记 fetch 后按 `gc --auto` 判定自动打包与 `--no-auto-gc`；`pull`/`clone` 不自动打包（有意差异）。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码，不新增 `StableErrorCode`）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=fetch后的auto-gc触发; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=FP-24,DEP-FP-01,DEP-FP-08; writeset=序列化于FP-24; release=independent; split-from=N/A; exception=N/A`

---

### Task FP-24: `gc --auto` 按 `gc.autoPackLimit` 判定并输出自动打包提示（#863；自 FP-18 拆出）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 按 ADR-FP-04 扩展 `gc --auto` 的触发判定：`gc.auto<=0` 时从不触发；否则「不带同名 `.keep` 文件的 pack 数 `>` `gc.autoPackLimit`（默认 50，`<=0` 不按 pack 数触发）」或「松散对象数达到 `gc.auto`（默认 6700，沿用现有精确计数与 `>=` 判定，属已登记的有意差异）」即运行完整 gc；触发判定集中在一个判定函数中（输入为配置、不带 `.keep` 的 pack 数与松散对象数，输出是否触发；`gc.autoPackLimit` 缺省 50），`gc --auto` 与 `FP-18` 均直接调用该函数；判定需要运行时先向 stderr 输出一行 `Auto packing the repository for optimum performance.`，判定不需要时行为不变。该判定函数是 `FP-18` fetch 后自动打包的唯一事实源（GC-02）。

**Out of scope:** fetch 后触发（`FP-18`）；`gc.autoDetach` 后台执行（Libra 恒前台）；Git 第二行提示 `See "git help gc" ...`；松散对象数的 Git 抽样估算。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `gc --auto` 只按松散对象数与 `gc.auto` 判定（`threshold <= 0 || loose_count < threshold` 时跳过并向 stdout 打印 `Auto gc skipped: ...`），不读取 `gc.autoPackLimit` | `src/command/gc.rs:151-187`、`src/command/gc.rs:330-345` |
| Git：`gc.auto<=0` 关闭；`too_many_packs` 计非 `.keep` pack 数 `> gc.autoPackLimit`（默认 50，`<=0` 不计）；松散对象以 `objects/17` 抽样估算 `> DIV_ROUND_UP(gc.auto,256)*256`；前台提示 `Auto packing the repository for optimum performance.` | Git `6de20f6092:odb/source-files.c:326-342,369-386,544-565`、`6de20f6092:builtin/gc.c:656-661` |

**Acceptance criteria:**

- [ ] **AC1** `gc.autoPackLimit=1` 时 `libra gc --auto` 后 `D/g/.libra/objects/pack` 下恰有 1 个 `*.pack` 文件。
- [ ] **AC2** AC1 的 stderr 含行 `Auto packing the repository for optimum performance.`。
- [ ] **AC3** `gc.autoPackLimit=1`、`gc.auto=0` 时 `libra gc --auto` 前后 `S(D/g)` 逐字相等（不运行任何 gc 步骤）。
- [ ] **AC4** `gc.autoPackLimit=0` 时 `libra gc --auto` 前后 `S(D/g)` 逐字相等。
- [ ] **AC5** `gc.autoPackLimit=1` 且其中一个 pack 带 `.keep` 时 `libra gc --auto` 前后 `S(D/g)` 逐字相等（非 `.keep` pack 数 1 不大于 1）。
- [ ] **AC6** 不写 `gc.autoPackLimit` 与 `gc.auto`（缺省 50/6700）且不带 `.keep` 的 pack 恰 50 个时 `libra gc --auto` 前后 `S(D/g)` 逐字相等（50 不大于缺省阈值 50，不触发）。
- [ ] **AC7** 同样不写两键、不带 `.keep` 的 pack 恰 51 个时 `libra gc --auto` 后 `D/g/.libra/objects/pack` 下恰有 1 个 `*.pack` 文件（缺省阈值 50 经 `gc --auto` 实际路径生效，未配置不等于关闭）。
- [ ] **AC8** AC6 的 stderr 不含 `Auto packing`。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F24：临时目录 `D` 下源仓库 `D/s`（提交 S1）；`D/g` 由 `libra clone file://<D/s 的绝对路径> g` 建立，随即以 `cwd=D/g` 的 `libra config` 写入 `fetch.unpackLimit=1` 与 `gc.autoDetach=false`（Libra 不读取这两个键：接收即保留 pack、gc 恒前台；写入是为使钉选 Git 的同构夹具同样逐次保留 pack——Git 缺省 `unpack_limit = 100`，少于该对象数的 pack 会被解包，`fetch-pack.c:43,991-999`——并以前台提示文本运行，`builtin/gc.c:656-661`）；随后在 `D/s` 提交 S2，在 `D/g` 执行 `libra fetch --no-auto-gc`；前置断言：此时 `D/g/.libra/objects/pack` 下不带同名 `.keep` 的 `*.pack` 恰 2 个、松散对象数低于 6700；AC5 的副本另对其中一个 pack 创建同名 `.keep` 文件（在取调用前 `S(D/g)` 之前创建）；AC6–AC8 的副本不写 `gc.autoPackLimit` 与 `gc.auto`，并在上述两 pack 状态上重复「在 `D/s` 新增一个提交、在 `D/g` 执行 `libra fetch --no-auto-gc`」：AC6（与 AC8 为同一调用）的副本重复 48 次、AC7 的副本重复 49 次，前置断言调用前 `D/g/.libra/objects/pack` 下不带同名 `.keep` 的 `*.pack` 分别恰为 50、51 个且松散对象数仍低于 6700；配置以 `cwd=D/g` 的 `libra config` 写入；每个 AC 在独立同构副本上执行；`libra gc --auto` 以 `cwd=D/g` 运行；每个 AC 的调用另按「验证执行协议」第 4 项取所需快照；标注「Git 同夹具期望」的值按该项以钉选 Git 复核）

- AC1 → `command::gc_test::gc_auto_triggers_on_auto_pack_limit`：期望 exit 0；调用后该目录下 `*.pack` 文件数等于 1。
- AC2 → `command::gc_test::gc_auto_triggers_on_auto_pack_limit`：期望 AC1 调用的 stderr 含该行（Git 同夹具期望，`builtin/gc.c:656-661`）。
- AC3 → `command::gc_test::gc_auto_triggers_on_auto_pack_limit`：期望 exit 0；调用前后 `S(D/g)` 逐字相等。
- AC4 → `command::gc_test::gc_auto_triggers_on_auto_pack_limit`：期望 exit 0；调用前后 `S(D/g)` 逐字相等。
- AC5 → `command::gc_test::gc_auto_triggers_on_auto_pack_limit`：期望 exit 0；调用前后 `S(D/g)` 逐字相等（Git 同夹具期望，`odb/source-files.c:369-386`）。
- AC6 → `command::gc_test::gc_auto_triggers_on_auto_pack_limit`：期望 exit 0；调用前后 `S(D/g)` 逐字相等（Git 同夹具期望，`odb/source-files.c:369-386` 的 `gc_auto_pack_limit < cnt`；按 `>=` 判定或缺省值小于 50 的实现会运行 gc 而改变对象清单）。
- AC7 → `command::gc_test::gc_auto_triggers_on_auto_pack_limit`：期望 exit 0；调用后该目录下 `*.pack` 文件数等于 1（Git 同夹具期望，`odb/source-files.c:369-386,544-565`；把未配置当作关闭或缺省值大于 50 的实现不运行 gc，pack 数保持 51）。
- AC8 → `command::gc_test::gc_auto_triggers_on_auto_pack_limit`：期望 AC6 调用的 stderr 不含子串 `Auto packing`（判定不触发时不输出提示；无条件输出提示的实现在此失败）。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::gc_test::gc_auto_triggers_on_auto_pack_limit)'`（卡特有 `(new)`，`tests/command/gc_test.rs` 内新增；覆盖 AC1–AC8；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::gc_test::/)'`（共享回归组：`command::gc_test` 模块族；按「验证执行协议」第 5 项先以同一前缀与过滤执行 `cargo nextest list` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^command::gc::tests::/)'`（共享回归组：`src/command/gc.rs` `mod tests` 单元测试族；按「验证执行协议」第 5 项先以同一前缀与过滤执行 `cargo nextest list` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 修改共享 gc 自动判定 src/command/gc.rs（GC-02，FP-18 复用），且版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `FP-17`, `DEP-FP-01`, `DEP-FP-08`

**Implementation write set:** `src/command/gc.rs`, `tests/command/gc_test.rs`, `docs/commands/gc.md`, `docs/commands/zh-CN/gc.md`, `docs/development/commands/gc.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/gc.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/gc.rs`, `tests/command/gc_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/gc.md`、`docs/commands/zh-CN/gc.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/gc.en.md`（由 `--only gc` 同步生成；网站 zh-CN 渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`gc.autoPackLimit`。逐文件语义核对：`docs/development/commands/gc.md` 与 `COMPATIBILITY.md` 的 `| gc |` 行（基线第 267 行）——受本卡行为影响即同卡原地更新（不增删行），否则按第 1 步记录理由与未变 SHA-256；本卡兼容登记要点：`| gc |` 行登记 `gc --auto` 读取 `gc.autoPackLimit` 与自动打包提示；松散对象计数沿用精确 `>=`、无后台模式（有意差异）。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码，不新增 `StableErrorCode`）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=gc--auto的pack数阈值判定; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=FP-17,DEP-FP-01,DEP-FP-08; writeset=序列化于FP-17; release=independent; split-from=FP-18; exception=N/A`

## 测试矩阵

| 类别 | 必须覆盖 | Target / command |
|---|---|---|
| 单元 | `push` 参数解析（`--force` 与 `--force-with-lease`） | 下方代码块 `M-UNIT` |
| 集成 | 23 张卡在 `command_test` 中的全部卡特有用例（各卡按「验证执行协议」逐条精确运行；`FP-06` 的单元用例由上一行覆盖） | 下方代码块 `M-INT` |
| 兼容 | fetch refspec 映射、本地 push 不切换 CWD、配置默认值 | `source .env.test && cargo nextest run --test compat_fetch_remote_refspec --test compat_local_push_no_cwd_switch --test compat_config_defaults_semantics` |
| 迁移 | N/A（无 SQLite schema 变更） | N/A |
| 安全 | 裸目录名只在解析为 Libra 仓库时生效、非常规目标的 refname 校验、租约不匹配与冲突 tag fail-closed | 下方代码块 `M-SEC` |
| 性能 | 多远程串行 fetch、自动打包阈值 | 下方代码块 `M-PERF` |
| live/gated | 每卡 T-1 与收口阶段双环境全量回归 | `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` |

矩阵命令（可直接复制；`|` 为 nextest 过滤表达式的并集运算符，均为共享回归口径，按「验证执行协议」第 5 项记录 N 与四计数）：

```bash
# M-UNIT
source .env.test && cargo nextest run --lib -E 'test(/^command::push::test::/) | test(/^command::gc::tests::/)'
# M-INT
source .env.test && cargo nextest run --test command_test -E 'test(/^command::fetch_test::/) | test(/^command::push_test::/) | test(/^command::gc_test::/)'
# M-SEC
source .env.test && cargo nextest run --test command_test -E 'test(=command::fetch_test::fetch_bare_local_directory_name_as_anonymous_repository) | test(=command::push_test::push_bare_local_directory_name_as_local_repository) | test(=command::fetch_test::fetch_unconventional_destination_namespace) | test(=command::push_test::push_force_with_lease_semantics_without_force) | test(=command::push_test::push_force_with_lease_empty_expectation) | test(=command::fetch_test::fetch_tags_rejects_clobbering_tag_without_force)'
# M-PERF
source .env.test && cargo nextest run --test command_test -E 'test(=command::fetch_test::fetch_multiple_remotes_and_jobs) | test(=command::fetch_test::fetch_triggers_auto_gc_on_pack_limit) | test(=command::gc_test::gc_auto_triggers_on_auto_pack_limit)'
```

## 追溯表

| 任务 | 来源/证据 | Libra 落点 | 文档/兼容动作 | 指定测试 |
|---|---|---|---|---|
| FP-01 | `#719` | `src/internal/protocol/repository_arg.rs`、`src/command/fetch.rs` | `docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md`、`docs/development/commands/fetch.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md` | `command::fetch_test::fetch_bare_local_directory_name_as_anonymous_repository` |
| FP-02 | `#710` | `src/command/push.rs` | `docs/commands/push.md`、`docs/commands/zh-CN/push.md`、`docs/development/commands/push.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md` | `command::push_test::push_bare_local_directory_name_as_local_repository` |
| FP-03 | `#683` | `src/command/push.rs` | `docs/commands/push.md`、`docs/commands/zh-CN/push.md`、`docs/development/commands/push.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md` | `command::push_test::push_uses_remote_push_refspec_config` |
| FP-04 | `#877` | `src/command/push.rs` | `docs/commands/push.md`、`docs/commands/zh-CN/push.md`、`docs/development/commands/push.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md` | `command::push_test::push_prune_wildcard_and_negative_refspecs` |
| FP-05 | `#1031` | `src/internal/protocol/local_push.rs`、`src/command/push.rs` | `docs/commands/push.md`、`docs/commands/zh-CN/push.md`、`docs/development/commands/push.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md` | `command::push_test::push_local_respects_receive_deny_current_branch` |
| FP-06 | `#1036` | `src/command/push.rs` | `docs/commands/push.md`、`docs/commands/zh-CN/push.md`、`docs/development/commands/push.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md` | `command::push_test::push_force_overrides_force_with_lease_rejection`、`command::push::test::force_and_force_with_lease_parse_together` |
| FP-21 | `#1036` | `src/command/push.rs` | `docs/commands/push.md`、`docs/commands/zh-CN/push.md`、`docs/development/commands/push.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md` | `command::push_test::push_force_with_lease_semantics_without_force` |
| FP-23 | `#719` | `src/command/fetch.rs` | `docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md`、`docs/development/commands/fetch.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md` | `command::fetch_test::fetch_command_line_refspec_short_destination_and_anonymous_write` |
| FP-25 | `#1036` | `src/command/push.rs` | `docs/commands/push.md`、`docs/commands/zh-CN/push.md`、`docs/development/commands/push.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md` | `command::push_test::push_force_with_lease_empty_expectation` |
| FP-07 | `#749` | `src/command/fetch.rs` | `docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md`、`docs/development/commands/fetch.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md` | `command::fetch_test::fetch_without_repository_uses_git_default_remote` |
| FP-08 | `#1000` | `src/command/fetch.rs` | `docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md`、`docs/development/commands/fetch.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md` | `command::fetch_test::fetch_multiple_command_line_refspecs` |
| FP-09 | `#722` | `src/command/fetch.rs` | `docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md`、`docs/development/commands/fetch.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md` | `command::fetch_test::fetch_unconventional_destination_namespace` |
| FP-10 | `#867` | `src/command/fetch.rs` | `docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md`、`docs/development/commands/fetch.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md` | `command::fetch_test::fetch_multiple_remotes_and_jobs` |
| FP-11 | `#865, #867` | `src/command/fetch.rs` | `docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md`、`docs/development/commands/fetch.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md` | `command::fetch_test::fetch_all_and_multiple_continue_after_remote_error` |
| FP-19 | `#867` | `src/command/fetch.rs` | `docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md`、`docs/development/commands/fetch.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md` | `command::fetch_test::fetch_all_config_and_no_all` |
| FP-12 | `#864` | `src/command/fetch.rs` | `docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md`、`docs/development/commands/fetch.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md` | `command::fetch_test::fetch_write_fetch_head_switches` |
| FP-13 | `#878` | `src/command/fetch.rs` | `docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md`、`docs/development/commands/fetch.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md` | `command::fetch_test::fetch_human_report_on_stderr_with_fetch_head_plan` |
| FP-14 | `#879` | `src/command/fetch.rs` | `docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md`、`docs/development/commands/fetch.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md` | `command::fetch_test::fetch_porcelain_and_no_porcelain_last_wins` |
| FP-15 | `#872` | `src/command/fetch.rs` | `docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md`、`docs/development/commands/fetch.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md` | `command::fetch_test::fetch_prune_limited_to_command_line_refspec_scope` |
| FP-16 | `#873` | `src/command/fetch.rs` | `docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md`、`docs/development/commands/fetch.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md` | `command::fetch_test::fetch_tags_rejects_clobbering_tag_without_force` |
| FP-17 | `#874` | `src/command/fetch.rs` | `docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md`、`docs/development/commands/fetch.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md` | `command::fetch_test::fetch_reads_remote_tagopt_case_insensitively` |
| FP-18 | `#863` | `src/cli.rs`、`src/command/fetch.rs` | `docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md`、`docs/development/commands/fetch.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md` | `command::fetch_test::fetch_triggers_auto_gc_on_pack_limit` |
| FP-24 | `#863` | `src/command/gc.rs` | `docs/commands/gc.md`、`docs/commands/zh-CN/gc.md`、`docs/development/commands/gc.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/gc.en.md` | `command::gc_test::gc_auto_triggers_on_auto_pack_limit` |

## 里程碑验收与回滚

| 里程碑 | 完成条件 | 发布/证据 | 回滚或前滚 |
|---|---|---|---|
| M0 | 计划文本 Claude 与 Codex 双审 `PASS` 并入库；Phase 0 锚点与 Git 期望复核完成 | 双审受审 SHA-256 与 Codex review log | N/A（修订计划文本） |
| M1 | Phase 1（`FP-01`、`FP-23`、`FP-02`、`FP-04`、`FP-03`、`FP-05`、`FP-06`、`FP-21`、`FP-25`）全部 `done`/`complete` | 各卡 `patch + 1`、`nextest` 与 D 组全绿 | 发布前 revert；发布后按 ER-10 新建 `FIX-*` 前滚发新版本 |
| M2 | Phase 2（`FP-07`、`FP-08`、`FP-09`、`FP-10`、`FP-19`、`FP-11`）全部 `done`/`complete` | 各卡 `patch + 1`、`nextest` 与 D 组全绿 | 同上 |
| M3 | Phase 3（`FP-12`、`FP-13`、`FP-14`、`FP-15`、`FP-16`、`FP-17`、`FP-24`、`FP-18`）全部 `done`/`complete` + 全量收口门通过 | 全量 `nextest` + `fmt` + `clippy` 全绿 | 同上 |

### 故障恢复矩阵

| 故障点 | 可接受残留 | 恢复动作 | 禁止结果 |
|---|---|---|---|
| 多远程 fetch 中个别 remote 失败（`FP-11`） | 已成功 remote 的引用、对象与 `FETCH_HEAD` 记录 | 逐远程输出 `could not fetch`，以首个失败的稳定码整体失败；用户修复后重跑幂等 | 首错中止丢弃其余 remote，或整体静默成功 |
| 多 refspec 中有无效项（`FP-08`） | 无 | 在任何网络交互与写入前整体拒绝（`S` 不变） | 部分 refspec 已写入 |
| `--tags` 冲突 tag（`FP-16`） | 非冲突 tag 已写入 | 冲突 tag 保持原值，整体以 `LBR-CONFLICT-002` 失败；`--force` 重跑覆盖 | 未加 `--force` 覆盖本地 tag，或静默成功 |
| 租约不匹配（`FP-21`、`FP-25`） | 无 | 拒绝且远端引用不变（`R` 不变，由 `FP-21` AC3/AC5 与 `FP-25` AC3 断言）；用户 fetch 后重试 | 覆盖远端 |
| 自动打包失败（`FP-18`） | fetch 结果已完整写入 | gc 错误按其稳定码报告；用户手动 `libra gc` | 回滚已成功的 fetch 结果 |
| 远端 D 组 CI 失败 | 已推送版本 tag 与提交 | 按 ER-04/ER-10 新建 `FIX-*` 前滚修复 | 回退已推送提交或删除已发布 tag |

## 风险登记

| 风险 | 影响 | 缓解 | 任务 |
|---|---|---|---|
| `FP-13` 把 fetch 人读报告从 stdout 移到 stderr 并删除 `N objects fetched` 等行 | 中：从 stdout 解析 fetch 报告的外部脚本失效 | `FP-13` 标 `T-5` 跑全量；`COMPATIBILITY.md` 与命令文档登记；`--json`/`--porcelain` 契约不变可供迁移 | FP-13 |
| 各卡「Git 同夹具期望」（输出列宽、`FETCH_HEAD` 顺序等）以源码与上游测试推导 | 中：与钉选 Git 实测不一致会使用例断言错误 | Phase 0 与每卡 ER-02 用 `git@6de20f6092` 构建的 Git 复核，不一致按 ER-03 回写本卡 | FP-01..FP-25 |
| `fetch.rs`、`push.rs`、`src/cli.rs` 跨计划写锁冲突 | 高：合并冲突 | 每卡开工前重新核验 `DEP-FP-01`（及适用的 `DEP-FP-02`、`DEP-FP-04`、`DEP-FP-05`） | FP-01..FP-25 |
| 外部 harness（`TC-*`）依赖同文件全部前置用例 | 中：本计划行为正确时 harness 仍可能因其它用例失败 | 改为 `DEP-FP-06` 补充证据，不作卡门；按 ER-10 只修归因于本计划行为轴的失败 | FP-18 等 |
| `FP-10` 的 `GIT_TRACE` 只实现绝对路径形式 | 低：其它取值不产生追踪 | `COMPATIBILITY.md` 登记为部分支持 | FP-10 |
| `libra for-each-ref` 把以 `refs/` 开头的全名分支行（`FP-09` 写入的 `refs/<ns>/...` 目标）显示为 `refs/heads/refs/<ns>/...`（既有缺陷，`src/command/for_each_ref.rs` 的分支枚举） | 中：`FP-09` 写入的自定义命名空间引用在 `for-each-ref` 中显示错误，`rev-parse` 与 `show-ref` 正确 | 本计划验收以 `show-ref`/`rev-parse` 观测（「验证执行协议」第 4 项）；`for-each-ref` 修复不在本计划范围，由协调者另行登记 issue | FP-09 |

## 性能与容量摘要

| 操作 | 单次成本 | 累积成本 | 预算/上限 | 验证 |
|---|---|---|---|---|
| 多远程 fetch（`--multiple`/`--all`） | 每个 remote 一次发现与 pack 传输 | `O(remote 数)` 次传输，按位置参数顺序串行（`--jobs` 不引入并发，见 `DEFER-FP-05`） | 同一时刻至多 1 个 remote 传输 | 设计约束（`FP-10` Description 规定串行执行）；`FP-10` 卡特有用例只验证结果、`FETCH_HEAD` 顺序与追踪行，不观测传输重叠（真正并发及其观测由 `DEFER-FP-05` 承接） |
| fetch 后自动打包判定 | 列出 `.libra/objects/pack` 与既有松散对象计数 | 仅在 `gc.auto`/`gc.autoPackLimit` 命中时运行完整 gc | `--no-auto-gc` 或 `gc.auto<=0` 时零额外打包 | `FP-24`、`FP-18` 卡特有用例 |
| push 通配符展开与 `--prune` | 对本地引用与远端广告引用各遍历一次 | `O(本地引用数 + 远端引用数)` | 继承 GC-10，无逐引用网络往返 | `FP-04` 卡特有用例 |

## 兼容与文档收口

- [ ] `COMPATIBILITY.md` 第 222、223、267 行已按各卡第 1 步原地同步（不增删行）。
- [ ] `docs/commands/{fetch,push,gc}.md` 已同步（`gc` 仅 `FP-24`）。
- [ ] `docs/commands/zh-CN/{fetch,push,gc}.md` 已同步。
- [ ] `docs/development/commands/{fetch,push,gc}.md` 已按各卡第 1 步逐文件语义核对：受影响者已同卡同步，其余已记录「无需修改」的理由与未变 SHA-256。
- [ ] `/Volumes/Data/gitmono/libra-backend` 的 `fetch`/`push`/`gc` 网站页已按各卡「文档与网站验收命令」第 3 步同步，且 `cd /Volumes/Data/gitmono/libra-backend && python3 scripts/sync-libra-command-docs.py --libra-root /Volumes/Data/gitmono/libra --only fetch push gc` 退出码为 0。
- [ ] `docs/error-codes.md`：`N/A`（复用既有稳定码，不新增 `StableErrorCode`）。
- [ ] `tests/INDEX.md`：`N/A`（只扩充既有 `command_test`，不新增或重命名 target；仅当既有索引行描述失真时更新）。
- [ ] `Cargo.toml` `[[test]]`：`N/A`（未新增 `--test` target）。
- [ ] `plan-long.md`：`N/A`（由协调者统一维护）。

## Codex review log

| Round | Scope | Result | P0/P1 | P2 处置 | Evidence |
|---|---|---|---|---|---|
| R1 (Claude) | `plan-20261117-fetch-push.md` 初稿 | `FAIL` | 5 项 P1（`FP-06` `--force` 语义与 `mod test`、`FP-01/02` `repository_arg.rs` 裸目录名、`#867`/`#863`/`#873`/`#722` 覆盖、`FP-11` 锚点）均在 R2 关闭 | P2 全部修复 | Claude R1 review |
| R1 (Codex) | `plan-20261117-fetch-push.md` 初稿 | `FAIL` | 7 项 P1（`FP-06`、`FP-04` 负向 refspec、`FP-08` `FETCH_HEAD`、#867/`FP-01/09/12/13/15/16/18`、G-03/G-04 计数、G-05）均在 R2 关闭 | — | Codex R1 review |
| R2 (Codex) | `plan-20261117-fetch-push.md` R2 | `FAIL` | P1（`FP-18` `fetch.unpackLimit` vs `gc.autoPackLimit`、`FP-15` `--tags`、D 组真实 job 名、`FP-01` 跨计划依赖）均在 R3 关闭 | P2 全部修复 | Codex R2 review |
| R3 (Claude) | `plan-20261117-fetch-push.md` R3 | `FAIL` | P1（`ADR-FP-01` opt-in、`DEP-FP-04`/`05`、`FP-17` 仅消费既有 helper、`FP-18` `t5510#308`）均在 R4 关闭 | P2 全部修复 | Claude R3 review |
| R3 (Codex) | `plan-20261117-fetch-push.md` R3 | `FAIL` | P1（`FP-08/10` `src/cli.rs`、`DEP-FP-04`、谓词计数）均在 R4 关闭 | P2 全部修复 | Codex R3 review |
| R4 (Claude) | `plan-20261117-fetch-push.md` R4 | `FAIL` | P1（`FP-08`/`10` 残留 `src/cli.rs`、`DEP-FP-05` 未挂接、`FP-17` `config.rs:536`）均在 R5 关闭 | P2 全部修复 | Claude R4 review |
| R4 (Codex) | `plan-20261117-fetch-push.md` R4 | `FAIL` | P1（`FP-08`/`10` 残留 `src/cli.rs`、`FP-11` `DEP-FP-04`、`FP-17` 追溯表）均在 R5 关闭 | P2（`review log` 同步）在 R5 关闭 | Codex R4 review |
| R5 (Claude) | `plan-20261117-fetch-push.md` R5 | `FAIL` | 2 项 P1（`FP-10` 拆出 `FP-19`、`FP-11` 补 `--multiple`/`--jobs` 与 `t5514#13`）均在 R6 关闭 | P2（`FP-07`/`08`/`18`/`03`/`15`/`04`、`DEP-FP-03`）在 R6 关闭 | Claude R5 review |
| R5 (Codex) | `plan-20261117-fetch-push.md` R5 | `FAIL` | 2 项 P1（`#863` `TC-1338` 门禁、ER-13 全量失败处置）均在 R6 关闭 | — | Codex R5 review |
| R6 (Claude) | `plan-20261117-fetch-push.md` R6 | `FAIL` | 4 项 P1（20 卡全文同步、12 卡 `COMPATIBILITY.md`、`FP-20` `../libra-testcases/...` 路径、`FP-18` `gc` 文档与 `gc_test`）均在 R7 关闭 | P2 全部修复 | Claude R6 review |
| R6 (Codex) | `plan-20261117-fetch-push.md` R6 | `FAIL` | 1 项 P1（`FP-20` 外部语料构建与自洽执行）在 R7 关闭 | P2（`M2`/`M3` 补 `FP-19`/`FP-20`）在 R7 关闭 | Codex R6 review |
| R7 (Claude) | `plan-20261117-fetch-push.md` R7 | `FAIL` | 1 项 P1（`FP-06` 拆出 `FP-21` 并重算 `FP-05/08/13/16` AC 计数）在 R8 关闭 | P2（`FP-20`/`DEP-FP-05`/`FP-07`/`FP-10/11/16`/`FP-18`/文档收口/发布者/错误码表）在 R8 关闭 | Claude R7 review |
| R7 (Codex) | `plan-20261117-fetch-push.md` R7 | `FAIL` | 1 项 P1（`FP-20` `>= fetch.unpackLimit` 边界与默认 `100`）在 R8 关闭 | — | Codex R7 review |
| R8 (Claude) | `plan-20261117-fetch-push.md` R8 | `ERROR` | 网络连接中断（`ECONNRESET`），合并至 R9 重审 | — | Claude R8 review |
| R8 (Codex) | `plan-20261117-fetch-push.md` R8 | `FAIL` | 0 项 P0/P1 | 1 项 P2（`FP-10` `--jobs=0` 应按 Git 选择默认并发度）在 R9 关闭 | Codex R8 review |
| R9 (Claude) | `plan-20261117-fetch-push.md` R9 | `ERROR` | 网络连接中断（`ECONNRESET`），合并至 R10 重审 | — | Claude R9 review |
| R9 (Codex) | `plan-20261117-fetch-push.md` R9 | `FAIL` | 1 项 P1（`FP-20` `TC-1338` / `t5510#308` 必须作为硬门控验收，缺失语料阻塞而非跳过）在 R10 关闭 | — | Codex R9 review |
| R10 (Claude) | `plan-20261117-fetch-push.md` R10 | `FAIL` | 2 项 P1（`FP-21`/`FP-16` 退出码对齐 `docs/error-codes.md` `128` + `COMPATIBILITY.md`；`FP-20` 澄清 `tc1338` 即 `#201` 门控与风险表）均在 R11 关闭 | P2（`FP-11` 错误码、`FP-05` `ignore`、`FP-18` `1555`、`FP-13` `T-5`、`Phase 0`）在 R11 关闭 | Claude R10 review |
| R10 (Codex) | `plan-20261117-fetch-push.md` R10 | `FAIL` | 1 项 P1（`FP-20` 澄清 `tc1338-fetch-verbose.sh` 即 `#201`/`#308` 唯一可执行门控并明确 `SKIP`/前置失败处置）在 R11 关闭 | P2（`FP-04` `AC=5/8`、`FP-18` `AC=4/8`）在 R11 关闭 | Codex R10 review |
| R11 (Codex) | `plan-20261117-fetch-push.md` R11 | `FAIL` | 1 项 P1（`FP-20` 验证命令需显式校验 `^TC-1338 PASS$` 输出而不仅靠退出码 0）在 R12 关闭 | — | Codex R11 review |
| R11 (Claude) | `plan-20261117-fetch-push.md` R11 | `FAIL` | 2 项 P1（`FP-20` 新增 `ADR-FP-03` 仅在显式配置 `fetch.unpackLimit` 时解包并补 `T-1`/GC-08/GC-10；`FP-18` 补 `#863` 跟踪分支推进 AC 与 `DEP-FP-06` `blocked` 契约）均在 R12/R13 关闭 | P2（`21 张`、`Phase 2` `FP-21`、`LBR-CLI-003`、`FP-11` 原始错误码、`Plan Author 5`、`auto-gc` 范围）在 R13 关闭 | Claude R11 review |
| R13 (Claude) | `plan-20261117-fetch-push.md` R13 | `FAIL` | 1 项 P1（`FP-14` 补 `#879`/`TC-1399` stderr `<src> -> <dst>` 与 `--no-porcelain` stdout 为空）在 R14 关闭 | P2（`FP-11` `could not fetch`/`GIT_TRACE`、`修订历史`、`FP-01` `1593-1599`）在 R14 关闭 | Claude R13 review |
| R13 (Codex) | `plan-20261117-fetch-push.md` R13 | `FAIL` | 1 项 P1（`FP-20` AC-1 与 `ADR-FP-03` 未配置行为矛盾）在 R14 关闭 | P2（`FP-18` `split-from` 双向注记）在 R14 关闭 | Codex R13 review |
| R14 (Claude) | `plan-20261117-fetch-push.md` R14 | `ERROR` | 网络连接中断（`ECONNRESET`），合并至 R15 重审 | — | Claude R14 review |
| R14 (Codex) | `plan-20261117-fetch-push.md` R14 | `FAIL` | 1 项 P1（`FP-03` 补齐 `#683` `t/t3200-branch.sh#188` / `TC-0180` 门禁命令）在 R15 关闭 | — | Codex R14 review |
| R15 (Codex) | `plan-20261117-fetch-push.md` R15 | `FAIL` | 1 项 P1（`FP-03` 修正为 `tc0164-0189-branch-upstream.sh 0180`）在 R16 关闭 | — | Codex R15 review |
| R16 (Claude) | `plan-20261117-fetch-push.md` R16 | `ERROR` | 网络连接中断（`ECONNRESET`），合并至 R17 重审 | — | Claude R16 review |
| R16 (Codex) | `plan-20261117-fetch-push.md` R16 | `FAIL` | 1 项 P1（`TC-0180` 依赖 `plan-20261117-branch.md` 的 `branch --delete-merged`，已新增 `DEP-FP-07` 交由 `branch` 计划闭环并收敛 `FP-03` 独立验收为 `VER=2/8`）在 R17 关闭 | — | Codex R16 review |
| R17 (Codex) | `plan-20261117-fetch-push.md` R17 | `FAIL` | 1 项 P1（`branch` 计划未承接 `TC-0180`，已将 `DEP-FP-07` 改为 `incoming` 并在本计划新增 `FP-22` 显式闭环 `#683` `TC-0180`）在 R18 关闭 | — | Codex R17 review |
| R18 (Codex) | `plan-20261117-fetch-push.md` R18 | `FAIL` | 2 项 P1（移除违反 G-02 的测试拆卡 `FP-22` 并由 `FP-03` 独立交付 `push` 侧行为；将依赖本批次之外 `branch --delete-merged` 的完整 `TC-0180` 脚本显式登记为 `DEFER-FP-02` + `DEP-FP-07` `outgoing`）均在 R19 关闭 | P2（`完成判据` 明确非延后任务范围）在 R19 关闭 | Codex R18 review |
| R19 (Codex) | `plan-20261117-fetch-push.md` R19 | `FAIL` | 2 项 P1（`ADR-FP-03`/`FP-20` 补齐 `GC-08` 多对象解包事务边界与崩溃恢复、`成功定义`/`DEFER-FP-02` 明确 `#683` `TC-0180` 归口 `CLI / Git-compat 维护者`）均在 R20 关闭 | P2（展开全卡 `Docs and compatibility impact` `EN+zh` 为逐文件显式路径）在 R20 关闭 | Codex R19 review |
| R20 (Codex) | `plan-20261117-fetch-push.md` R20 | `FAIL` | 1 项 P1（`ADR-FP-03`/`FP-20` 对齐 Git 默认 `fetch.unpackLimit = 100` 仅作用于 `libra fetch` CLI）在 R21 关闭 | P2（`FP-20` `AC=2/8`、行 5 `R21`）在 R21 关闭 | Codex R20 review |
| R21 (Codex) | `plan-20261117-fetch-push.md` R21 | `FAIL` | 2 项 P1（`ADR-FP-03`/`FP-20` 改用 `path::try_objects()` 并持有 `install.lock`/租约直至 `FETCH_HEAD` 提交以防并发 GC，补并发 GC 与 linked worktree 测试）在 R22 关闭 | — | Codex R21 review |
| R22 (Codex) | `plan-20261117-fetch-push.md` R22 | `FAIL` | 2 项 P1（`FP-20` 接收路径修正为 `fetch.rs:2647` `write_pack_and_index`，松散解包并发 GC 保护改用 `MaintenanceLock::shared` 与 `gc.rs:433` 对齐并补并发 `gc --prune=now` 测试）在 R23 关闭 | — | Codex R22 review |
| R23 (Claude) | `plan-20261117-fetch-push.md` R23 | `ERROR` | 网络中断（与 R22 同次未成行） | — | — |
| R23 (Codex) | `plan-20261117-fetch-push.md` R23 | `FAIL` | 1 项 P1（`ADR-FP-03`/`FP-20` 松散解包路径补齐等价 `PackCommitEdges` 与 `validate_network_history` 校验及残缺网络历史拒绝回归测试）在 R24 关闭 | — | Codex R23 review |
| R24 (Claude) | `plan-20261117-fetch-push.md` R24 | `ERROR` | 网络中断（`ECONNRESET`），合并至 R25 重审 | — | Claude R24 review |
| R24 (Codex) | `plan-20261117-fetch-push.md` R24 | `FAIL` | 1 项 P1（修正为先在 `MaintenanceLock::shared` 下原子 `rename` 落盘使 `configured_storage` 可见，再运行 `validate_network_history` 并于通过后发布 refs）在 R25 关闭 | P2（`gc.rs` 在 `MaintenanceLock::try_exclusive` 下清理过期 `tmp_unpack_*` 目录并补有效/残缺小网络 pack 测试）在 R25 关闭 | Codex R24 review |
| R25 (Claude) | `plan-20261117-fetch-push.md` R25 | `FAIL` | 3 项 P1（`FP-18` `cli.rs` post-dispatch 释放共享锁后触发 `gc --auto` 防自锁死、`ADR-FP-03`/`FP-20` 复用 `cli.rs:3718` 锁键且失败不在共享锁下删对象、`FP-20` 改经 `ClientStorage::put` 写入以保分层后端与 object-index）均在 R26 关闭 | 4 项 P2（修正 `validate_response_shallow_updates`/`validate_fetched_history` 锚点、补 `FP-20` `gc.rs`/`gc` 文档、明确 `tmp_unpack_*` 截止时间与零候选不早退、`AC=7/8`）在 R26 关闭 | Claude R25 review |
| R25 (Codex) | `plan-20261117-fetch-push.md` R25 | `FAIL` | 3 项 P1（`ADR-FP-03`/`FP-20` 复用 `cli.rs` `repo_path` 锁键、失败不删内容寻址对象防并发 `fetch` 悬空、`gc.rs` 在无松散候选时不早退跳过 `tmp_unpack_*` 清理）均在 R26 关闭 | — | Codex R25 review |
| R26 (Claude) | `plan-20261117-fetch-push.md` R26 | `ERROR` | 网络中断（`ECONNRESET`），合并至 R27 重审 | — | Claude R26 review |
| R26 (Codex) | `plan-20261117-fetch-push.md` R26 | `FAIL` | 1 项 P1（`FP-20` `Verification` 显式补充 `cutoff == Never` 回归测试场景）在 R27 关闭 | P2（`FP-20` `AC=7/8` 校准为 `6/8`）在 R27 关闭 | Codex R26 review |
| R30-A (Codex gpt-6.1-terra) | 当时计划第 1–400 行、FP-04/05/06/21 及关联测试、计数、发布与镜像合同；其余卡未完整审查 | `FAIL` | FP-21 陈旧更新/删除与空期望创建/拒绝拆为独立谓词；FP-07 增加镜像更新、prune 保留/删除 Git 对照 | 后续 FP-07 粒度与具名门修订；不得据此推定其余卡通过 | 原始输出：/Users/eli/.pi/agent/sessions/--Volumes-Data-gitmono-libra--/subagent-artifacts/8596a6d6-037c-4b94-b56b-5a3555b9ce8c_codex-terra-review_output.md |
| R31-A (Codex gpt-6.1-terra) | 当时计划第 1–400 行及 FP-04/05/06/21、FP-07 完整卡；其余任务卡未完整复审 | `FAIL` | 按有效 prune 与配置/CLI 优先级限定删除并拆分验收；FP-07 原 AC=2/8 计数不足，后续按独立谓词重算 | 后续 FP-07 配置矩阵修订 | 原始输出：/Users/eli/.pi/agent/sessions/--Volumes-Data-gitmono-libra--/subagent-artifacts/05334ac5-d0a8-42a7-9adf-bcd3b0c65711_codex-terra-review_output.md |
| R32-A (Codex gpt-6.1-terra) | 当时计划第 1–400 行及 FP-04/05/06/21、FP-07 完整卡；其余任务卡未完整复审 | `FAIL` | 固定 CLI last-wins、remote 优先于 global、默认 false，补双向冲突与顺序对照 | 后续具名门与审计修订 | 原始输出：/Users/eli/.pi/agent/sessions/--Volumes-Data-gitmono-libra--/subagent-artifacts/fcb1d2f2-2bce-47b3-8eb6-ffa2fb085aec_codex-terra-review_output.md |
| R33-A (Codex gpt-6.1-terra) | 未核实（缺少可独立证明 R33-A 范围与发现的原始输出） | 未核实 | 未核实；不得以 R31-A 输出代证 FP-05 发现 | 后续仅以 R34-A 及之后可核验复审处理 FP-05/FP-07，R33-A 不作为关闭证据 | 原始输出未找到，范围、结论、发现均标为未核实 |
| R34-A (Codex gpt-6.1-terra) | FP-07/FP-06/FP-03/04/05/21 定向复审 | `FAIL` | FP-07 AC 低估、FP-06 无 force lease 安全回归不足；补 FP-07 豁免、FP-06 回归、prune 配置矩阵 | 后续 R47-A/R50-B 分区复核，见下两行；不表示当前字节全计划通过 | 原始输出：/Users/eli/.pi/agent/sessions/--Volumes-Data-gitmono-libra--/subagent-artifacts/1ec05e4a-7a46-4070-8010-1abe80d4315f_codex-terra-review_output.md |
| R47-A (Codex gpt-6.1-terra) | FP-07 指定范围分区复核 | PASS（仅指定范围） | 覆盖 FP-07 当前门族与计数；不证明其他分区或实现测试通过 | 后续修改须按当前字节重新复审 | 原始输出：/Users/eli/.pi/agent/sessions/--Volumes-Data-gitmono-libra--/subagent-artifacts/89724cb4-5454-451e-9ec1-98a10c2cccfc_codex-terra-review_output.md；受审 SHA `ca94c30b20ba1f4acb85affa651d297a8a1e4b649af45a0eb3fb40ffabbf048a` |
| R50-B (Codex gpt-6.1-terra) | 第 1–800 行分区复核 | PASS（仅该历史分区） | 该历史分区无阻塞发现；不覆盖后续修订或其他分区 | 当前字节按 R158+ 继续复审 | 原始输出：/Users/eli/.pi/agent/sessions/--Volumes-Data-gitmono-libra--/subagent-artifacts/38e692ee-5073-4af3-8363-13613ba2398c_codex-terra-review_output.md |
| R1 (Claude subagent claude-opus-5-5) | 全文（受审 SHA-256 `ef9fe264659d080d9df0b6507971f533c876fd930141cdd0fa8d83aaa9290f0e`） | `FAIL` | 7 项 P1（FP-07 Git 默认 remote 规则与 AC8 指向、FP-01 `remote_get` 引用、匿名 fetch「只写 FETCH_HEAD」不实、ADR-FP-04 松散对象阈值误称对齐 Git、`Full-suite trigger` 嵌套反引号、元组合并掩盖 AC 计数）均在 R2 字节修订 | 审计表裸 `<name>`、`FetchArgs`/`cli.rs` 行号已修 | 本轮修订历史行 |
| R1-A (Codex gpt-6.1-terra) | 非卡片章节 + 审计表（受审 SHA-256 `ef9fe264659d080d9df0b6507971f533c876fd930141cdd0fa8d83aaa9290f0e`） | `FAIL` | 7 项 P1（From URL 脱敏、松散对象阈值、元组计数、共享回归组计入 VER、`R` 快照缺 reflog 列、FP-07 AC8 指向、列宽算法）均在 R2 字节修订 | — | 本轮修订历史行 |
| R1-B (Codex gpt-6.1-terra) | FP-01、FP-23、FP-02、FP-04、FP-03、FP-05、FP-06（受审 SHA-256 `ef9fe264659d080d9df0b6507971f533c876fd930141cdd0fa8d83aaa9290f0e`） | `FAIL` | 3 项 P1（FP-04 模式外保留只抽查、FP-03 命令行覆盖可空操作通过、FP-01 Git 引用错位）均在 R2 字节修订 | P2（FP-01 配置不变断言）已采纳为 AC3 | 本轮修订历史行 |
| R1-C (Codex gpt-6.1-terra) | FP-21、FP-07、FP-08、FP-09、FP-10、FP-19、FP-11（受审 SHA-256 `ef9fe264659d080d9df0b6507971f533c876fd930141cdd0fa8d83aaa9290f0e`） | `FAIL` | 7 项 P1（FP-21 AC7 缺源分支、FP-07 AC8 指向、FP-19 未推进默认 remote、FP-09「恰为」未整表比较、FP-21/FP-19 跨卡夹具、FP-08 告警未验收、FP-11 诊断误标 Git 期望）均在 R2 字节修订 | — | 本轮修订历史行 |
| R1-D (Codex gpt-6.1-terra) | FP-12..FP-18（受审 SHA-256 `ef9fe264659d080d9df0b6507971f533c876fd930141cdd0fa8d83aaa9290f0e`） | `FAIL` | 5 项 P1（FP-15 保留只抽查与计数、FP-18 元组超限、FP-18 直接 `gc --auto` 未建两 pack、FP-17 裸镜像内提交不可构造）均在 R2 字节修订（FP-18 拆出 FP-24） | P2（FP-12 `--multiple`）已采纳为 AC8 | 本轮修订历史行 |
| R2 (Claude subagent claude-opus-5-5) | 全文（受审 SHA-256 `9bde2b6fc0bcca158d814807a36d4c0cf9b6c0cedab057ad154f5255b74d46a8`） | `FAIL` | 3 项 P1（FP-03 夹具推 OID 不可构造、FP-09 `for-each-ref` 显示缺陷使 `RefMap` 断言不可达、FP-10 AC3 以字面 `../one` 归属 FETCH_HEAD）均在 R3 字节修订 | `codeql.yml` 触发写准 | 本轮修订历史行 |
| R2-A (Codex gpt-6.1-terra) | 非卡片章节 + 审计表（受审 SHA-256 `9bde2b6fc0bcca158d814807a36d4c0cf9b6c0cedab057ad154f5255b74d46a8`） | `FAIL` | 5 项 P1（失败映射未要求非 0 退出、FP-11 AC7 混入失败判据、FP-18 AC3 合并两输出流、summary 列宽固定 17 不实、`--verbose` 已是最新行被抑制）均在 R3 字节修订 | — | 本轮修订历史行 |
| R2-B (Codex gpt-6.1-terra) | FP-01..FP-06 区间（受审 SHA-256 `9bde2b6fc0bcca158d814807a36d4c0cf9b6c0cedab057ad154f5255b74d46a8`） | `FAIL` | 3 项 P1（FP-03 夹具推 OID、FP-04 未验源侧负向过滤、FP-01/FP-02 缺另一种仓库形态）均在 R3 字节修订 | — | 本轮修订历史行 |
| R2-C (Codex gpt-6.1-terra) | FP-21..FP-11 区间（受审 SHA-256 `9bde2b6fc0bcca158d814807a36d4c0cf9b6c0cedab057ad154f5255b74d46a8`） | `FAIL` | 3 项 P1（FP-09 `for-each-ref` 读不出自定义命名空间、FP-11 AC7 计数、失败映射未要求非 0 退出）均在 R3 字节修订 | — | 本轮修订历史行 |
| R2-D (Codex gpt-6.1-terra) | FP-12..FP-18 区间（受审 SHA-256 `9bde2b6fc0bcca158d814807a36d4c0cf9b6c0cedab057ad154f5255b74d46a8`） | `FAIL` | 2 项 P1（FP-14/FP-16 失败映射可被 exit 0 通过、FP-12 AC5 映射多出内容判据）均在 R3 字节修订 | P2（FP-18 `--dry-run`）已采纳为 AC3 | 本轮修订历史行 |
| R3 (Claude subagent claude-opus-5-5) | 全文（受审 SHA-256 `ffc13a70d6f76a897515a54315d55ae31ef54e81e9e6ab91620edce2079dd181`） | `FAIL` | 2 项 P1（FP-05 AC2 文本「含」与映射「含行」不一致且与 Git `remote: ` 前缀冲突、多卡夹具未写明无 tag 而断言精确新增集合）均在 R4 字节修订 | FP-18 AC3 文本重复夹具步骤（已随 AC3 改写消除） | 本轮修订历史行 |
| R3-A (Codex gpt-6.1-terra) | 非卡片章节 + 审计表（受审 SHA-256 `ffc13a70d6f76a897515a54315d55ae31ef54e81e9e6ab91620edce2079dd181`） | `FAIL` | 4 项 P1（裸仓库 `FETCH_HEAD` 路径、`RefMap` 丢弃真实 `HEAD` 名引用、FP-01 组合快照计数、「风险登记」引用悬空）均在 R4 字节修订 | — | 本轮修订历史行 |
| R3-B (Codex gpt-6.1-terra) | FP-01..FP-06 区间（受审 SHA-256 `ffc13a70d6f76a897515a54315d55ae31ef54e81e9e6ab91620edce2079dd181`） | `FAIL` | 2 项 P1（FP-02 同名 remote 未配置、FP-04 `show-ref` 枚举盲区）均在 R4 字节修订（夹具前置断言保证枚举完整） | — | 本轮修订历史行 |
| R3-C (Codex gpt-6.1-terra) | FP-21..FP-11 区间（受审 SHA-256 `ffc13a70d6f76a897515a54315d55ae31ef54e81e9e6ab91620edce2079dd181`） | `FAIL` | 2 项 P1（FP-09 AC3 `extra_dir` 引用不被 `show-ref` 枚举、FP-10/FP-11 `FETCH_HEAD` URL 与已配置值不符）均在 R4 字节修订 | — | 本轮修订历史行 |
| R3-D (Codex gpt-6.1-terra) | FP-12..FP-18 区间（受审 SHA-256 `ffc13a70d6f76a897515a54315d55ae31ef54e81e9e6ab91620edce2079dd181`） | `FAIL` | 3 项 P1（FP-13 `--dry-run --no-write-fetch-head` 优先级、FP-18 dry-run 只比 pack 数、FP-18 静默契约未覆盖 stderr）均在 R4 字节修订 | — | 本轮修订历史行 |
| R4 (Claude subagent claude-opus-5-5) | 全文（受审 SHA-256 `a47fe1b18bdd4c121427bd9b8f02676b9ec385d5b09087b2c871e1d09b6a4a6b`） | `FAIL` | 1 项 P1（FP-12 AC3 无法区分重写同内容的错误实现）在 R5 字节修订 | FP-03 通配符展开须忽略内部分支行（由精确新增集合约束） | 本轮修订历史行 |
| R4-A (Codex gpt-6.1-terra) | 非卡片章节 + 审计表（受审 SHA-256 `a47fe1b18bdd4c121427bd9b8f02676b9ec385d5b09087b2c871e1d09b6a4a6b`） | `FAIL` | 3 项 P1（ADR-FP-05 与 FP-13 的 dry-run 优先级矛盾、`RefMap` 合成 HEAD 判别依据缺失、M-INT 覆盖描述不实）均在 R5 字节修订 | — | 本轮修订历史行 |
| R4-B (Codex gpt-6.1-terra) | FP-01..FP-06 区间（受审 SHA-256 `a47fe1b18bdd4c121427bd9b8f02676b9ec385d5b09087b2c871e1d09b6a4a6b`） | `FAIL` | 1 项 P1（拆出卡 FP-23 未置于所属 Phase 末尾）在 R5 字节修订 | — | 本轮修订历史行 |
| R4-C (Codex gpt-6.1-terra) | FP-21..FP-11 区间（受审 SHA-256 `a47fe1b18bdd4c121427bd9b8f02676b9ec385d5b09087b2c871e1d09b6a4a6b`） | `FAIL` | 1 项 P1（FP-10 AC2「行不变」与 `RefMap` 映射不同谓词）在 R5 字节修订 | 2 项 P2（FP-08 预网络拒绝的网络访问观测、FP-10 实际并发度验证）登记为残余风险，具名接受人：计划协调者 | 本轮修订历史行 |
| R4-D (Codex gpt-6.1-terra) | FP-12..FP-18 区间（受审 SHA-256 `a47fe1b18bdd4c121427bd9b8f02676b9ec385d5b09087b2c871e1d09b6a4a6b`） | `FAIL` | 2 项 P1（FP-12 AC3 未产生 `FETCH_HEAD`、FP-13 静默规则与 `--verbose` 诊断冲突）均在 R5 字节修订 | — | 本轮修订历史行 |
| R5 (Claude subagent claude-opus-5-5) | 全文（受审 SHA-256 `ed1a7b65049dd0f7b6157e070ff8af79bfedc0e140f1c6df57475fba290ac3cc`） | `FAIL` | 1 项 P1（FP-24 「不运行 gc」只比 pack 文件名集合）在 R6 字节修订 | — | 本轮修订历史行 |
| R5-A (Codex gpt-6.1-terra) | 非卡片章节 + 审计表（受审 SHA-256 `ed1a7b65049dd0f7b6157e070ff8af79bfedc0e140f1c6df57475fba290ac3cc`） | `FAIL` | 1 项 P1（`COLUMNS` 的 Git `atoi` 语义）在 R6 字节修订 | — | 本轮修订历史行 |
| R5-B (Codex gpt-6.1-terra) | FP-01..FP-06（受审 SHA-256 `ed1a7b65049dd0f7b6157e070ff8af79bfedc0e140f1c6df57475fba290ac3cc`） | `FAIL` | 1 项 P1（FP-01 `RefMap` 看不到未配置 remote 名下的错误写入）在 R6 字节修订 | — | 本轮修订历史行 |
| R5-C (Codex gpt-6.1-terra) | FP-21、FP-23、FP-07..FP-11、FP-19（受审 SHA-256 `ed1a7b65049dd0f7b6157e070ff8af79bfedc0e140f1c6df57475fba290ac3cc`） | `FAIL` | 3 项 P1（FP-09 `extra_dir` 目标不可枚举、FP-21 表达式成功案例不能发现跳过租约、FP-10 未推进 `origin` 源）均在 R6 字节修订 | — | 本轮修订历史行 |
| R5-D (Codex gpt-6.1-terra) | FP-12..FP-18、FP-24（受审 SHA-256 `ed1a7b65049dd0f7b6157e070ff8af79bfedc0e140f1c6df57475fba290ac3cc`） | `PASS` | 无 | — | 本分区 PASS 只对 R5 字节有效 |
| R6 (Claude subagent claude-opus-5-5) | 全文（受审 SHA-256 `687b3bb6aec4f8d67ac1aa93d19590f0d44780ffafd83ea00044a1b584e24b3e`） | `FAIL` | 未独立发现新增 P1；核实并认同 Codex R6 各 P1，均在 R7 字节修订 | — | 本轮修订历史行 |
| R6-A (Codex gpt-6.1-terra) | 非卡片章节 + 审计表（受审 SHA-256 `687b3bb6aec4f8d67ac1aa93d19590f0d44780ffafd83ea00044a1b584e24b3e`） | `FAIL` | 2 项 P1（`RefMap` HEAD 依据过宽、`base.yml` PR 触发描述不准）均在 R7 字节修订 | — | 本轮修订历史行 |
| R6-B (Codex gpt-6.1-terra) | FP-01..FP-06（受审 SHA-256 `687b3bb6aec4f8d67ac1aa93d19590f0d44780ffafd83ea00044a1b584e24b3e`） | `FAIL` | 2 项 P1（FP-01/FP-02 夹具工作目录、FP-06 字符串租约）均在 R7 字节修订 | — | 本轮修订历史行 |
| R6-C (Codex gpt-6.1-terra) | FP-21、FP-23、FP-07..FP-11、FP-19（受审 SHA-256 `687b3bb6aec4f8d67ac1aa93d19590f0d44780ffafd83ea00044a1b584e24b3e`） | `FAIL` | 1 项 P1（FP-21 删除路径的表达式解析不可区分）在 R7 字节修订 | P2（FP-10 实际并发度）同 R4 登记残余风险 | 本轮修订历史行 |
| R6-D (Codex gpt-6.1-terra) | FP-12..FP-18、FP-24（受审 SHA-256 `687b3bb6aec4f8d67ac1aa93d19590f0d44780ffafd83ea00044a1b584e24b3e`） | `FAIL` | 1 项 P1（FP-24 缺省阈值 50 不可区分）在 R7 字节修订 | — | 本轮修订历史行 |
| R7 (Claude subagent claude-opus-5-5) | 全文（受审 SHA-256 `9501fa5b82d29cf2849ff6a9351828561929b7e6bc4eda6052652d2398ff2086`） | `FAIL` | 2 项 P1（FP-10 并发不可观测、ADR-FP-05 漏浅历史拒绝项排除）均在 R8 字节修订 | FP-21「远端不变」表述已改写 | 本轮修订历史行 |
| R7-A (Codex gpt-6.1-terra) | 非卡片章节 + 审计表（受审 SHA-256 `9501fa5b82d29cf2849ff6a9351828561929b7e6bc4eda6052652d2398ff2086`） | `FAIL` | 1 项 P1（列宽规则漏 `REF_STATUS_REJECT_SHALLOW`）在 R8 字节修订 | — | 本轮修订历史行 |
| R7-B (Codex gpt-6.1-terra) | FP-01..FP-06（受审 SHA-256 `9501fa5b82d29cf2849ff6a9351828561929b7e6bc4eda6052652d2398ff2086`） | `PASS` | 无 | P2（FP-05 未配置/未知值拒绝的直接回归）由共享回归组既有 `test_push_local_libra_nonbare_checked_out_branch_rejected` 部分覆盖，余量登记残余风险，具名接受人：计划协调者 | 本分区 PASS 只对 R7 字节有效 |
| R7-C (Codex gpt-6.1-terra) | FP-21、FP-23、FP-07..FP-11、FP-19（受审 SHA-256 `9501fa5b82d29cf2849ff6a9351828561929b7e6bc4eda6052652d2398ff2086`） | `FAIL` | 1 项 P1（FP-10 `--jobs` 可完全串行而通过）在 R8 字节修订（改为明确串行并延后真正并发） | P2（FP-21 stale 后远端快照）已改写 Description 去掉该表述 | 本轮修订历史行 |
| R7-D (Codex gpt-6.1-terra) | FP-12..FP-18、FP-24（受审 SHA-256 `9501fa5b82d29cf2849ff6a9351828561929b7e6bc4eda6052652d2398ff2086`） | `PASS` | 无 | — | 本分区 PASS 只对 R7 字节有效 |
| R8 (Claude subagent claude-opus-5-5) | 全文（受审 SHA-256 `380506545d1a441810c9b40d40593386ade65501be75819da72a8d5878368b5a`） | `FAIL` | 2 项 P1（FP-23 夹具 cwd 与执行协议冲突、FP-11 AC5 集合比较不能区分重复行）均在 R9 字节修订 | 性能摘要串行预算的验证列已改为设计约束 | 本轮修订历史行 |
| R8-A (Codex gpt-6.1-terra) | 非卡片章节 + 审计表（受审 SHA-256 `380506545d1a441810c9b40d40593386ade65501be75819da72a8d5878368b5a`） | `PASS` | 无 | P2（串行预算无传输重叠观测）已把性能摘要验证列改为设计约束，并发观测随 `DEFER-FP-05` 承接 | 本分区 PASS 只对 R8 字节有效 |
| R8-B (Codex gpt-6.1-terra) | FP-01..FP-06（受审 SHA-256 `380506545d1a441810c9b40d40593386ade65501be75819da72a8d5878368b5a`） | `PASS` | 无 | — | 本分区 PASS 只对 R8 字节有效 |
| R8-C (Codex gpt-6.1-terra) | FP-21、FP-23、FP-07..FP-11、FP-19（受审 SHA-256 `380506545d1a441810c9b40d40593386ade65501be75819da72a8d5878368b5a`） | `FAIL` | 2 项 P1（FP-23 夹具「全部命令」cwd 约束、FP-11 AC5 映射用集合比较）在 R9 字节修订 | — | 本轮修订历史行 |
| R8-D (Codex gpt-6.1-terra) | FP-12..FP-18、FP-24（受审 SHA-256 `380506545d1a441810c9b40d40593386ade65501be75819da72a8d5878368b5a`） | `PASS` | 无 | — | 本分区 PASS 只对 R8 字节有效 |
| R9 (Claude subagent claude-opus-5-5) | 全文（受审 SHA-256 `1fa843a6db6811f354d45bb60d5a4b3b048b11d2efbfd8464e279ae532441cdd`） | `PASS` | 无 | — | 同轮 Codex A/C 发现的 3 项 P1 已在 R10 字节修订，R9 字节不提交 |
| R9-A (Codex gpt-6.1-terra) | 非卡片章节 + 审计表（受审 SHA-256 `1fa843a6db6811f354d45bb60d5a4b3b048b11d2efbfd8464e279ae532441cdd`） | `FAIL` | 1 项 P1（故障恢复矩阵要求租约拒绝后 `R` 不变而 `FP-21` 未断言）在 R10 字节修订（`FP-21` AC3/AC5、`FP-25` AC3） | — | 本轮修订历史行 |
| R9-B (Codex gpt-6.1-terra) | FP-01..FP-06（受审 SHA-256 `1fa843a6db6811f354d45bb60d5a4b3b048b11d2efbfd8464e279ae532441cdd`） | `PASS` | 无 | — | 本分区 PASS 只对 R9 字节有效 |
| R9-C (Codex gpt-6.1-terra) | FP-21、FP-23、FP-07..FP-11、FP-19（受审 SHA-256 `1fa843a6db6811f354d45bb60d5a4b3b048b11d2efbfd8464e279ae532441cdd`） | `FAIL` | 2 项 P1（`FP-21` 空期望拒绝夹具因非快进本就被拒而不可区分、`FP-10` 删除旧 `FETCH_HEAD` 后无法观测截断）在 R10 字节修订（拆出 `FP-25`、AC3 预写旧记录） | — | 本轮修订历史行 |
| R9-D (Codex gpt-6.1-terra) | FP-12..FP-18、FP-24（受审 SHA-256 `1fa843a6db6811f354d45bb60d5a4b3b048b11d2efbfd8464e279ae532441cdd`） | `PASS` | 无 | — | 本分区 PASS 只对 R9 字节有效 |
| R10 (Claude subagent claude-opus-5-5) | 全文（受审 SHA-256 `ad01616f64825460b5458e3472ac3c82d2d2ce35e6504c4597f35f2ea81455c7`） | `PASS` | 无 | — | 同字节 Codex R11-C 发现的 P1 已在 R12 字节修订，R10 字节不提交 |
| R10-A (Codex gpt-6.1-terra) | 非卡片章节 + 审计表（受审 SHA-256 `ad01616f64825460b5458e3472ac3c82d2d2ce35e6504c4597f35f2ea81455c7`） | `PASS` | 无 | — | 本分区 PASS 只对 R10 字节有效 |
| R10-B (Codex gpt-6.1-terra) | FP-01..FP-06（受审 SHA-256 `ad01616f64825460b5458e3472ac3c82d2d2ce35e6504c4597f35f2ea81455c7`） | `PASS` | 无 | P2（FP-05 未配置/未知值直接回归）同 R7-B 登记 | 本分区 PASS 只对 R10 字节有效 |
| R10-C (Codex gpt-6.1-terra) | FP-21、FP-23、FP-25、FP-07..FP-11、FP-19（受审 SHA-256 `ad01616f64825460b5458e3472ac3c82d2d2ce35e6504c4597f35f2ea81455c7`） | `FAIL` | 未报 P0/P1（仅以核验覆盖不完整为由给出 FAIL）；按评审合同「PASS 当且仅当无 P0/P1」在同一字节上复审为 R11-C | P2（FP-09「以 `HEAD` 结尾的保留目标」措辞可更精确）登记残余风险，具名接受人：计划协调者 | 同字节复审 R11-C |
| R11-C (Codex gpt-6.1-terra) | 分区 C 同字节复审（受审 SHA-256 `ad01616f64825460b5458e3472ac3c82d2d2ce35e6504c4597f35f2ea81455c7`） | `FAIL` | 1 项 P1（`FP-23` AC5 只断言稳定码，无法区分补全后标签拒绝与未补全拒绝）在 R12 字节修订 | P2（`FP-10` `--jobs=0` 的 CPU 核数取值未直接验证）已在 R12 改为追踪行断言 | 本轮修订历史行 |
| R12 (Claude subagent claude-opus-5-5) | 全文（受审 SHA-256 `4a76a229239cbe4756bf5e92099ef42336e63e97aefff6e533f98177ed5b19d4`） | `PASS` | 无 | — | 同轮 Codex R12-A 发现的 P1 已在 R13 字节修订，R12 字节不提交 |
| R12-A (Codex gpt-6.1-terra) | 非卡片章节 + 审计表（受审 SHA-256 `4a76a229239cbe4756bf5e92099ef42336e63e97aefff6e533f98177ed5b19d4`） | `FAIL` | 1 项 P1（拆出 `FP-25` 的修订历史行「原卡 → 新卡」误记 `N/A`）在 R13 字节修订 | P2（列宽公式 `len` 应为显示宽度）已在 R13 写明 `utf8_strwidth` | 本轮修订历史行 |
| R12-B (Codex gpt-6.1-terra) | FP-01..FP-06（受审 SHA-256 `4a76a229239cbe4756bf5e92099ef42336e63e97aefff6e533f98177ed5b19d4`） | `PASS` | 无 | P2（`FP-01` 无 tag 夹具不能检出错误的标签自动跟随）登记残余风险，具名接受人：计划协调者 | 本分区 PASS 只对 R12 字节有效 |
| R12-C (Codex gpt-6.1-terra) | FP-21、FP-23、FP-25、FP-07..FP-11、FP-19（受审 SHA-256 `4a76a229239cbe4756bf5e92099ef42336e63e97aefff6e533f98177ed5b19d4`） | `PASS` | 无 | P2（`FP-11` AC4 未断言整体失败；`FP-10` 负值/未给出/非整数 `--jobs` 无专项 AC）登记残余风险，具名接受人：计划协调者 | 本分区 PASS 只对 R12 字节有效 |
| R12-D (Codex gpt-6.1-terra) | FP-12..FP-18、FP-24（受审 SHA-256 `4a76a229239cbe4756bf5e92099ef42336e63e97aefff6e533f98177ed5b19d4`） | `PASS` | 无 | P2（`FP-15` 仅由配置启用 prune 时的显式 refspec 作用域无卡特有 AC，由共享配置回归组部分覆盖）登记残余风险，具名接受人：计划协调者 | 本分区 PASS 只对 R12 字节有效 |
| R13 (Claude subagent claude-opus-5-5) | 全文（受审 SHA-256 `42c182093ef7a94fd58d768e63f34ce218c42665fabc03d5d9c2eeac747988c6`） | `PASS` | 无 | — | 同轮 Codex R13-A/C 发现的 3 项 P1 已在 R14 字节修订，R13 字节不提交 |
| R13-A (Codex gpt-6.1-terra) | 非卡片章节 + 审计表（受审 SHA-256 `42c182093ef7a94fd58d768e63f34ce218c42665fabc03d5d9c2eeac747988c6`） | `FAIL` | 1 项 P1（`W(r)` 只记内容摘要，漏检可执行位与文件类型变化）在 R14 字节修订 | — | 本轮修订历史行 |
| R13-B (Codex gpt-6.1-terra) | FP-01..FP-06（受审 SHA-256 `42c182093ef7a94fd58d768e63f34ce218c42665fabc03d5d9c2eeac747988c6`） | `PASS` | 无 | — | 本分区 PASS 只对 R13 字节有效 |
| R13-C (Codex gpt-6.1-terra) | FP-21、FP-23、FP-25、FP-07..FP-11、FP-19（受审 SHA-256 `42c182093ef7a94fd58d768e63f34ce218c42665fabc03d5d9c2eeac747988c6`） | `FAIL` | 2 项 P1（`FP-21` 删除路径只用简单分支名匹配、`FP-09`「以 `HEAD` 结尾的保留目标」与执行协议对嵌套名的表述冲突）在 R14 字节修订 | — | 本轮修订历史行 |
| R13-D (Codex gpt-6.1-terra) | FP-12..FP-18、FP-24（受审 SHA-256 `42c182093ef7a94fd58d768e63f34ce218c42665fabc03d5d9c2eeac747988c6`） | `PASS` | 无 | P2（`FP-15` 仅配置启用 prune 的显式 refspec 作用域）同 R12-D 登记 | 本分区 PASS 只对 R13 字节有效 |
| R14 (Claude subagent claude-opus-5-5) | 全文（受审 SHA-256 `a0fa23ae2945dc55ee9314d229eba883144814df5837fa8f18d26e965d6b4821`） | `PASS` | 无 | — | 同轮 Codex R14-A/D 发现的 3 项 P1 已在 R15 字节修订，R14 字节不提交 |
| R14-A (Codex gpt-6.1-terra) | 非卡片章节 + 审计表（受审 SHA-256 `a0fa23ae2945dc55ee9314d229eba883144814df5837fa8f18d26e965d6b4821`） | `FAIL` | 1 项 P1（ADR-FP-02 称 `--force` 覆盖「所有拒绝原因」，与接收端拒绝及 `FP-05` 冲突）在 R15 字节修订 | — | 本轮修订历史行 |
| R14-B (Codex gpt-6.1-terra) | FP-01..FP-06（受审 SHA-256 `a0fa23ae2945dc55ee9314d229eba883144814df5837fa8f18d26e965d6b4821`） | `PASS` | 无 | — | 本分区 PASS 只对 R14 字节有效 |
| R14-C (Codex gpt-6.1-terra) | FP-21、FP-23、FP-25、FP-07..FP-11、FP-19（受审 SHA-256 `a0fa23ae2945dc55ee9314d229eba883144814df5837fa8f18d26e965d6b4821`） | `PASS` | 无 | — | 本分区 PASS 只对 R14 字节有效 |
| R14-D (Codex gpt-6.1-terra) | FP-12..FP-18、FP-24（受审 SHA-256 `a0fa23ae2945dc55ee9314d229eba883144814df5837fa8f18d26e965d6b4821`） | `FAIL` | 2 项 P1（`FP-16` 保留 tag 只断言调用后值、`FP-24` 缺省阈值只测解析函数）在 R15 字节修订 | — | 本轮修订历史行 |
| R15 (Claude subagent claude-opus-5-5) | 全文（受审 SHA-256 `418e7d0053deaca61c51be4e3d4d110e6affdfabbc36538831e573269762269b`） | `PASS` | 无 | — | 同字节 Codex R15-A/D 发现的 2 项 P1 已在 R16 字节修订，R15 字节不提交 |
| R15-A (Codex gpt-6.1-terra) | 非卡片章节 + 审计表（受审 SHA-256 `418e7d0053deaca61c51be4e3d4d110e6affdfabbc36538831e573269762269b`） | `FAIL` | 1 项 P1（`FP-24` AC8 以列表合并 50/51 两次判定，实为两个独立谓词，AC 实为 9/8）在 R16 字节修订 | — | 本轮修订历史行 |
| R15-B (Codex gpt-6.1-terra) | FP-01..FP-06（受审 SHA-256 `418e7d0053deaca61c51be4e3d4d110e6affdfabbc36538831e573269762269b`） | `FAIL` | 未报 P0/P1（仅以核验覆盖不完整为由给出 FAIL）；R15 字节已因 A/D 分区的 P1 修订，不再对该字节复审，由 R16-B 在新字节上复审 | P2（`FP-05` 未直接覆盖未配置时的拒绝与 `ignore` 时工作区不变；未配置拒绝由共享回归组既有 `test_push_local_libra_nonbare_checked_out_branch_rejected` 回归）登记残余风险，具名接受人：计划协调者 | R16-B |
| R15-C (Codex gpt-6.1-terra) | FP-21、FP-23、FP-25、FP-07..FP-11、FP-19（受审 SHA-256 `418e7d0053deaca61c51be4e3d4d110e6affdfabbc36538831e573269762269b`） | `PASS` | 无 | P2（`FP-08` 未覆盖第二个目标在应用阶段失败时的整体不提交；`FP-07` 未覆盖读取分支/配置存储错误不回落）登记残余风险，具名接受人：计划协调者 | 本分区 PASS 只对 R15 字节有效 |
| R15-D (Codex gpt-6.1-terra) | FP-12..FP-18、FP-24（受审 SHA-256 `418e7d0053deaca61c51be4e3d4d110e6affdfabbc36538831e573269762269b`） | `FAIL` | 2 项 P1（`FP-17` AC4 断言的 `tag_one` 可经自动跟随取得、无法区分忽略 `--tags` 的实现；`FP-24` AC8 合并两个独立谓词）在 R16 字节修订 | P2（`FP-13` 测试辅助函数在被测 cwd 下创建的 `.libra-test-home/` 落入 `W`）已在 R16 由 `W(r)` 排除该目录修复 | 本轮修订历史行 |
| R16 (Claude subagent claude-opus-5-5) | 全文（受审 SHA-256 `63e52b8e608d4381beab4681907e52ca5d158a2f6b9c40c686266a30ef22fb9a`） | `PASS` | 无 | P2（`FP-19` 非法 `fetch.all` 值未直接覆盖）登记残余风险，具名接受人：计划协调者 | 同字节 Codex R16-A/C 发现的 3 项 P1 已在 R17 字节修订，R16 字节不提交 |
| R16-A (Codex gpt-6.1-terra) | 非卡片章节 + 审计表（受审 SHA-256 `63e52b8e608d4381beab4681907e52ca5d158a2f6b9c40c686266a30ef22fb9a`） | `FAIL` | 2 项 P1（`S(r)` 排除 `.lock` 文件、无法证明「无写入」；`FP-24` 缺省配置端到端只断言提示文本、无法发现绕过判定函数静默运行 gc 的实现）在 R17 字节修订 | — | 本轮修订历史行 |
| R16-B (Codex gpt-6.1-terra) | FP-01..FP-06（受审 SHA-256 `63e52b8e608d4381beab4681907e52ca5d158a2f6b9c40c686266a30ef22fb9a`） | `PASS` | 无 | — | 本分区 PASS 只对 R16 字节有效 |
| R16-C (Codex gpt-6.1-terra) | FP-21、FP-23、FP-25、FP-07..FP-11、FP-19（受审 SHA-256 `63e52b8e608d4381beab4681907e52ca5d158a2f6b9c40c686266a30ef22fb9a`） | `FAIL` | 1 项 P1（`FP-08` 称 `--set-upstream` 与多个 refspec 同用时「按 Git」告警，实际 Git 只按无本地目标的条目计数）在 R17 字节修订 | — | 本轮修订历史行 |
| R16-D (Codex gpt-6.1-terra) | FP-12..FP-18、FP-24（受审 SHA-256 `63e52b8e608d4381beab4681907e52ca5d158a2f6b9c40c686266a30ef22fb9a`） | `PASS` | 无 | — | 本分区 PASS 只对 R16 字节有效 |
| R17 (Claude subagent claude-opus-5-5) | 全文差异 + 全卡「不变」类断言脚本清扫（受审 SHA-256 `da78f5405e13375022a7bdc6765c54dbe021434cbc8e70f0f6c194bc70cf2ada`） | `PASS` | 无 | — | 同字节 Codex R17-D 发现的 1 项 P1 已在 R18 字节修订，R17 字节不提交 |
| R17-A (Codex gpt-6.1-terra) | 非卡片章节 + 审计表（受审 SHA-256 `da78f5405e13375022a7bdc6765c54dbe021434cbc8e70f0f6c194bc70cf2ada`） | `PASS` | 无 | — | 本分区 PASS 只对 R17 字节有效 |
| R17-B (Codex gpt-6.1-terra) | FP-01..FP-06（受审 SHA-256 `da78f5405e13375022a7bdc6765c54dbe021434cbc8e70f0f6c194bc70cf2ada`） | `PASS` | 无 | — | 本分区 PASS 只对 R17 字节有效 |
| R17-C (Codex gpt-6.1-terra) | FP-21、FP-23、FP-25、FP-07..FP-11、FP-19（受审 SHA-256 `da78f5405e13375022a7bdc6765c54dbe021434cbc8e70f0f6c194bc70cf2ada`） | `PASS` | 无 | P2（`FP-08` 应用阶段失败的事务回滚、`FP-07` 存储错误不回落）同 R15-C 登记 | 本分区 PASS 只对 R17 字节有效 |
| R17-D (Codex gpt-6.1-terra) | FP-12..FP-18、FP-24（受审 SHA-256 `da78f5405e13375022a7bdc6765c54dbe021434cbc8e70f0f6c194bc70cf2ada`） | `FAIL` | 1 项 P1（`FP-24` 夹具未设 `fetch.unpackLimit`，钉选 Git 的同构夹具会把小 pack 解包，造不出 50/51 个 pack，「Git 同夹具期望」无法复核）在 R18 字节修订 | — | 本轮修订历史行 |
| R18 (Claude subagent claude-opus-5-5) | 全文差异 + 全卡「Git 同夹具期望」缺省配置对照（受审 SHA-256 `bcf2de50e44e7c9b2c6ea6b1ed5af3cf6c2ec9a46c654f86d05cde93a0a36c62`） | `PASS` | 无 | — | 同字节 Codex R18-A/C 发现的 2 项 P1 已在 R19 字节修订，R18 字节不提交 |
| R18-A (Codex gpt-6.1-terra) | 非卡片章节 + 审计表（受审 SHA-256 `bcf2de50e44e7c9b2c6ea6b1ed5af3cf6c2ec9a46c654f86d05cde93a0a36c62`） | `FAIL` | 1 项 P1（`DEP-FP-01` 写锁「于本卡推送后释放」，早于在 squash merge 与发布之后才执行的网站第 3–7 步，网站页写入失去互斥）在 R19 字节修订 | — | 本轮修订历史行 |
| R18-B (Codex gpt-6.1-terra) | FP-01..FP-06（受审 SHA-256 `bcf2de50e44e7c9b2c6ea6b1ed5af3cf6c2ec9a46c654f86d05cde93a0a36c62`） | `PASS` | 无 | — | 本分区 PASS 只对 R18 字节有效 |
| R18-C (Codex gpt-6.1-terra) | FP-21、FP-23、FP-25、FP-07..FP-11、FP-19（受审 SHA-256 `bcf2de50e44e7c9b2c6ea6b1ed5af3cf6c2ec9a46c654f86d05cde93a0a36c62`） | `FAIL` | 1 项 P1（`FP-10` 要求无条件截断 `FETCH_HEAD` 并归因于 Git，实际 Git 只在未给 `--append` 时截断，会破坏既有 `--append`）在 R19 字节修订 | — | 本轮修订历史行 |
| R18-D (Codex gpt-6.1-terra) | FP-12..FP-18、FP-24（受审 SHA-256 `bcf2de50e44e7c9b2c6ea6b1ed5af3cf6c2ec9a46c654f86d05cde93a0a36c62`） | `PASS` | 无 | — | 本分区 PASS 只对 R18 字节有效 |
| R19 (Claude subagent claude-opus-5-5) | 全文差异 + 全卡「与 Git 一致」类声明对照钉选源码（受审 SHA-256 `bda4c9f4798d2f5ab234ef57510daa794ba0b3800f4a3ae241de55095903380b`） | `PASS` | 无 | — | 与 Codex R19-A/B/C/D 对同一字节双审 PASS |
| R19-A (Codex gpt-6.1-terra) | 非卡片章节 + 审计表（受审 SHA-256 `bda4c9f4798d2f5ab234ef57510daa794ba0b3800f4a3ae241de55095903380b`） | `PASS` | 无 | — | 双审 PASS 字节 |
| R19-B (Codex gpt-6.1-terra) | FP-01..FP-06（受审 SHA-256 `bda4c9f4798d2f5ab234ef57510daa794ba0b3800f4a3ae241de55095903380b`） | `PASS` | 无 | P2（`FP-01` 无 tag 夹具未覆盖匿名 fetch 不自动跟随 tag；`FP-05` 未知配置值与 `ignore` 时工作区快照未直接覆盖）登记残余风险，具名接受人：计划协调者 | 双审 PASS 字节 |
| R19-C (Codex gpt-6.1-terra) | FP-21、FP-23、FP-25、FP-07..FP-11、FP-19（受审 SHA-256 `bda4c9f4798d2f5ab234ef57510daa794ba0b3800f4a3ae241de55095903380b`） | `PASS` | 无 | P2（`FP-11` 未单独断言 `--multiple` 部分失败时的整体失败；`FP-21` 未覆盖显式期望匹配的非快进更新）登记残余风险，具名接受人：计划协调者 | 双审 PASS 字节 |
| R19-D (Codex gpt-6.1-terra) | FP-12..FP-18、FP-24（受审 SHA-256 `bda4c9f4798d2f5ab234ef57510daa794ba0b3800f4a3ae241de55095903380b`） | `PASS` | 无 | — | 双审 PASS 字节 |

## 实施证据汇总

> 按 v2.12，执行、验收与收口证据（测试计数、SHA-256、run ID、门结果、网站部署哈希）统一写入本节或对应卡内，不在计划目录新建证据文件。

（尚未执行。）

## 非目标与延后项

| ID | 延后内容 | 原因 | 重启条件 | 承接位置 |
|---|---|---|---|---|
| DEFER-FP-01 | `libra clone` / `libra pull` 的参数解析与多 refspec | 分属同批其它计划 | 协调者排期 | 对应计划 |
| DEFER-FP-02 | `#683` 的 `TC-0180` 整序列（`t/t3200-branch.sh:2006`，`tc0164-0189-branch-upstream.sh 0180`） | `FP-03` 已交付 `push` 侧 `remote.<name>.push`；整序列依赖本批未立项的 `branch --delete-merged`（对照 `plan-20261117-branch.md#DEFER-BR-02`） | `branch --delete-merged` 落地 `main` 后 | 外部上游兼容回归计划（Owner: CLI / Git-compat 维护者；`DEP-FP-07`） |
| DEFER-FP-03 | `--prune-tags` 三态（`--no-prune-tags`）、`remote.<name>.pruneTags` 与 `fetch.pruneTags` 优先级、显式 refspec 下的 tag 修剪组合（原 `FP-15a`/`FP-15c`/`FP-15b`/`FP-15R`） | 不属于 18 个 issue 的验收条目；既有实现不变（`src/command/fetch.rs:2858-2870`） | 出现对应 issue 或上游用例需求时 | 新计划 |
| DEFER-FP-04 | `fetch.unpackLimit` 小包解包为松散对象（原 `FP-20`） | `#863` 场景不需要（ADR-FP-03）；Libra 接收即保留 pack，登记为有意差异 | 出现依赖该行为的 issue，或对象打包策略重构时 | 新计划 |
| DEFER-FP-05 | `fetch --jobs=<n>`（`n>1`）的真正并发执行 | `#867` 验收只要求 `--jobs`/`--multiple` 被接受、逐远程失败报告与追踪行（`FP-10`、`FP-11` 交付）；并发执行需要隔离进程级全局状态，超出本计划范围 | 出现并发性能需求或相关 issue 时 | 新计划 |

## 完成判据

- [ ] 全量收口门须在**最终代码树**上执行，并记录被测提交的 `libra rev-parse HEAD` 与 `libra rev-parse HEAD^{tree}` 及结果。「代码树」指 `docs/development/plan/` 以外的全部路径：其后若 `libra diff --name-only <被测提交> HEAD` 出现该目录以外的任何路径即须重跑；只改 `docs/development/plan/` 下计划/状态文件的纯文档记账提交（含把本门结果写入本计划的提交）不视为代码树变化、不触发重跑。终止链末端提交例外（v2.14，预登记）：本计划最终的纯文档记账提交的 D 组结果在推送后取得，不在该提交自身登记，其 SHA、run ID、结论与观察 UTC 时间由下一次 `plan-status.md` 例行同步补记。
- [ ] 计划文本提交门：Claude（Subagent）与 Codex 对同一受审字节均返回字面结论 `VERDICT: PASS` 且 P0/P1 全部关闭；网站交付另需独立 Claude Code `VERDICT: PASS`，两者互不替代。
- [ ] 网站交付门：`DEP-FP-08`（用户 2026-11-18 11:55:09 UTC 确认）仍有效；每张卡按「文档与网站验收命令」完成第 3–7 步（同步与漂移检查且网站页字节改变、前端验证、独立 Claude Code `PASS`、精确暂存与签名+DCO 提交、提交字节与已审字节一致、推送 `cf`、`<article>` HTML 逐字节比对的部署核验）；任一环节失败即 STOP 且不得标 `complete`。
- [ ] 所有任务卡满足粒度规则 `G-*`；「任务卡粒度审计表」已填齐。
- [ ] 所有非延后任务（全部 23 张现行卡；`DEFER-FP-*` 与「废弃编号」所列不在其内）`Lifecycle=done` **且** `Acceptance=complete`（ER-04），其 `Verification` 命令均已按「验证执行协议」运行并记录结果。
- [ ] 全部任务卡完成后运行 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` 全绿，且修复全量暴露的全部 Bug：定位到本计划的失败前滚修复，判定为既有失败的附「计划基线 commit 上同样失败」的复现证据并登记 `FIX-*` 或 `DEFER-*`，修复后重跑全量至全绿（ER-13）。
- [ ] 必要的 docs/compat/error-code/test-index 更新已完成（见「兼容与文档收口」）。
- [ ] 每张卡的 `Rollback mode`（`immutable-release`）已按其降级指引核验或记录不可验证的原因。
- [ ] Codex review 最终结论为 `PASS`，P0/P1 全部关闭；仅 P2 residual risk 允许保留，且有具名接受人。
- [ ] 每张 `independent` 发布卡已执行 `patch + 1`、版本面集合一致、`Cargo.lock` 仅由工具链刷新、提交推送及 `gh release create` 完成并取得 D 组证据（ER-08）。
- [ ] 「修订历史」已记录成稿后的全部规范性变更（G-09）；`plan-status.md` 已同步；`plan-long.md` 由协调者登记或明确 `N/A`。
- [ ] 本计划未新建证据文件或证据文件夹（v2.12）。
