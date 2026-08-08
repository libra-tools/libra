# Libra 计划模板

本文是 `docs/development/plan/` 下新建计划的标准模板。新计划应复制本文件结构，替换 `<...>` 占位符，并删除不适用的说明性文字；强制章节不得删除，不适用时写 `N/A` 和原因。

**模板版本:** `v2`（2026-07-28 起生效；本版新增任务卡粒度规则 `G-*`、`Task type` / `Implementation write set` / `Release write set` / `Rollback mode` / `Version increment` / `Granularity` 字段、依赖登记表、发布分组与并发窗口、修订历史）。

### 模板版本与迁移政策

- 生效日期之后**新建**的计划必须整份符合本版模板。
- 生效日期之前成稿的计划按**增量迁移**：只有本次被新增或做规范性修改的任务卡需要满足本版 `G-*` 与新增字段；未触碰的卡保持原样，不构成违规，也不要求整份回填。
- 存量计划整份迁移是一次独立的计划工作，必须单独立卡；不得作为其它任务的附带产物。
- 若某份存量计划因迁移成本暂时保留与本版冲突的口径（例如旧的 clippy 命令行、L/XL 卡），在该计划的「修订历史」登记一行例外与预期迁移时机即可。

## 使用规则

- 日期计划命名为 `plan-YYYYMMDD.md`，用于可执行的实现、迁移、发布或文档收敛任务。
- 长期能力只进入 `plan-long.md`。日期计划可以链接长期能力编号，但不得把长期路线图复制成重复任务表。
- 每个计划必须以当前 checkout 的源码、测试、用户文档和兼容矩阵为事实基线。历史计划、截图、会议记录或竞品描述只能作为线索，不能作为已实现证据。
- 每个任务卡必须能交给 Agent 独立执行：范围明确、依赖明确、文件落点明确、验收标准明确、验证命令明确。
- 每个任务卡必须满足「任务卡粒度规则」全部 `G-*` 条款：单一可独立恢复的行为轴、条目与规模在上限内、默认一张卡一个发布切片。粒度不合格的卡不得进入开工态，必须先拆分或合并。
- 涉及公开命令、配置、schema、错误码、存储格式、网络协议、Agent 数据、迁移、权限或安全边界的计划，必须包含测试、文档、回滚和兼容处理。
- 若计划使用外部项目或竞品作为参照，必须 pin 具体 revision、文件路径和核对日期；不得把浮动 `main` 当作规范。
- 新增或重命名 `--test` target 时必须更新 `tests/INDEX.md`；`tests/compat/*` 下的文件还必须在 `Cargo.toml` 注册 `[[test]]` 并更新 `tests/compat/README.md`（未注册的 compat 文件根本不会运行）。只改动既有用例时，仅当索引行的描述失真才更新。
- 生产代码不得新增未解释的 `unwrap()`、`expect()` 或 `panic!()`；如确属不可失败逻辑，必须有 `// INVARIANT:` 注释并在任务验收中说明。

### 规范性 ID 与术语

计划正文引用规范条款时一律用具体 ID（例如「按 G-03 拆卡」），不要用「上一节」「前面那条」或会随条款增删失效的范围表述。新增条款时必须同步下表。

| 前缀 | 含义 | 定义位置 |
|---|---|---|
| `ER-*` | 执行检查必备需求（开工、验收、发布、证据） | 「执行检查必备需求」 |
| `GC-*` | 全局工程约束（对全部任务生效） | 「全局工程约束」 |
| `G-*` | 任务卡粒度规则 | 「任务卡粒度规则」 |
| `ADR-*` | 已决议设计决策 | 「已决议设计决策」 |
| `GAP-*` | 事实基线缺口 | 「当前缺口」 |
| `DEP-*` | 依赖登记项（含跨计划与外部前置） | 「依赖登记表」 |
| `REL-*` | 发布分组（含家族卡窗口） | 「发布分组与并发窗口」 |
| `EX-*` | 白名单内的规则 waiver（需具名审批） | 「字段全局默认与例外」 |
| `FIX-*` | 执行期发现的越界修复卡（ER-10） | 对应 Phase 末尾 |
| `DEFER-*` | 延后项 | 「非目标与延后项」 |
| `M-*` | 里程碑 | 「里程碑验收与回滚」 |

术语（全文统一，不要混用同义词）：

- **行为轴**：一个可独立恢复、对外语义自洽的变化方向（例如「SSE 协议版本协商」是一个轴，「SSE 背压阈值」是另一个轴）。
- **落点**：一个可枚举的代码或文档归属域，粒度为**一个具体目录**（如 `src/internal/ai/runtime/`）或**一组同名文档**（如 `docs/commands/<cmd>.md` EN+zh）。仓库根、`src/`、`tests/`、`docs/` 这类顶层目录**不算**一个落点。
- **写集**：会被修改的文件/目录集合，分三类（G-10）——**实现写集 I**（每卡字段，决定能否并发）、**发布写集 R**（每卡字段，版本面五处 + `Cargo.lock` + artifact；不用于实现阶段的并发分组，但进入发布窗口后按 I–R / R–R 规则串行化）、**协调写集 C**（计划级，发布顺序与窗口记录，不进任务卡字段、不参与并发判定；「禁止多 Agent 并发发布」是 ER-12 的仓库级规则，不是 C 的状态）。
- **发布切片**：一次独立的 review + 验收 + 版本 + 提交 + 推送。
- **家族卡**：共用唯一发布点的一组子卡（G-08）。
- **恢复模式（字段名 `Rollback mode`）**：`revert` / `forward-only` / `compensating` / `immutable-release` 四种之一（G-01）。不可逆变更用后三种表达，不要求「一次 revert 撤销」。

## 标题

`# <主题>计划（<YYYY-MM-DD>）`

## 文档职责

本文解决 `<问题/能力>`，目标是 `<可交付结果>`。

本文只规划任务，不宣称实现完成。落地时每个任务都必须先刷新源码锚点，再按任务卡验收。

### 适用范围

- `<包含的命令/模块/用户工作流>`
- `<包含的存储、schema、协议或 UI 表面>`
- `<包含的测试、文档、兼容矩阵或发布动作>`

### 非目标

- `<明确不做的能力>`
- `<延后到其它计划/RFC/ADR 的范围>`
- `<容易被误解但本计划不承诺的行为>`

### 成功定义

- `<用户或系统行为变化>`
- `<机器接口或数据状态变化>`
- `<文档、测试、发布证据>`
- `<何时可标记计划完成>`

## 事实基线

> 所有行号和源码锚点必须在开工当天刷新。过期锚点只能作为历史线索。

| 类别 | 当前事实 | 证据 |
|---|---|---|
| 代码入口 | `<src/...>` | `<file:line>` |
| 数据/状态 | `<SQLite table/ref/object/path>` | `<file:line>` |
| 用户命令 | `<libra ...>` | `<src/cli.rs:line>` |
| 机器输出 | `<--json/--machine/schema>` | `<file:line>` |
| 错误码 | `<LBR-...>` | `<src/utils/error.rs:line>` |
| 文档 | `<docs/...>` | `<file:line>` |
| 测试 | `<tests/...>` | `<target::test_fn>` |
| 外部参照 | `<repo@sha>` | `<path + date>` |

### 当前缺口

| ID | 缺口 | 影响 | 证据 | 计划动作 |
|---|---|---|---|---|
| GAP-01 | `<问题>` | `<用户/生产影响>` | `<file:line 或外部证据>` | `<任务 ID>` |

## 与其它计划的关系

| 计划/文档 | 关系 | 本计划处理 |
|---|---|---|
| `plan-long.md` | `<关联 LR/SB/UP 编号>` | `<链接、消费、更新状态或不触碰>` |
| `plan-YYYYMMDD.md` | `<前置/并行/替代/冲突>` | `<复用、不重做、迁移、关闭>` |
| `docs/development/...` | `<事实源或契约>` | `<同步方式>` |

## 评审结论与修订记录

计划成稿前必须从以下维度做一次自审；如果有阻断项，先修计划再开工。

| 维度 | 结论 | 修订动作 |
|---|---|---|
| 合理性 | `<目标是否值得做>` | `<调整>` |
| 可行性 | `<任务是否可拆、可交付>` | `<调整>` |
| 任务卡粒度 | `<是否存在多轴卡、L/XL 卡、碎片卡、未登记的合并发布>` | `<按 G-* 拆分/合并/登记例外>` |
| 依赖与顺序 | `<DAG 是否无环、是否缺边、发布顺序是否可执行>` | `<调整>` |
| 完整性 | `<测试/文档/迁移/回滚是否齐全>` | `<调整>` |
| 安全性 | `<权限、secret、路径、网络、模型输入>` | `<调整>` |
| 功能正确性 | `<状态机、边界条件、错误路径>` | `<调整>` |
| 接口兼容 | `<CLI/API/schema/JSON/错误码>` | `<调整>` |
| 数据流与控制流 | `<事务、CAS、幂等、并发>` | `<调整>` |
| 性能与容量 | `<热路径、复杂度、存储增长>` | `<调整>` |
| 可靠性与容错 | `<崩溃恢复、重试、资源释放>` | `<调整>` |
| 可维护性 | `<事实源、抽象边界、重复实现>` | `<调整>` |

### 修订历史

计划成稿后的每次规范性变更（任务卡拆分/合并、依赖调整、发布边界变化、决策反转）都必须在此登记一行；G-09 的拆分同步以本表为闭环凭证。

| 日期 | 触发 | 变更内容 | 原卡 → 新卡 | 受影响的引用 |
|---|---|---|---|---|
| `<YYYY-MM-DD>` | `<自审 / Codex R<n> / 现状核对>` | `<做了什么规范性修改>` | `<TASK-ID> → <TASK-ID>, <TASK-ID>` | `<实施顺序、依赖登记表、REL-*、追溯表、测试矩阵、里程碑、风险表>` |

## 已决议设计决策

实现时若需偏离本节，必须先修改计划并说明原因，不得在代码中静默改语义。

### ADR-<PREFIX>-01: <决策标题>

- **Status:** Accepted
- **Context:** `<为什么需要这个决策>`
- **Decision:** `<选定方案>`
- **Alternatives considered:** `<备选方案及拒绝理由>`
- **Consequences:** `<带来的约束、风险、后续工作>`
- **Revisit when:** `<何时应重审>`

## 全局工程约束

以下约束对本文所有任务生效。任务条目不再逐条重复，违反任一项即视为任务未完成。

- **GC-01 现状核实前置:** 每个任务开工前重新核对计划、相关开发文档、用户文档、当前代码和测试。如果已实现，则任务改为补测试、补文档、更新状态或关闭，不重复实现。
- **GC-02 单一事实源:** 状态机、schema、输出结构、权限策略、配置解析和共享 helper 必须有单一事实源。禁止 CLI、Web、Agent adapter 或测试 fixture 各自复制逻辑。
- **GC-03 Git 互操作与 Libra 扩展边界:** Git 兼容表面必须说明与 Git 的一致点和有意差异；Libra-only 表面必须说明替代工作流、用户影响和机器接口。
- **GC-04 输出与错误契约:** 用户可见错误使用稳定 `LBR-*` 码并同步 `docs/error-codes.md`。`--json`、`--machine`、退出码和人读输出必须分别验收。
- **GC-05 文档同步:** 命令或公开行为变化必须同步 `docs/commands/<cmd>.md`、`docs/commands/zh-CN/<cmd>.md`、相关 `docs/development/commands/*.md`、`COMPATIBILITY.md` 和测试索引。
- **GC-06 测试索引:** 新增或重命名 `--test` target 必须更新 `tests/INDEX.md`；`tests/compat/*` 新文件还必须在 `Cargo.toml` 注册 `[[test]]` 并更新 `tests/compat/README.md`。
- **GC-07 安全默认值:** 未满足认证、授权、路径归属、schema 版本、对象闭包、sandbox 或 secret redaction 前置时默认 fail-closed。任何 fail-open 必须有显式用户选择、日志和测试。
- **GC-08 原子性与恢复:** 修改 HEAD、refs、index、worktree registry、SQLite、D1/R2、对象库、Agent session/checkpoint 或发布状态时，必须定义事务边界、幂等键、崩溃窗口和回滚/前滚策略。
- **GC-09 并发与资源生命周期:** 锁顺序、CAS、lease、队列、子进程、连接池、临时目录和 watcher 必须有释放/恢复语义；测试不得依赖未隔离的全局状态。
- **GC-10 性能预算:** `status`、`diff`、`add`、`commit`、fetch/push、Agent hot path 和 Web/SSE 热路径不得引入无界扫描、无界内存或 N+1 网络/DB 调用。需要时写出数据规模和断言。
- **GC-11 生产 panic 禁止:** 生产路径不得新增裸 `unwrap()`、`expect()`、`panic!()`；必须用 `Result`、`anyhow::Context` 或领域错误返回可操作信息。
- **GC-12 精确暂存:** 提交前只 `libra add <相关路径>`，不得使用 `commit -a`。发现无关脏状态时保留并报告，不得清理、重置或混入提交。

## 执行检查必备需求（强制）

任一要求未满足，对应任务不得标记完成。条目使用稳定 ID，正文引用时用 ID 而不是序号，便于后续插入条目而不破坏交叉引用。

1. **ER-01 开工前安全检查:** 运行 `libra status --short --branch`（`--branch` 才会输出 `## <branch>...<upstream>` 行；不带它只有文件状态），确认当前分支与计划指定分支一致、工作区脏状态、目标文件是否已有无关改动。若目标文件已有未确认用户改动，先报告并避免覆盖。
2. **ER-02 先核对后实现:** 刷新本任务相关源码锚点、文档锚点、测试 target 和外部参照 revision，再决定实现、补测、补文档、关闭或降级。
3. **ER-03 粒度门禁:** 开工前按粒度规则 `G-*` 逐条复核本任务卡，并逐字段核对该卡的 `Granularity` 摘要行。若核对后发现范围已扩大（新增行为轴、AC/Verification 超限、scope 升到 L、写集与其它在跑任务重叠），先修改计划拆卡再开工，不得在实现中静默扩张任务范围。
4. **ER-04 每卡验收门:** 门由 **A 表面 focused 门**（按实际改动的表面）+ **B 类型门**（按 `Task type`）+ **C 发布收口门**（覆盖要求对所有非延后卡生效，执行归属见下）+ **D 远端后置门**（有不可本地复现的 CI 语义时）四组组成，**所有适用行累加**，全部通过才算验收。权威口径分层：`CLAUDE.md`「Quality Acceptance Criteria」的三门（`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && cargo test --all`）是**任何会推送的改动**的完成契约，模板不得削弱；任务卡指定的 focused 用例是在此之上的**附加**门，用来证明本卡行为，不是三门的替代品。`web/`、`worker/` 的命令以各自 `package.json` 的 `scripts` 为权威（本表按当前 scripts 列出，scripts 变更时以 `package.json` 为准并同批更新本表）。权威口径变更时必须同批同步 `CLAUDE.md`、`AGENTS.md`，以及**采用本模板当前版本的计划**（存量计划按「模板版本与迁移政策」处理，不因本条被追认为违规）。

   **门不计入条目上限：** 本条列出的门是全局强制门，**不计入**任务卡 `Verification` 的 G-03 条目计数；`Verification` 只登记本卡特有的判据（指定用例、新增守卫、手工证据）。

   **两个正交状态字段（都在任务卡登记）:**
   - `Lifecycle`（执行生命周期）：`pending` | `in-progress` | `blocked`（ER-10 的越界故障置此值）| `done`。
   - `Acceptance`（验收状态）：空 | `locally-accepted` | `remote-pending`（仅当本卡有适用的 D 组远端后置门）| `complete`。与 `CLAUDE.md` 的完成契约对齐 —— `CLAUDE.md` 规定任何 change 只有三门全绿才算 done，因此：
     - `locally-accepted` = 本卡适用的 **A 组 + B 组**门已过，但本卡的 **C 组覆盖**（自行执行或从承接卡继承，见下）尚未取得。此状态下**不得**对外报告「完成 / done」。
     - `remote-pending` = A/B 已过且 C 组覆盖已取得（含一次三门全绿的运行，其被测树状态包含本卡最终变更），但本卡适用或继承的 **D 组**远端后置门尚未全绿。此状态同样**不得**报告完成。
     - `complete` = 本卡的 **A + B** 已过、**C 组覆盖**已取得、且适用或继承的 **D 组**已全绿。无 D 时 C 覆盖到手即可 `complete`；有 D（含继承的 D）时必须先经 `remote-pending`。
     - 唯一状态转移路径：A/B 通过 → `locally-accepted` → ER-05 review PASS → 取得 C 组覆盖 →（无 D：`complete`；有 D：`remote-pending` → D 全绿 → `complete`）。
   - 两者独立取值：`blocked` 卡的 `Acceptance` 可以已是 `locally-accepted` 甚至 `complete`（例如变更已被三门覆盖，但仍卡在外部前置）。`Lifecycle=done` 必须以 `Acceptance=complete` 为前提；`blocked` 必须先回到 `in-progress` 并完成剩余动作才能进入 `done`，**不允许**从 `blocked` 直接标 `done`。计划完成门另要求所有非延后任务都到 `done`（见「完成判据」）。`Granularity` 里的 `complete=yes` 是 G-02 的结构完整性判据，与本字段无关，不可混用。

   **C 组覆盖与执行归属（每张非延后卡都必须取得 C 覆盖，但不都自己执行）:**
   - **独立发布卡（`Release boundary = independent`）与发布点卡（`release` / `family release point`）**：自行执行完整 C 组门。
   - **`family child`**：不 bump、不构建、不推送，**继承**其家族唯一发布点的 C 覆盖——前提是该发布点的三门运行其被测树状态包含本子卡的最终变更；同时继承该发布点适用的 D 组。
   - **`no-release` 卡（`docs` / `audit` / `spike` / `handoff`）**：**继承**任务卡显式声明的承载发布点（或计划收口点）的 C 覆盖与其 D 组；该承载点必须在卡内写明 ID，不得留空。
   - **`batch-release child`（任意 `Task type`，含 `implementation`）**：用于「逐卡提交并**推送**、全计划收口时统一发一次版本」的批量发布组（G-07a）。它**自行执行** C 组中的三门、提交与 `push`（因为它会推送，`CLAUDE.md` 的完成契约不可继承），**不** bump 版本面、**不**构建/安装、**不**打 tag、**不**产 artifact；版本面/artifact 这一段与 **D-02** 继承自其批量发布组的唯一发布点，**D-01 不继承**（每次推送各自触发，须自行取证）。其 `Acceptance` 在发布点完成前停在 `locally-accepted`。必须在卡内写明发布点 ID，并在「发布分组与并发窗口」登记该组。
   - 继承 D 组的卡同样要经过 `remote-pending`，直到被继承的 D 组证据全绿。
   - **不存在「用零命中守卫替代三门」的通道**——零命中守卫只用于证明这类卡未改代码，不改变其在取得 C 覆盖前仍是 `locally-accepted` 的事实。

   门分四组，全部适用者累加：**A 表面 focused 门**（按实际改动的表面，每个表面唯一命中一行，命中几行加几行）+ **B 类型门**（按 `Task type` 取一行）+ **C 发布收口门**（由会推送的卡执行，其覆盖可被 `family child` / `no-release` 卡继承）+ **D 远端后置门**（只在存在不可本地复现的 CI 语义时适用）。A/B/C 是本地可完成的门；D 只能在推送之后取得证据，因此**不阻塞** `locally-accepted` 与 ER-05 的 review 顺序。

   **A 表面 focused 门**（覆盖全部合法生产表面；`family child` 复用同一映射）：

   | 实际改动的表面 | focused 门 |
   |---|---|
   | Rust 内部单元逻辑（测试写在 `src/**` 的 `#[cfg(test)]` 里） | `source .env.test && cargo test --lib <filter>` |
   | Rust 集成 / CLI 行为（测试在 `tests/**`） | `source .env.test && cargo test --test <target> [<filter>]` |
   | Rust 二进制入口（测试挂在 bin target 上） | `source .env.test && cargo test --bin <name> [<filter>]` |
   | `web/` | `pnpm --dir web lint`、`pnpm --dir web test`（可加 `-- <file>` 收窄）、`pnpm --dir web build` |
   | `worker/`，**不触及** Cloudflare 运行时绑定（纯逻辑 / UI） | `pnpm --dir worker lint`、`pnpm --dir worker test`、`pnpm --dir worker build` |
   | `worker/`，**触及** D1 / R2 / Cloudflare 运行时绑定 | 上一行全部 + `pnpm --dir worker test:miniflare`（判据：改动涉及绑定、查询或运行时行为即必须加） |
   | `sql/migrations/**`（运行时迁移） | forward 幂等重放、`*_down.sql`（若有）回滚、旧二进制读新库的兼容判定，各自有指定用例 |
   | `sql/sqlite_*.sql`（bootstrap） | 新建库（bootstrap 路径）与既有库（迁移路径）最终 schema 一致的断言用例 |
   | `sql/publish/**`（Worker D1） | 迁移在 D1 上 forward 可应用 + `pnpm --dir worker test:miniflare` 覆盖受影响查询 |
   | `install.sh` / 打包脚本 | `bash -n install.sh` + 关键路径 dry-run 或安装冒烟证据 |
   | 仓库配置与 CI（`Cargo.toml` 非版本行、`rustfmt.toml`、`.github/workflows/**`） | 受影响 CI job 的本地等价命令，按下文「仓库配置与 CI 展开规则」现场提取；不可本地复现的部分归 D 组 |
   | 只改文档 / 索引（无代码、无配置） | 无表面 focused 门，只走 B 组的结构与链接门 |

   **Rust 行的修饰规则（不是第四行）:** feature gate 与 env/线程约束是上面三条 Rust 行的**修饰条件**，不构成独立表面：先按测试归属唯一选中 `--lib` / `--test` / `--bin` 中的一行，再把该 target 实际需要的 `--features`、env 变量与线程约束**并入同一条命令**，例如 `source .env.test && LIBRA_ENABLE_TEST_PROVIDER=1 cargo test --features test-provider --test code_ui_scenarios -- --test-threads=1`（`source .env.test &&` 前缀不可省略）。不得另跑一条不带 feature 的裸命令充当「集成行」，也不得留未替换占位符。

   **不得**为了凑一条 Rust 命令而制造与本卡无关的用例；也不得因为「C 组已有全量测试」就跳过 A 组某个表面的行。

   **仓库配置与 CI 展开规则:** 受影响 job 必须**从本卡实际改动的那个 workflow 文件现场提取**（不限于 `base.yml`；仓库当前还有 `release.yml`、`codeql.yml`、`live-compat.yml` 及 nightly workflows）。模板不保存 job→命令的映射快照（必然漂移）。两步：

   ① 列出 job，且必须限定在 `jobs:` 节点内（裸 `rg "^  [a-z...]:"` 会把 `on:` 下的 `push`/`schedule`、`permissions:` 下的 `contents`、`concurrency:` 下的 `group` 误报为 job）：

   ```bash
   awk '/^jobs:/{in_jobs=1; next} in_jobs && /^[^[:space:]]/{exit} in_jobs && /^  [A-Za-z0-9_-]+:/{print FNR ":" $0}' .github/workflows/<file>.yml
   ```

   ② 读取受影响 job 的**完整定义**——`permissions`、`strategy` / `matrix`（含 `fail-fast`）、job 与 step 级 `env`、`if`、`shell`、`run`、`uses`、`with`——再据此写判据。只看 `run:` 不够：`codeql.yml` 当前**没有任何 `run:` 步骤**，其执行步骤主要由 action 定义并同时受上述完整 job/step 字段约束；`release.yml` 也有大量语义性 `uses:` 步骤。分流：
   - **可本地复现**的步骤 → 抄成命令进 A 组，保留其 `env`、shell、`--features`、`--test-threads` 等全部约束。**依赖 GitHub checkout 的 Git 命令不得照抄**（本仓库无 `.git`，见 ER-07）：写出保持同一判据的 Libra-native 等价实现，例如 `web-check` 的 `web/out` 漂移检查用 `libra status --porcelain=v1 web/out`（输出非空即失败）替代 `git status --porcelain -- web/out`。
   - **不可本地复现**的 action-only 语义（CodeQL 扫描、artifact 上传、Homebrew tap 更新等）→ 不得凭空造命令，归入下文 **D 组远端后置门**。

   参考：`base.yml` 在 2026-07-28 的 job 集合为 `format` / `clippy` / `web-check` / `redundancy` / `test` / `network-remotes`（仅核对日快照，判定一律以现场读取的 workflow 文件为准）。

   **B 类型门**（按 `Task type` 取一行）：

   | Task type | 类型门 |
   |---|---|
   | `implementation` / `migration` / `removal` | 无额外类型门（由 A 组 + C 组构成完整验收；`family child` 自跑 A 组 + fmt/clippy，C 覆盖继承自家族唯一发布点） |
   | `docs` / `audit` / `handoff` | 结构与链接门：本卡产物文件存在且章节完整、内部链接与 `file:line` 锚点可解析、`libra status --short --branch` 无越界改动；涉及命令文档时另加 EN/zh 双份存在性检查。这三类**必须保持 no-code / no-config**：一旦发现需要改动代码或配置，不得「就地升级门」，必须先按 ER-03 把卡重分类为 `implementation` / `migration` / `removal`，同步 `Release boundary`、`Version increment`、`Release write set`，重跑粒度门后再执行 |
   | `spike` | 产物门：结论文档或 ADR 已落盘、go/no-go 已判定、承接卡已登记；**allowlist diff 门**——`libra status --short --branch` 的全部变更必须落在本卡 `Deliverables` 声明的产物内，且生产表面零改动（至少覆盖 `src/**`、`web/src/**`、`worker/src/**`、`sql/**`、`build.rs`、`install.sh`、`Cargo.toml`/`Cargo.lock`、CI 与仓库配置），用「Verification 判定口径」的退出码模板逐条守卫 |
   | `release` | 聚合守卫（本组引入的全部新守卫用例）+ release note / 兼容证据 |

   **C 发布收口门（由会推送的卡执行，顺序强制）:** ① 版本面五处 parity 预检（ER-08）→ ② 按 `Version increment` bump 五处 + 让工具链刷新 `Cargo.lock`（不手改）→ ③ 在**已 bump 的状态**上跑 `CLAUDE.md` 三门（fmt、clippy、`source .env.test && cargo test --all`）→ ④ `cargo build --release` → ⑤ 安装 → ⑥ `libra add <相关路径>` + `libra commit -s -m`（ER-07 的签名预检与提交后校验）→ ⑦ 推送并确认 branch ref：`libra push origin main` 成功且远端 ref 已更新 → ⑧ **条件步（仅当本计划要产出用户可获取的 release artifact；否则整步 `N/A`）**：创建并推送版本 tag `libra tag v<version>` + `libra push origin v<version>`，**并确认 tag 推送成功**（当前 `release.yml` 只由 `push` 的 `v*` tag 触发，**不**由 main branch push 触发——不推 tag 则 release 流水线根本不会运行）。

   三门必须覆盖 bump 后的最终状态——bump 与 `Cargo.lock` 刷新本身可能引入格式、lint 或编译回归，`cargo build --release` 不能替代 clippy 与全量测试。

   **C / D 边界（唯一口径）:** C 组**截止到已验证的 branch / tag 推送**；推送之后由远端流水线产生的一切证据——release artifact 上传与可获取性、Homebrew tap 更新、CodeQL 结果——全部归 **D 组**。任务卡必须为每个 D 组项登记：workflow 文件、job 名、**触发事件与 ref**（例如 `release.yml` = `push` `v*` tag）、以及判据。

   **D 远端后置门（post-push，仅在有不可本地复现的 CI 语义时适用）:** CodeQL 扫描、artifact 上传、Homebrew tap 更新这类步骤只能由 push / PR / tag 触发（例如 `codeql.yml` 由 push 与 PR 触发，`release.yml` 需要 tag），本地无法产出证据。因此：
   - D 组**不属于**本地验收，不阻塞 `locally-accepted`，也不改变 ER-05「先本地验收再 review」与 C 组「review 通过后才提交推送」的顺序。
   - 任务卡必须写明要看哪个 workflow 的哪个 job、**触发事件与 ref**（哪条命令触发的：`libra push origin main` 还是 `libra push origin v<version>`）、判据是什么，并在推送后取得结果。release artifact 的上传与可获取性、Homebrew tap 更新均属本组。
   - 只有当适用的 D 组门也全绿，该卡的 `Acceptance` 才能到 `complete`；此前停在新增的中间态 `remote-pending`。
   - D 组失败一律**前滚修复**（新提交 / 新版本），不得回退已推送提交或已发布 artifact；修复卡按 ER-10 的越界规则处理。

   「完成判据」的计划级门是最后一次总检查，不替代每张会推送的卡各自跑过的发布收口门。
5. **ER-05 Codex review 闭环:** 实现和本地验收完成后进行代码 review；review 问题修复后重跑相关验收，直到 review 明确给出 PASS。P0/P1 必须关闭，不得以「residual risk 已接受」替代 PASS（仅 P2 可由具名责任人书面接受）。
6. **ER-06 文档与兼容同步:** 涉及公开行为的任务必须同步用户文档 EN + zh、开发文档、兼容矩阵、错误码、help/examples、测试索引。
7. **ER-07 Libra-native 工作流与提交签名:** 本仓库使用 Libra 工作流：`libra status`、`libra add <相关路径>`、`libra commit -s -m "<scope>: <summary>"`、`libra push origin main`。不要把仓库当普通 Git 仓库处理。签名口径按 `AGENTS.md`「Commit & Pull Request Guidelines」执行 —— 提交需同时带 DCO 与 PGP 签名，并按仓库事实落地：`libra commit` **没有** `-S` 开关，`-s` 只添加 `Signed-off-by`；签名策略的优先级是 `--no-gpg-sign`（最高）> `commit.gpgSign`（Git 级联，`false` 直接关闭签名）> `vault.signing` 默认（为 `true` 且 vault unseal key 可用时签名），见 `src/command/commit.rs:215-224`、`src/command/history_config.rs:67-78`。因此：
   - 预检必须**先读 `commit.gpgSign`**（`libra config get commit.gpgSign`），未设置时再回退 `libra config get vault.signing`；`commit.gpgSign=false` 时不得当作「已启用签名」。
   - 每次提交后强制校验：`libra cat-file -p HEAD | rg -q '^gpgsig'`（注意 `libra log` 不支持 `--show-signature`，没有 `verify-commit` 子命令）。校验失败不得推送。
   - 只有在「字段全局默认与例外」的 waiver 表中存在 `豁免项 = ER-07 签名要求` 的 `EX-*`（含 Approver、Review round、证据、有效期）时，才允许以 sign-off-only 方式提交；「事实基线」最多链接该 `EX-*`，不构成独立授权路径。
   - **与 `AGENTS.md` 的已知漂移及优先级（必须按此执行）:** `AGENTS.md`「Commit & Pull Request Guidelines」示例写的是 `git commit -S -s`，「Workspace Notes」示例写的是 `libra commit -a -s`。两者在本仓库都不可照抄——本仓库没有 `.git`（必须用 `libra`），`libra commit` 没有 `-S`，而 `-a` 会把并发节点改动和带外删除一起提交（违反 GC-12 的精确暂存）。计划执行时**以 ER-07 + GC-12 为准**：`libra add <相关路径>` → `libra commit -s -m` →（按上两条做签名预检与提交后 `gpgsig` 校验）。该漂移应作为独立的文档修复项承接（更新 `AGENTS.md` 这两行），不得在计划执行中两套并行。
8. **ER-08 版本与发布:** 版本权威源是 `Cargo.toml` 的 `version`。发布前先做版本面 parity 预检：`Cargo.toml`、`web/package.json`、`worker/package.json`、`install.sh` 的 `DEFAULT_VERSION`、`install.ps1` 的 `$DefaultVersion`（后两者允许 `v` 前缀差异）必须一致；**不一致时先建立修复卡对齐，禁止直接 bump**。对齐后按任务卡的 `Version increment` 递增并同步全部五处，其余步骤与顺序按 ER-04 的「发布收口门」执行（bump 后必须重跑三门），并记录安装/发布证据。开工时须重新核对版本面文件数量是否仍为五处（2026-08-05 起为五处——`install.ps1` 由 `tests/compat/version_surface_sync.rs`（target `compat_version_surface_sync`）纳入强制相等；该守卫是版本面集合的权威）。
   - `Version increment` 取值：`patch`（默认）| `minor` | `major` | `N/A`。
   - 删除公开 surface、破坏兼容的 schema/协议变更必须用 `minor` 或 `major`，且递增级别由 ADR + 兼容窗口证据决定，不得用 patch 夹带；家族卡（G-08）的递增级别写在唯一发布点卡上，子卡为 `N/A`。
   - `docs` / `audit` / `spike` / `handoff` 卡为 `N/A`，但必须说明产物随哪次发布进入用户渠道。
9. **ER-09 push 失败策略:** 非 fast-forward 需要 pull/merge 后重新验收再推；认证、权限、网络或服务端失败不 blind retry，记录原因，待下一次修复/发布窗口处理。
10. **ER-10 内部服务错误（有界重试）:** AI provider、MCP、R2/D1、GitHub API、release/download 等错误不得直接把任务宣告完成。先分类：确定性错误（4xx 参数/权限、schema 不符、编译或配置缺陷）不重试，按范围决定归属——只有当修复落在**本卡行为轴内**，且先更新本卡 `Acceptance criteria` / `Verification` / `Implementation write set` / `Granularity` 后**重跑 ER-03 的全部 `G-*` 仍然通过**（含 G-10 写集不与在跑卡新冲突）时，才作为本卡修复项就地修；任一条不满足就新建修复卡 `FIX-*`、加一条 `FIX-* -> 当前卡` 的依赖边，并把当前卡置为 `blocked`，不得为了「顺手修完」突破粒度；暂时性错误（超时、5xx、限流、网络中断）按指数退避重试，并写明**最大尝试次数与总时间预算**（计划未另行规定时默认 ≤ 5 次、总计 ≤ 30 分钟）。超预算后把任务置为 `blocked` 并记录 sanitized 证据与升级对象，不得静默空转。发布类动作（push、release、安装）不自动重试，按 ER-09 处理。
11. **ER-11 证据卫生:** 验收证据不得保存 secret、API key、token、PII、未脱敏 transcript、绝对私有路径或原始 tool payload。需要留存时只写 sanitized summary。
12. **ER-12 并发边界与串行发布:** 并发只适用于**实现与 review 阶段**：只有 `Implementation write set` 不相交（G-10）的卡可以并发推进。**发布动作一律串行，且由单一发布者执行**：
    - 计划必须在「发布分组与并发窗口」声明发布者（哪个 Agent/人负责 C 组的 bump、构建、安装、提交、branch/tag 推送，以及推送后跟踪 D 组远端证据）。同一时刻只允许一个卡处于「已 bump 未完成推送」状态。
    - 进入发布前重新读取 `Cargo.toml` 权威版本（ER-08 的 parity 预检），按顺序做完整套发布动作后才轮到下一张卡。
    - **禁止多 Agent 并发发布。** 本仓库当前**没有**仓库级发布锁：`libra push origin main` 推送的是本地 `main` 的整个 ref tip，无法只发布一条协调记录，也无法在 push 之外提供 CAS 仲裁；靠纯文档约定实现的 lease 无法验证，属于未经实现验证的协议。若某计划确实需要并发发布，必须先用独立 ADR + 独立计划落地一个仓库级发布锁（含原子认领、fence 校验、超时回收、崩溃恢复与测试），并在本计划以 `DEFER-*` 登记；在该机制落地并通过验收之前，一律按本条串行执行。
    - 并发实现期间仍受 I–R 约束（G-10）：发布者持有发布窗口时，其它卡不得修改 `Release write set` 内的文件。

## 实施顺序

依赖边格式：`A -> B` 表示 A 必须先于 B。依赖图必须无环，且每条边都指向具体任务卡而不是整个 Phase（G-06）；任务卡拆分后本节必须同步更新。

- `<TASK-01> -> <TASK-02>`
- `<TASK-02> -> <TASK-03>`

### 依赖登记表

本计划外的一切依赖关系（其它日期计划、外部服务、人工审批、上游 release），以及本计划向外移交的范围，都必须在此登记后才能被任务卡引用（G-06）。计划内任务之间的依赖直接写任务 ID，不进本表。

`direction` 区分方向：`incoming` = 本计划等待外部产物；`outgoing` = 本计划把范围移交给别的计划（此时 Owner 是接收方，「超时与失败策略」写接收方未接手时的回落处理）。

| ID | direction | 类型 | 对象 | Owner | 产物与可用性判据 | 证据 | 超时与失败策略 |
|---|---|---|---|---|---|---|---|
| DEP-01 | `<incoming / outgoing>` | `<跨计划 / 外部服务 / 审批 / 上游 release>` | `<plan-YYYYMMDD#TASK-ID 或外部对象>` | `<负责人/系统/接收方计划>` | `<交付什么、如何判定可用>` | `<file:line / commit / URL + 核对日期>` | `<等待上限、超时后降级或回落路径>` |

### 发布分组与并发窗口

默认每张卡独立发布（G-07）。只有需要合并发布或需要显式并发/串行窗口时才登记本表；登记项必须在任务卡 `Release boundary` 中被引用。

| ID | 成员 | 唯一发布点 | 窗口规则 | 失败回滚顺序 | 理由 |
|---|---|---|---|---|---|
| REL-01 | `<TASK-ID 列表>` | `<TASK-ID>` | `<例如：不推送窗口——子卡只本地提交，不 bump、不构建、不推送；窗口期禁止插入其它切片>` | `<按依赖逆序 revert 本地提交并重跑 ER-04>` | `<为何无法拆成独立发布切片>` |

**并发声明:** `<实现阶段可并发的卡组（实现写集互不相交）/ 全串行>`（G-10）

**发布者:** `<负责 C 组 bump/构建/安装/提交/branch+tag 推送，并跟踪 D 组远端证据的唯一 Agent/人>`（ER-12：发布一律串行，禁止多 Agent 并发发布）

**发布窗口顺序:** `<按依赖与 REL-* 分组列出发布顺序；同一时刻只允许一张卡处于「已 bump 未完成推送」状态>`

并发执行时另需满足：实现写集不相交（G-10）。

### Phase 0: <基线冻结和消歧>

**目标:** `<本阶段目标>`

**进入条件:**

- `<前置条件>`

**退出条件:**

- `<阶段完成判据>`

### Phase 1: <实现第一个可发布切片>

**目标:** `<本阶段目标>`

**进入条件:**

- `<前置条件>`

**退出条件:**

- `<阶段完成判据>`

## 任务卡

任务 ID 使用稳定前缀，例如 `A0-01`、`DR-01`、`W1-02`、`P0-03`。编号被引用后不重排；拆分出的新卡在所属 Phase 末尾追加新编号，因此**编号顺序 ≠ 执行顺序**，执行以「实施顺序」的依赖边和各卡 `Dependencies` 为准。废弃的编号保留并标记替代关系。

### 任务卡粒度规则（强制）

粒度是任务卡质量的第一判据：卡过大则无法 review、无法回滚、无法交给单个 Agent 完成；卡过碎则实现、测试与文档脱节，且每片都要付一次发布成本。新增或修改任务卡时必须逐条满足下列 `G-*` 规则。任一条不满足即为「粒度不合格」，必须在开工前拆分或合并，并同步实施顺序、依赖登记表、发布分组、追溯表、测试矩阵、里程碑、风险表的任务归属和修订历史。

- **G-01 单一行为轴与恢复模式（上限）:** 一张卡只承担一个**可独立恢复**的行为轴——该卡失败或需要撤回时，存在**单一已声明的恢复动作**，执行后系统停在一个自洽状态，不留半吊子中间态。这里的「恢复」不等于「一次 `libra revert`」：不可逆变更同样合格，只要恢复路径是单一且已写明的。禁止把「协议变更 + 容量门 + 前端接入」「删 A + 删 B + 删 C」「新增能力 + 顺带重构既有实现」压进同一张卡。快速判据：`Description` 中出现两个以上并列的「并且 / 同时 / 顺带 / 以及」，先按「推荐拆分维度」拆。恢复形式必须在 `Rollback mode` 字段声明为四种模式之一：
  - `revert`：纯本地代码/文档变更，一次 revert 即完整撤销（默认）。
  - `forward-only`：已产生不可逆数据或迁移（SQLite/D1/R2/对象库），只能前滚修复；必须写出数据不变量、恢复验证命令和用户影响，**不得**为了凑「可 revert」而设计不安全的 down migration。
  - `compensating`：对外部服务已有副作用（发布、删除、远端写入），撤销靠补偿动作；必须写出补偿命令与幂等键。
  - `immutable-release`：已发布 artifact 不可撤回，只能发新版本；必须写出降级指引与兼容窗口。
- **G-02 完整可交付（下限）:** 一张卡必须是一个自洽的可验收增量。同一行为轴的实现、测试、文档、兼容矩阵与索引同步是**同一张卡**的验收内容，禁止拆成「实现卡 / 补测试卡 / 补文档卡」。只有当被拆出的部分本身就是独立可恢复的行为轴（G-01 意义上：有单一已声明的恢复动作——独立迁移、独立 deprecation 收口、独立性能门、独立 UI 切面、跨计划移交）时才允许单独成卡。
- **G-03 条目上限与计数口径:** 上限按 `Task type` 取值，计数按**独立判据**而非行数。ER-04 的强制门（fmt / clippy / 全量测试 / 表面门）**不计入**本条计数，`Verification` 只登记本卡特有的判据：

  | Task type | AC 上限 | Verification 上限 |
  |---|---|---|
  | `implementation` / `migration` / `removal` | 8 | 8 |
  | `spike` | 8 | 8 |
  | `docs` / `audit` / `handoff` | 20 | 20 |
  | `release` | 12 | 12（只计聚合守卫、release note、兼容证据等本卡特有项） |

  计数细则：
  - AC 按「独立 pass/fail 谓词」计。一条 checklist 内用「且 / 并且 / 以及 / 同时」连接的多个可分别失败的断言按多条计；嵌套子列表逐项计；表格行逐行计。
  - Verification 按「独立验证门」计。判据是「是否构成一次独立的通过/失败判定」：环境准备前缀（`source .env.test`、`LIBRA_*=1` 赋值、`cd`、`export`）与其后的命令合计为**一门**；一条命令中的多个 `--test` target 分别计；`&&` 串联两个都会独立判定的验收命令按两门计；手工证据按项计。
  - 超限视为多轴信号，必须拆卡，**不得**通过合并长句、塞进表格或改写成「等等」来规避。
  - 文档 / 审计 / 索引-only 卡的条目是清单项、不构成独立行为轴，故适用 20 条上限；这类卡仍受 G-01 约束，且必须在任务卡 `Deliverables` 字段登记产物范围（具体文件清单）——这是常规登记，不是例外，无需进 waiver 表。需要突破本表上限时，只能在 waiver 白名单登记 `EX-*`（具名审批），不得私自改写分母。
- **G-04 规模上限（可计数）:** `Estimated scope` 的开工态只允许 `S` 或 `M`。`L`/`XL` 只能作为「必须再拆」的中间标注，计划成稿后不得存在 L/XL 卡。计数**只统计行为实现落点与生产文件**，不统计「随附同步集」：
  - **计入**：承载本卡行为变更的生产代码落点与文件（`src/**`、`sql/migrations/**`、`web/src/**`、`worker/src/**` 等）。
  - **不计入（随附同步集）**：本卡自己的测试文件、按 GC-05/ER-06 强制同步的文档集（`docs/commands/<cmd>.md` EN+zh、`docs/development/**`、`COMPATIBILITY.md`、`docs/error-codes.md`）、测试索引（`tests/INDEX.md`、`tests/compat/README.md`、`Cargo.toml` 的 `[[test]]`）、以及 ER-08 的版本面五处。这些是每张卡的固定成本，不构成粒度信号；但仍要在写集字段中如实列出（文档/测试进 `Implementation write set`，版本面五处进 `Release write set`）。
  - **仓库根文件**（`Cargo.toml`、`install.sh`、`COMPATIBILITY.md` 等）按「单个文件」计，不各占一个落点；若某张卡的行为变更**就发生在**根文件本身（例如改 `build.rs` 逻辑），则该文件计为一个落点。
  - `S`：≤ 2 个行为落点、≤ 3 个生产文件，无 schema / 协议 / 公开接口变更。
  - `M`：≤ 4 个行为落点、≤ 12 个生产文件，最多一处公开行为或接口变化，仍是单一行为轴。
  - 超出 `M` 的计数即为 L：默认必须拆分。确实不可拆的机械变更（全仓重命名、批量删除、格式化）可在「字段全局默认与例外」的 waiver 白名单中登记 `EX-*`（需具名审批人与 review 轮次），写明为何不可拆、如何 review、如何恢复；此时该卡 `Estimated scope` 写 `L-exception:EX-<n>`，这是全文唯一允许出现 `L` 字样的形式，`XL` 永不允许。
  - 把 `src/`、`tests/`、`docs/` 或仓库根算作「一个落点」是规避行为，按「粒度反模式速查」的「落点注水」处理。
- **G-05 Agent 可独立执行:** 一张卡必须能在不阅读其它卡正文的前提下被执行：`Current evidence` 给出可核对的 `file:line` 锚点，`Acceptance criteria` 自洽可判定，`Verification` 是可直接复制执行的确切命令，`Dependencies` 只引用「依赖登记表」中的 `DEP-*` / 任务 ID。禁止「见上文」「同上一卡」式跨卡隐式约定；确属跨卡共享的约定要提升为全局工程约束或 ADR。
- **G-06 依赖闭合且无环:** 依赖必须有向无环。本计划内依赖直接引用任务 ID；跨计划与外部前置必须先在「依赖登记表」登记为 `DEP-*` 再引用，不得在卡内自由描述。互相等待、循环依赖、以及「等某个 Phase 整体完成」都是拆分错误——把依赖收敛到具体前置卡。「实施顺序」的依赖边与各卡 `Dependencies` 必须一致；不一致时以「实施顺序」为准并当场修正卡片。
- **G-07 发布切片对齐（按任务类型）:** 默认「一张卡 = 一个发布切片」（独立 review + ER-04 门 + 版本 + 提交 + 推送）。适用范围按 `Task type`（G-11）区分：`implementation` / `migration` / `removal` 必须走完整发布切片；`docs` / `audit` / `spike` / `handoff` 卡不 bump 版本、不产出 artifact，`Release boundary` 写 `no-release` 并说明其产物随哪次发布进入用户可见渠道；`release` 卡本身就是发布点（家族卡的唯一发布点必须是 `release` 卡，见 G-08）。任何「多卡合并发布」都是例外，必须在「发布分组与并发窗口」登记 `REL-*`：成员、唯一发布点、窗口期禁止插入的内容、失败时的逆序回滚顺序。例外必须先修订计划并通过 Codex review 才可开工，**不得**在开工时凭笔记临时合并。
- **G-07a 批量发布组（`batch-release child`）:** 当一组卡各自都是完整、可独立回滚的变更，但**逐卡发版对用户没有独立价值**（例如同一 wave 内的文档/账本/测试产物，或版本号推进会与并发计划频繁抢号）时，可登记为「批量发布组」：成员卡逐张 review、逐张通过全部适用的 A/B 门与三门、逐张提交并**推送**，但不 bump、不构建、不 tag、不产 artifact；全组共用一个唯一发布点卡（`Task type` 必须是 `release`），由它一次性完成版本面与 tag/artifact。与 G-08 家族卡的区别是**成员会推送**且各自可独立回滚（前滚补偿），因此不要求「变更不可分割」；与 G-07 默认的区别是把「发布」从每卡收敛到一次。必须在「发布分组与并发窗口」登记 `REL-*`（成员、唯一发布点、窗口规则、失败回滚顺序、理由），并在每张成员卡的 `Release boundary` 写 `batch-release child of REL-<n>`，**唯一发布点卡的 `Release boundary` 写 `batch-release point of REL-<n>`**（2026-08-06 补充：该取值属 G-07a 自身，不要写成 G-08 的 `family release point`——后者的前提是「成员不推送、变更不可分割」，与本形态互斥）。**成员卡的 `Acceptance` 在发布点完成前不得标为 `complete`，也不得对外报告完成。**
- **G-08 家族卡（不可分割变更的唯一出路）:** 当一次公开 surface 删除、或 schema 与 reader 必须同时上线这类变更确实无法切成可独立发布的切片时，用「家族卡」表达：拆成多张各自 review、各自通过全部适用 ER-04 门、各自本地提交的子卡，共用一个唯一发布点卡；**该发布点卡的 `Task type` 必须是 `release`**（不引入新行为，只做版本、构建、安装、聚合守卫与发布证据），以保证它在 ER-04 的 B 组中唯一命中 `release` 行。家族内子卡仍受除 G-07 外的全部 `G-*` 约束；子卡 `Release boundary` 写 `family child`，发布点卡写 `family release point`，家族边界与「不推送窗口」写进 `REL-*` 登记。
- **G-09 拆分协议:** 拆分已被引用的卡时，原编号保留给主轴，新子卡在所属 Phase 末尾追加新编号，不重排既有编号。原卡必须写明「拆出 `<ID>`、`<ID>`」，新卡写明「自 `<ID>` 拆出」，并同步实施顺序、依赖登记表、「发布分组与并发窗口」、追溯表、测试矩阵、里程碑、风险表，以及「修订历史」中的一行（日期、原因、原卡、新卡、受影响引用）。
- **G-10 写集与并发:** 写集分三类，每张卡必须声明前两类（第三类由 ER-12 统一定义，卡内不重复）：
  - **`Implementation write set`（I）**：承载本卡行为的代码、测试、文档文件。
  - **`Release write set`（R）**：ER-08 的版本面五处、`Cargo.lock`、release artifact。对所有发布卡相同；`family child` 与 `no-release` 卡写 `N/A`（它们不 bump、不构建、不推送）；`batch-release child` 同样写 `N/A`（它推送，但不 bump、不构建、不产 artifact——版本面归其批量发布组的唯一发布点）。
  - **协调写集（C）**：计划级的发布顺序与窗口记录（「发布分组与并发窗口」的发布者、发布顺序、`REL-*` 登记）。由 ER-12 的单一发布者串行维护，**不计入**任何卡的 I 或 R，也不参与并发判定。

  冲突规则：
  - **I–I 相交** → **禁止并发，无豁免通道**（G-10 不在 waiver 白名单内）：只有两个合法出路——补一条顺序依赖边，或把相交部分合并到唯一集成卡。
  - **I–R 相交**（某卡把 `Cargo.toml`、`install.sh` 等当行为落点，而它同时属于别的卡的 R）→ 在**已声明的串行发布窗口**内（ER-12），该窗口对 R 内文件是写锁：其它卡不得在此期间修改这些文件，必须等窗口结束或补顺序边。
  - **R–R 相交** → 由 ER-12 的串行发布窗口顺序化，不构成并发禁止条件。
  - `Files likely touched` 是估计值，并发判定以 `Implementation write set` 为准。
- **G-11 任务类型:** 每张卡必须声明 `Task type`，不同类型适用不同粒度口径：
  - `implementation`：默认类型，全部 `G-*` 条款全量适用。
  - `migration`：数据/schema 迁移，`Rollback mode` 通常为 `forward-only`，必须有 up/down 或前滚验证与故障注入用例。
  - `removal`：公开 surface 删除，通常进入家族卡（G-08），必须先有 deprecation 窗口证据。
  - `spike`：探索/验证，**不得**改动生产代码。必须写出待回答的问题、时间箱、产物（结论 + ADR 或缺口登记）、go/no-go 退出标准与后续承接卡；不适用 G-04 的文件计数，`Estimated scope` 按时间箱判定：`S` ≤ 0.5 人日、`M` ≤ 2 人日，超出即拆成多个问题或直接转 ADR / `implementation` 卡。
  - `audit` / `docs`：只读核对或文档收敛，按 G-03 的文档-only 口径执行。规模上限按**产物文件数或人日**判定（不适用 G-04 的生产文件计数）：`S` ≤ 5 个产物文件或 ≤ 0.5 人日；`M` ≤ 15 个产物文件或 ≤ 2 人日；超出即拆卡。随代码卡强制同步的文档仍按 G-04 的随附同步集处理，不计入这里。
  - `release`：发布点卡，不引入新行为，只做版本、构建、安装、聚合守卫与发布证据。
  - `handoff`：跨计划移交，默认 `no-release`。**移入**（本计划承接他人）在「依赖登记表」登记 `direction: incoming`；**移出**（本计划把范围交给别的计划）登记 `direction: outgoing`，并写明接收方计划、移交日期、本计划不再重做的部分，以及接收方未接手时的回落处理。

#### 推荐拆分维度

超限卡按下列维度之一切开；切完每片仍须独立满足 G-01（单一行为轴 + 已声明的恢复模式）。

| 维度 | 切法 | 典型结果 |
|---|---|---|
| 数据 / 状态轴 | schema 扩展 → 写入幂等与持久化 → 读取投影与恢复 | 3 张卡 |
| 协议轴 | 协议版本与协商 → 容量 / 背压与性能门 → 消费端接入 | 3 张卡 |
| 表面轴 | 后端 runtime → 机器接口（JSON / 事件 / 错误码） → 前端或 UI 组件 | 2–3 张卡 |
| 生命周期轴 | 新实现上线 → 默认切换 → deprecation shim → 物理删除 | 按发布窗口分卡 |
| 安全轴 | 身份与请求边界 → 文件 / 进程 posture → 敏感信息 redaction | 每轴一卡 |
| 清理轴 | 公开 surface 删除（家族卡） → 内部模块退场 → 依赖摘除 | 家族卡 + 普通卡 |

#### 粒度反模式速查

| 反模式 | 症状 | 处理 |
|---|---|---|
| 巨型卡 | `Estimated scope` = L；AC > 8；Description 含多个并列目标 | 按「推荐拆分维度」拆分（G-01/G-03/G-04） |
| 碎片卡 | 「补测试」「补文档」「改个字段名」单独成卡 | 合并回所属行为轴（G-02） |
| 多轴伪装 | 把多条 AC 合成一条长句、塞进表格或写「等等」以压到 8 条以内 | 按独立谓词还原计数后重新判定（G-03） |
| 落点注水 | 把 `src/` 或仓库根算作「一个落点」以保住 S/M | 按目录级落点重新计数（G-04） |
| 隐式依赖 | Description 写「按 X 卡的约定」而 X 卡未交付该约定 | 写进本卡，或提升为全局约束 / ADR（G-05） |
| 幽灵验收 | `Verification` 只写 `cargo test --all`，或零命中守卫不区分 `rg` 退出码 `1` 与 `>1` | 指定 target 与 test fn；按「Verification 判定口径」的退出码模板重写（G-05） |
| 悬空依赖 | `Dependencies` 写「Phase N 完成」或自由描述外部前置 | 收敛到具体前置卡 ID / `DEP-*`（G-06） |
| 假回滚 | 已发布或已迁移数据的卡仍写「一次 revert 撤销」 | 按实际选 `forward-only` / `compensating` / `immutable-release`（G-01） |
| 并发冲撞 | 两张无依赖的卡实现写集相交 | 只有两条出路：补顺序边，或合并到唯一集成卡（G-10 不可豁免）。版本面争用不算并发冲突，由 ER-12 的串行发布窗口处理 |
| 顺手合并 | 多张卡凭开工笔记合成一次发布 | 登记为 `REL-*` 家族卡，或拆回独立发布切片（G-07/G-08） |

#### 字段全局默认与例外

计划在本节声明字段的全局默认值后，任务卡中**取默认值的字段可以整行省略**，或写 `Inherited`；只有偏离默认的字段才在卡内展开并在下表登记。`Task type`、`Lifecycle / Acceptance`、`Rollback mode`、`Implementation write set`、`Version increment`、`C/D coverage from`、`Granularity` 摘要行是每卡必填，不可省略；`Release write set` 可写 `Inherited` 或 `N/A`；`Deliverables` 对 `docs` / `audit` / `spike` / `handoff` 卡必填。

- **Release boundary 默认:** `<每张卡独立发布切片 / 其它>`
- **Task type 默认:** `<implementation / 其它>`
- **Rollback mode 默认:** `<revert / 其它>`
- **Migration and rollback 默认:** `<N/A：无 schema 迁移 / 其它>`
- **Security and privacy 默认:** `<继承 GC-07、GC-11 / 其它>`
- **Performance budget 默认:** `<继承 GC-10 / 其它>`
- **Docs and compatibility impact 默认:** `<按 GC-05 同步 EN + zh / 其它>`

**默认覆盖**（不是例外，只是取了非默认值，无需审批）：

| 任务 | 偏离的字段 | 取值与理由 |
|---|---|---|
| `<ID>` | `<Rollback mode>` | `<forward-only：SQLite 迁移不可逆，前滚修复 + doctor 校验>` |
| `<ID>` | `<Docs and compatibility impact>` | `<仅内部文档，无用户命令文档变化>` |

**规则 waiver（`EX-*`，需具名审批）**：可豁免的规则是**白名单**，只有下表三项；`G-01`、`G-02`、`G-05`、`G-06`、`G-07`、`G-08`、`G-09`、`G-10`、`G-11` **永不可豁免**（它们是可 review、可恢复、可并发的前提）。

| 可豁免项 | 允许的理由范围 |
|---|---|
| G-03 条目上限 | 清单型产物（文档 / 审计 / 索引）确实需要超过本类上限，且已写明产物文件清单 |
| G-03 条目上限（**门族型验收**，2026-08-06 扩充） | `implementation` 卡的验收本质是**同一恢复轴上的机械门族/fixture 清单**（净室、schema、绑定、投影、锁等），逐门计数必然超过本类上限，而按门拆卡会违反 G-01/G-02（同一行为轴的实现/测试/文档不得拆散）与「碎片卡」反模式。准入条件（缺一不可）：① 每个门都是**可复制执行的具名命令**，失败即整卡不达标；② 门族清单在卡内「判据规范（非计数正文）」块逐条枚举；③ 分子如实写作 `n/上限@EX-ID`，不得以合并长句掩盖；④ 门族增减时同批更新 waiver 行与粒度审计表 |
| G-04 规模上限（`L-exception`） | 不可拆的机械变更：全仓重命名、批量删除、格式化 |
| ER-07 签名要求 | 仓库策略层面的具名豁免（sign-off-only） |

| 例外 ID | 任务（或 `ALL/<作用域>`） | 豁免项 | 理由与补偿措施 | Approver | Review round | 证据 | 有效期 |
|---|---|---|---|---|---|---|---|
| EX-01 | `<ID>` | `<G-03 条目上限>` | `<文档-only 卡，产物范围 = docs/commands/<cmd>.md EN+zh；补偿 = 逐文件 checklist>` | `<具名审批人>` | `<R-n>` | `<file:line / review 结论>` | `<本计划内 / 至 YYYY-MM-DD>` |
| EX-02 | `<ID>` | `<G-04 规模上限>` | `<全仓重命名不可拆；review = 逐目录 diff 抽检 + 守卫用例；恢复 = revert 单提交>` | `<具名审批人>` | `<R-n>` | `<命令与守卫用例>` | `<本计划内>` |

#### 任务卡粒度审计表

计划成稿与每次规范性修订后填一次，逐卡汇总各卡 `Granularity` 行，便于机械核对与脚本校验。判定规则：任一列不达标即不得开工；`AC` / `VER` / `scope` 列超限时必须带 `@EX-ID` 或 `L-exception:EX-n`，且该 `EX-*` 必须同时满足：存在于 waiver 表、`任务` 列等于引用它的卡（或显式写 `ALL/<作用域>` 的计划级豁免）、`豁免项` 等于被超限的那条规则、理由落在白名单、且仍在有效期内。任一条不满足仍判为不达标。

| 任务 | type | axis | recovery | complete | self-contained | AC | VER | landing / prod-files | scope | deps | writeset | release | split-from | exception |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `<ID>` | `<Task type>` | `<行为轴>` | `<恢复动作>` | `<yes>` | `<yes>` | `<n/上限[@EX-ID]>` | `<n/上限[@EX-ID]>` | `<n>/<n>` | `<S/M/L-exception:EX-n>` | `<TASK-ID/DEP-ID/none>` | `<no-overlap/序列化于 ID>` | `<independent/batch-release child/batch-release point/family child/family release point/no-release>` | `<ID/N/A>` | `<EX-ID/N/A>` |

#### Verification 判定口径

- **零命中守卫必须区分「无命中」与「命令失败」。** `rg` 的退出码是 `0` = 有命中、`1` = 零命中、`>1` = 执行失败（非法正则、路径不存在、I/O 错误）。`! rg …` 和 `if rg …; then exit 1; fi` 都会把 `>1` 误判为通过，**不得**单独作为证据。裸 `rg …; rc=$?` 也不行——在 `set -e` 下零命中会让脚本在取 `$?` 前就退出。使用下列模板（已在 `bash -e` 与 `zsh -e` 下验证三条分支）：

  ```bash
  if rg -n "<pattern>" <paths>; then
    echo "FAIL: forbidden pattern found"; exit 1
  else
    rc=$?
    if [ "$rc" -ne 1 ]; then echo "ERROR: rg failed with exit $rc"; exit "$rc"; fi
    echo "OK: zero hits"
  fi
  ```

- 「只允许 allowlist 命中」类守卫必须逐条比对固定 allowlist，并在任务记录中附命中 diff。
- 仅用于定位符号的 `rg` 必须注明「锚点定位用，非判据」。
- 本任务新增的 test fn / 场景过滤必须标 `(new)`，并确保其所属 `--test` target 已存在，或在同卡内注册（`tests/compat/*` 还需 `Cargo.toml` `[[test]]`）并同步 `tests/INDEX.md`。
- `cargo test --all` 不能替代任务指定用例（ER-04）；反过来，指定用例也不能替代计划完成前的全量 L1 门（见「完成判据」）。

### Task <ID>: <任务标题>

**Task type:** `<implementation | migration | removal | spike | audit | docs | release | handoff>`（G-11）

**Lifecycle / Acceptance:** `<pending | in-progress | blocked | done>` / `<空 | locally-accepted | remote-pending | complete>`（ER-04；两者正交，`done` 以 `complete` 为前提）

**Description:** `<要做什么、为什么、现实影响。一句话点明本卡唯一的行为轴。>`

**Out of scope:** `<逐项列出本卡明确不做的内容，每项标注状态：「由 <ID> 承接」/「尚未排期，重启条件 …」/「永久非目标，理由 …」。不得为了填表制造虚假承接关系（ER-03）。>`

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `<当前实现或缺口>` | `<file:line / test / external repo@sha>` |

**Acceptance criteria:**

- [ ] `<用户可见或系统行为判据>`
- [ ] `<机器输出/错误码/schema 判据>`
- [ ] `<失败路径/边界条件判据>`
- [ ] `<文档/兼容/索引同步判据>`

**Verification:**

- [ ] `<exact command>`
- [ ] `<exact command>`
- [ ] `<manual/sanitized evidence, if required>`

**Dependencies:** `<无 / 本计划 Task ID + 本卡消费的具体产物（接口、文件、测试） / 「依赖登记表」中的 DEP-ID>`（G-06）

**Deliverables:** `<docs / audit / spike / handoff 卡必填：产物文件清单（G-03 的产物范围登记位置）。代码卡写 N/A 或 Inherited。>`

**Implementation write set:** `<承载本卡行为的代码/测试/文档文件或目录。并发判定只看这一项：与并发在跑的卡不得相交，相交时只能补顺序边或合并到唯一集成卡>`（G-10）

**Release write set:** `<Inherited（= ER-08 版本面五处 + `Cargo.lock` + release artifact）/ N/A（family child / no-release 卡）>`（不用于实现阶段并发分组；进入发布窗口后按 G-10 的 I–R / R–R 规则串行化）

**Files likely touched:** `<src/...>, <tests/...>, <docs/...>`（估计值；并发判定以 `Implementation write set` 为准）

**Docs and compatibility impact:** `<Inherited / 具体文件（EN + zh）>`

**Rollback mode:** `<revert | forward-only | compensating | immutable-release>`（G-01）

**Migration and rollback:** `<N/A 或 up/down、前滚步骤、数据不变量、恢复验证命令、用户影响>`

**Security and privacy:** `<N/A 或权限、secret、path、redaction、sandbox 约束>`

**Performance budget:** `<N/A 或数据规模、复杂度、wall-clock/benchmark 断言>`

**Estimated scope:** `<S / M / L-exception:EX-<n>（仅限已登记的不可拆机械变更）>`（G-04；`XL` 永不允许作为开工态）

**Version increment:** `<patch（默认）| minor | major | N/A>`（ER-08）

**Release boundary:** `<independent（默认）| batch-release child of REL-<n>（G-07a：推送但不发版）| batch-release point of REL-<n>（G-07a 的唯一发布点，`Task type` 必须为 `release`）| family child of REL-<n>（k/n）| family release point of REL-<n>（G-08 家族卡专用）| no-release（docs/audit/spike/handoff）>`（合并发布须先按 G-07/G-07a 登记 `REL-*`）

**C/D coverage from:** `<self（自行执行 C 组）| <TASK-ID>（继承该发布点/收口点的 C 覆盖与其 D 组）>`（ER-04；`family child` 与 `no-release` 卡必填具体 ID，不得留空）

**Granularity:** `type=<Task type>; axis=<本卡唯一的行为轴>; recovery=<失败/撤回时的单一恢复动作与恢复后的自洽状态>; complete=<yes：实现+测试+文档+索引同步都在本卡内>; self-contained=<yes：不读其它卡正文即可执行>; AC=<n>/<上限>[@EX-ID]; VER=<n>/<上限>[@EX-ID]; landing=<n>; prod-files=<n>; scope=<S|M|L-exception:EX-n>; deps=<none|TASK-ID,…|DEP-ID,…>; writeset=<no-overlap|序列化于 TASK-ID>; release=<independent|batch-release child|batch-release point|family child|family release point|no-release>; split-from=<TASK-ID|N/A>; exception=<EX-ID[,EX-ID…]|N/A>`

字段与规则的对应：`type`→G-11，`axis`/`recovery`→G-01，`complete`→G-02，`AC`/`VER`→G-03（分母按 G-03 的 Task type 上限表取值：代码卡与 spike 为 8，`release` 为 12，docs/audit/handoff 为 20；ER-04 的强制门不计入）。**超限只有一种合规写法**：`AC=21/20@EX-01` —— 分子超过分母时必须紧跟豁免该列的 `EX-ID`，否则审计判为不达标；一张卡可同时需要多个豁免，`exception` 用逗号分隔并逐个说明所豁免的列，`landing`/`prod-files`/`scope`→G-04，`self-contained`→G-05，`deps`→G-06，`release`→G-07/G-08，`split-from`→G-09，`writeset`→G-10，`exception`→已登记的 `EX-*`。这一行是 `G-*` 的机器可核对摘要，ER-03 开工前逐字段核对；写不出来就说明卡还没拆干净。计划级汇总见「任务卡粒度审计表」。

## 测试矩阵

| 类别 | 必须覆盖 | Target / command |
|---|---|---|
| 单元 | `<纯逻辑、parser、state machine>` | `<cargo test ...>` |
| 集成 | `<真实 CLI 工作流>` | `<cargo test --test ...>` |
| 兼容 | `<Git/CLI/schema/文档矩阵>` | `<cargo test --test compat_...>` |
| 迁移 | `<up/down、old/new binary、故障注入>` | `<cargo test ...>` |
| 安全 | `<path traversal、secret、authz、sandbox>` | `<cargo test ...>` |
| 性能 | `<规模与预算>` | `<criterion / wall-clock>` |
| live/gated | `<真实外部服务或 provider>` | `<feature/env gated command>` |

## 追溯表

| 任务 | 来源/证据 | Libra 落点 | 文档/兼容动作 | 指定测试 |
|---|---|---|---|---|
| `<ID>` | `<file:line / issue / repo@sha>` | `<src/tests/sql/docs>` | `<docs/commands, COMPATIBILITY, error-codes>` | `<target::test_fn>` |

## 里程碑验收与回滚

| 里程碑 | 完成条件 | 发布/证据 | 回滚或前滚 |
|---|---|---|---|
| M0 | `<基线冻结>` | `<commit/test/doc>` | `<N/A>` |
| M1 | `<首个可发布切片>` | `<version/test/review>` | `<rollback/forward fix>` |

### 故障恢复矩阵

| 故障点 | 可接受残留 | 恢复动作 | 禁止结果 |
|---|---|---|---|
| `<reservation 后、提交前>` | `<lease/临时对象>` | `<retry/abandon/doctor>` | `<双写/数据丢失/静默成功>` |

## 风险登记

| 风险 | 影响 | 缓解 | 任务 |
|---|---|---|---|
| `<风险>` | `<高/中/低 + 影响>` | `<测试/设计/门禁>` | `<ID>` |

## 性能与容量摘要

| 操作 | 单次成本 | 累积成本 | 预算/上限 | 验证 |
|---|---|---|---|---|
| `<操作>` | `<O(...)>` | `<O(...)>` | `<阈值>` | `<测试/benchmark>` |

## 兼容与文档收口

- [ ] `COMPATIBILITY.md` 已同步，或说明 `N/A`。
- [ ] `docs/commands/<cmd>.md` 已同步，或说明 `N/A`。
- [ ] `docs/commands/zh-CN/<cmd>.md` 已同步，或说明 `N/A`。
- [ ] `docs/development/commands/<cmd>.md` 已同步，或说明 `N/A`。
- [ ] `docs/error-codes.md` 已同步，或说明 `N/A`。
- [ ] `tests/INDEX.md` 已同步，或说明 `N/A`。
- [ ] `Cargo.toml` `[[test]]` 已同步，或说明 `N/A`。
- [ ] `plan-long.md` 日期计划索引或 LR 状态已同步，或说明 `N/A`。

## Codex review log

Result 只允许 `PASS` 或 `FAIL`。`FAIL` 必须列出 P0/P1 条目并在下一轮复审关闭；P2 可由具名责任人书面接受为 residual risk，但不改变本轮 `FAIL` 记录（ER-05）。

| Round | Scope | Result | P0/P1 | P2 处置 | Evidence |
|---|---|---|---|---|---|
| R1 | `<files/tasks>` | `<PASS / FAIL>` | `<条目与关闭状态>` | `<修复 / 具名接受人>` | `<test commands / 复审轮次>` |

## 非目标与延后项

| ID | 延后内容 | 原因 | 重启条件 | 承接位置 |
|---|---|---|---|---|
| DEFER-01 | `<内容>` | `<原因>` | `<何时重启>` | `<plan/RFC/ADR>` |

## 完成判据

计划只有在以下条件全部满足后才能标记完成：

- [ ] 所有任务卡满足粒度规则 `G-*`：无未登记的 L 例外、无 XL 卡、无碎片卡、无未登记的合并发布例外、实现写集冲突均已消解；「任务卡粒度审计表」已填齐。
- [ ] 所有非延后任务的 acceptance criteria 已满足，且 `Lifecycle=done` **且** `Acceptance=complete`（ER-04）。任何停在 `remote-pending` 的卡都必须先取得其 D 组远端后置门的绿色证据。任何仍为 `blocked` 的任务都必须先解除阻塞（`blocked` → `in-progress` → 完成剩余动作 → `done`）或按 `DEFER-*` 正式延后，不得带着 `blocked` 通过完成门。
- [ ] 所有任务的 Verification 命令已运行并记录结果。
- [ ] **计划完成门（区别于每卡 focused gate）**：`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && cargo test --all` 全绿（L1 全量；L2/L3 未设置 env 时打印 skipped 可接受，失败不可接受）；若本计划改动 `web/`，`pnpm --dir web lint`、`pnpm --dir web test`、`pnpm --dir web build` 已通过；改动 `worker/` 时 `pnpm --dir worker lint`、`pnpm --dir worker test`、`pnpm --dir worker test:miniflare`、`pnpm --dir worker build` 已通过；改动 feature-gated 代码时，写明实际 feature/target/env 的命令已通过（不得留未替换占位符）。
- [ ] 必要的 docs/compat/error-code/test-index 更新已完成。
- [ ] 必要的 migration、rollback、failure-recovery 验证已完成；每张卡的 `Rollback mode` 都已被实际验证或记录为不可验证的原因。
- [ ] Codex review 最终结论为 `PASS`，P0/P1 全部关闭；仅 P2 residual risk 允许保留，且有具名接受人。
- [ ] 如有发布要求，版本面五处一致、构建、安装、提交、推送和发布证据已完成（ER-08）。
- [ ] 「修订历史」已记录成稿后的全部规范性变更（G-09）。
- [ ] `plan-long.md` 相关状态或日期计划索引已同步，或明确 `N/A`。
