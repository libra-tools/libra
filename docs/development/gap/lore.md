# Lore → Libra 能力差距补齐计划

本文是以 Epic Games Lore VCS 为参照，规划 Libra 如何补齐用户可见能力差距的落地计划。命令名、模块名、协议名、crate 名、特性名保留原文，避免失真。

参考项目路径：`/Volumes/Sky/EpicGrames/lore`

校验时间：2026-06-19

参考版本：Lore `0.8.4-nightly`，见 `/Volumes/Sky/EpicGrames/lore/Cargo.toml`

命令面权威来源：Lore 的 clap CLI 在 `/Volumes/Sky/EpicGrames/lore/lore-client/src/cli/`（`cli.rs` + `commands/*.rs`），而非 `lore/src/interface.rs`（后者是 `lore-capi` 的 `extern "C"` C-ABI 面，印证 Lore「API/C-ABI 第一」的产品形态）；命令文档见 `docs/reference/lore-cli-commands.md`。本次修订已对照 Lore `lore-client` CLI 与 Libra 源码逐条复核（含 §3、附录 A 的事实更正），更正了若干把 Libra 已实现能力误列为缺口、以及把 Lore 命令层级压平的描述。

## 0. 结论摘要

### 0.1 比较边界

本文只保留一个方向：

- **Lore → Libra。** 以 Lore 为参照，分析 Libra 为了补齐 Lore 的用户可见能力需要做什么，同时保持 Libra 的核心身份：Git 磁盘格式兼容、Git 协议兼容、SQLite 管理可变状态、AI agent 原生。

“补齐”不是复制 Lore 底层。Libra 不应复制 Lore 的 BLAKE3 对象 ID、node-block、partition 能力边界或无 index 模型。每一项能力都必须落到 Libra 自己的架构上：Git object/index/pack、SQLite 侧表、`Storage` trait、LFS、分层云存储、hooks、MCP/agent 接口。

### 0.2 当前重新核对后的关键事实

对 `/Volumes/Sky/EpicGrames/lore` 的源码核对修正了旧文档里的若干过期判断：

- Lore 当前 workspace 版本为 `0.8.4-nightly`。
- Lore 命令面已经包含 `status --scan`、`status --check-dirty`、`dirty`、`stage --scan`、`stage --case`、`service`、`notification`、`completions`、`shared-store set-use-automatically`、`branch diff`、`branch reset`、`branch protect`、`branch archive`、`branch metadata`。
- Lore 的 `clone`/`sync` 已经包含 `--root-file`、`--dependency-tag`、`--dependency-recursive`、`--dependency-depth-limit` 等 dependency-based selective clone/sync 入口，Libra 若补齐同类能力，应复用 sparse/materialization 语义，不能单独做一套选择性同步模型。
- Lore 已经把 modified file tracking 作为 LEP 实现方向写清楚，且 CLI 中已有 `Status`、`Stage`、`Dirty` 快捷入口。
- Lore 的 roadmap 明确把可扩展锁、VFS、links/layers、桌面/Web/Unreal 客户端、edge 拓扑、forks/isolated partitions 放在 2026 以后持续推进。

### 0.3 最重要的落地判断

- **Libra 补 Lore：高度可落地的是增量式 CLI、缓存、元数据、auth、dirty-set、冲突 UX、object alternates、sparse v1。** 这些都能通过 SQLite 侧表、现有 `Storage` trait、LFS、分层云存储、hooks、MCP/agent 接口实现，不破坏 Git 格式。
- **Libra 补 Lore：需要谨慎推进的是 per-worktree HEAD/index/refs 隔离、obliteration、hydrating VFS。** 这些有真实价值，但牵涉面大，必须分阶段推进。
- **LFS FastCDC 必须作为最后支持的特性。** 它不是纯客户端功能；跨机器去重、断点续传、按需水合都需要 Libra-aware media 服务端协议支持，因此单独放在 §6，等 sparse、shared store、auth、fsck/heal、obliteration 等基础能力稳定后再做。
- **Libra 不应推进自研 Lore 式服务端协议、BLAKE3 对象格式、partition 作为仓内能力边界、移除 Git index。** 这些会破坏 Libra 的立身之本。

## 0.4 按请求维度的方案评审结果（修订驱动）

### 0.4.1 结论评分（1-5）

评分列为「修订前→修订后」：修订后分值反映本文档已落入正文的改进，对应的具体交付物在「修订决策」列给出章节锚点。

| 维度 | 评分（前→后） | 风险点 | 修订决策（已落入正文） |
|---|---|---|---|
| 合理性 | 4→4 | 目标与 Lore 用户体感基本对齐，但部分“可做/可不做”边界未精确定义，且少数条目对 Lore/Libra 现状描述失真 | 保留 `Git 兼容优先` 红线；更正 1.9/2.6/2.10/links/node-block/SWFS 等失真（见 §3、§3.5、附录 A），每项功能映射到明确替代模型 |
| 可行性 | 4→4 | 一部分项的前置依赖判断有误（1.6 vault、2.6 存量、2.3 周数估算） | 修正 1.6/2.6/2.3 前置事实，并在 §3.0.1 为每项加“四面兼容矩阵 + schema/migration/回滚” |
| 完整性 | 3→4 | 非功能性交付物（兼容性矩阵、错误码兼容、配置演进、回滚策略）未进入正文 | 新增 §3.0.1 强制门禁模板、§3.6 收敛点、附录 A 补充行，补齐遗漏命令面 |
| 安全性 | 3→4 | token 生命周期、日志脱敏、权限边界仅零散出现 | 新增 §4.2 逐特性威胁模型、§4.3 保留撤销、§7.9 隐私节，复用既有脱敏/vault/审计原语 |
| 功能正确性与接口兼容性 | 3→4 | 部分功能语义未给出 `--json`/exit code/错误码与一致性约束 | §3.0.1 钉死 `--json` 信封/schema 演进与 `StableErrorCode`/退出码契约，§6 明确 fsck Obliterated 退出语义 |
| 数据流与控制流正确性 | 2→4 | 缺少关键路径状态转换与事务边界 | 新增 §7.1.1 dirty 生命周期表、§2.5/§7.7 obliteration 状态机、§7.2 branch reset 原子边界、§7.1 scan 隔离与自愈闭环 |
| 性能与效率 | 3→4 | 未设定容量边界和复杂度上限 | 扩充 §7.6 预算表（默认 status/--scan/working_dirty/heal），新增 §7.6.1 量化基准回归门禁与淘汰演进约束 |
| 可靠性与容错性 | 4→4 | 已有方向但缺故障注入、幂等重试、本地原子写 | 点出本地非原子写缺口（§7.7）、补退避幂等/上限（0.2）、§7.10 故障注入矩阵、§7.7 service 恢复协议 |
| 兼容性与互操作性 | 4→4 | 标准 CLI/LFS 兼容测试场景不完整，且 push 现状误判 | 修正 push 已支持四 flag（2.10）、§6.3 `media_oid` 恒 SHA-256、§6.2/§6.9 Libra LFS 互操作边界、§3.0 双 hash 门禁 |
| 可扩展性与可维护性 | 3→4 | 缺插件化扩展边界与代码所有权划分 | 新增 §3.6 收敛点/owner，禁止命令内懒建表，定义退役策略与提案模板脚手架 |
| 合规性与标准符合性 | 3→4 | 对供应商依赖、凭证、审计、备份保留缺可执行条款 | 新增 §4.3 保留撤销、§7.9 独立 Privacy 节、许可证（MIT→MIT）与供应链结论、§6 与 LFS quota 服务对齐 |

### 0.4.2 主要修订结论

1. 保留现有 `Phase 0 -> 3 -> FastCDC` 大框架，但把 Phase 间边界从“功能顺序”改为“功能 + 验收门禁”。
2. 进入下一阶段前，schema 与 migration 必须完成并具备回滚路径；CLI 与数据兼容门禁必须通过；关键故障场景必须有集成测试覆盖。
3. 将 `fsck --heal`、`backoff`、`verify`、`auth` 组合为全局基础设施，不作为单点阶段依赖，而是每个后续阶段默认继承的能力。
4. 不改变 Git 兼容命令的默认语义来换取 Lore 式性能。类似 Lore 默认缓存化 `status` 的行为，在 Libra 中应以显式 `--cached`、`--check-dirty` 或新子命令形式提供。

### 0.4.3 已补充的治理条目（建议直接落到计划中）

- 增设 `docs/development/lore.md` 里的 `compat checklist`，至少包含：
  - `git status/commit/add/diff/log/push/pull` 标准路径；
  - `lore` 参考能力在 `status --scan/stage --scan`、`branch diff/reset/protect/archive/metadata`、`file obliterate`、`shared-store` 上的行为对照；
  - `--json` 输出、退出码、错误代码不变性。
- 新增统一安全清单：secret 存储加密、token 过期/撤销、scope 粒度、日志脱敏、审计事件字段清单。
- 所有新增持久化表和能力都必须明确迁移步骤与降级方案。

## 1. 两套架构的根本差异

### 1.1 Lore 的核心架构

Lore 是集中式、面向大型二进制资产、内容寻址的版本控制系统。它的关键设计是：

- **存储子系统与版本控制子系统解耦。** `ImmutableStore`/`MutableStore` 抽象承载 BLAKE3 地址、FastCDC 分块、递归分片、CAS 可变指针；revision/branch/merge/sync 建在其上。
- **API-first。** `lore-capi/lore.h` 是一等产物，CLI、server、IDE、SDK 都是薄客户端。
- **无 Git index。** 文件系统是事实来源；dirty/staged 是 Merkle 树节点上的正交状态。
- **partition 是访问边界。** 16 字节 partition/context 体系承载多租户和权限隔离。
- **面向大资产规模。** FastCDC、fragment cache、shared-store、layers、links、VFS、obliteration 都围绕超大文件和超大仓库。
- **服务端中心。** `lore-server`、`lore-transport`、`lore-proto` 提供 QUIC/gRPC、复制、通知、鉴权和运维面。

### 1.2 Libra 的核心架构

Libra 是 Rust 实现的 Git 兼容 VCS，同时加入 AI agent 原生运行时。它的关键设计是：

- **Git 磁盘格式兼容。** loose objects、index、pack/pack-index、SHA-1/SHA-256 是基本承诺。
- **Git 协议生态兼容。** smart HTTP、SSH、git://、LFS 是远端互操作基础。
- **SQLite 管理可变状态。** refs、HEAD、config、reflog、AI runtime contract 等放在 `.libra/libra.db`。
- **分层对象存储。** 本地 + S3/R2 + LRU + D1/R2 备份 + Cloudflare Worker read-only publish。
- **AI 原生运行时。** `src/internal/ai/` 下已有 agents、orchestrator、MCP、sandbox、automation、providers、skills、goal/supervisor、usage、session、prompt、TUI `libra code`。

### 1.3 设计原则

- 能力按用户价值迁移，底层按本系统架构实现。
- 任何破坏 Git 兼容的 Lore 能力，在 Libra 中只能改造或推迟。
- 默认 CLI 行为优先保持 Git 兼容。Lore 式缓存快路径必须通过显式 flag、配置或新命令启用，并在输出中标明数据新鲜度。
- 新命令必须同步 CLI help、命令文档、兼容测试、`tests/INDEX.md`、错误码文档和端到端测试。
- 新生产代码不得引入无说明的 `unwrap()`、`expect()` 或 `panic!()`。

## 2. Libra 相对 Lore 的能力缺口

| 主题 | Libra 当前状态 | 缺口判断 | 落地性 |
|---|---|---|---|
| 稀疏/VFS/惰性水合 | 有 bare/shallow、`.libraignore`、tiered LRU、FUSE worktree 基础 | 缺 sparse view、view-filtered checkout/sync、object alternates、hydrating VFS | sparse 和 alternates 可落地；hydrating VFS 推迟到 Phase 3 |
| 工作区人体工学 | Git index + status 全量 reconcile；worktree 共享 `.libra` | 缺 dirty-set、`status --cached`、`status --check-dirty`、真正 per-worktree HEAD/index/refs | dirty-set 可落地；worktree 隔离牵涉大 |
| 冲突 UX | 有 index stage 1/2/3 和 merge/cherry-pick/revert | 缺 `restore --ours/--theirs`、diff3、`merge --dry-run`、统一 sequencer | 高可落地，优先做 |
| branch 便捷命令 | Git 风格命令较多 | 缺 `branch diff`、`branch reset`、protect/archive metadata | 中高可落地 |
| diff/merge 深度 | `A..B`/`A...B`/`diff A B`/`diff A`/`--`、五个空白选项均已支持 | （`--diff3` 属 1.3 的 merge.conflictStyle，已落地；Git 无 `diff --diff3`） | ✅ 已落地 |
| typed metadata | 有 notes、config_kv | 缺 repo/branch/revision/file typed metadata | 可落地，建议作为基石 |
| obliteration | 无真正字节擦除 | 缺合规删除 | 可做但必须诚实限制，难度高 |
| auth/ops | 无完整 `libra auth`，日志和 telemetry 不完整 | 缺 token/keyring/OTLP/completions/resource knobs | 大多可落地 |
| locking | LFS lock push-enforced | commit/add 阶段未强制 | 可落地 |
| 服务端/复制 | Git client only | 无 Lore 式 server、QUIC/gRPC、replication、partition | 大多推迟，不建议复制 |

## 3. Libra 补齐计划

### 3.0 跨阶段落地约束

每个阶段都必须同时交付功能、接口契约、数据模型、测试和运维说明。缺少任一项时，只能作为实验能力保留。

| 约束 | 必须回答的问题 | 验收方式 |
|---|---|---|
| 接口契约 | 命令、flag、exit code、`--json` schema 是否稳定 | CLI help、docs、compat 测试同步 |
| 数据模型 | SQLite 表、对象索引、远端元数据是否可迁移和回滚 | migration 测试、旧库打开测试 |
| 安全边界 | token、host、repo、branch、path scope 如何传递 | 拒绝用例、日志脱敏用例 |
| 容错恢复 | 中断、重试、部分写入、远端失败如何处理 | chaos/fault 注入测试 |
| 互操作 | 普通 Git、标准 Git LFS、现有 Libra repo 是否继续可用 | interop 测试和降级路径 |
| 性能预算 | 热路径复杂度、并发上限、缓存淘汰策略是什么 | 大仓库 smoke/benchmark |
| hash-format 兼容 | SHA-1 与 SHA-256 仓库下 OID 的存取、校验、跨仓库共享是否一致；是否硬编码 20/32 字节 | 每个触碰 OID 的功能（dirty-set `working_dirty`、verify-on-cache、object alternates、obliteration、FastCDC manifest）都必须在 sha1/sha256 两类仓库下各跑 interop，复用 `cli.rs` 的 hash-kind preflight，禁止假定 hash 字节宽度 |

### 3.0.1 每能力项强制门禁模板（对标 Lore LEP）

借鉴 Lore LEP 工艺（见 `/Volumes/Sky/EpicGrames/lore/docs/proposals/README.md`），每个 Phase 1/2/3 编号项动工前必须填写并通过下表，缺任一格只能作为 feature-gate 实验保留。§5.1 的全局门禁视为本模板的默认继承项。

#### (A) 四面兼容矩阵（任一格不得裸 N/A，N/A 须给理由）

| 兼容面 | 必答内容 | 拒绝标准 |
|---|---|---|
| Git 磁盘格式（objects/index/pack/refs/LFS pointer） | 是否新增/改动磁盘字段；新仓库能否被旧 Libra 读、旧仓库能否被新 Libra 读 | 注入私有不可解析字段即拒绝 |
| Git 线协议（smart-HTTP/SSH/git://、标准 LFS） | 报文是否变化；旧客户端对新远端、新客户端对旧远端各看到什么；是否需能力协商 | 破坏标准 Git/LFS 互操作即拒绝 |
| SQLite schema/migration | 新表/列是否幂等迁移、可探测版本、可只读降级；有无配套 `*_down.sql` | 无 down 迁移或无旧库打开测试即拒绝 |
| CLI/public API | 命令、flag、`--json` schema、退出码、错误码是否稳定且向后兼容 | 改变现有 Git 兼容命令默认语义即拒绝 |

样例（1.1 dirty-set）：Git 磁盘格式=不变（仅 SQLite 侧表）；Git 线协议=N/A（纯本地）；SQLite=新增 `working_dirty` 表，幂等迁移 + 只读降级；CLI=新增 `--cached/--check-dirty/dirty`，默认 `status` 行为不变。

#### (B) 命名分期迁移（触碰持久化/工作区语义的项强制，如 1.1、2.1、2.5、2.6）

| 相位 | 旧库×新二进制 读/写 | 新库×旧二进制 读/写 | 回滚触发 | 回滚后可恢复状态 |
|---|---|---|---|---|
| 灰度（feature-gate 默认关闭） | … | … | … | … |
| 早期过渡（默认开，保留旧路径） | … | … | … | … |
| 默认启用（移除旧路径） | … | … | … | … |

后向兼容硬约束：旧二进制遇到高于自身已知最高 `schema_version` 的仓库时，纯读命令（status/log/diff）只读放行并打印版本警告，写命令返回可操作的「请升级 libra」错误而非 panic；须新增 interop 测试「旧二进制打开新 schema 仓库」。

#### (C) Security / Privacy（禁止裸 N/A）、Assumptions、Alternatives

- **Security**：是否改变信任模型、恶意 peer/构造仓库能否滥用、新数据是否完整性/机密性敏感；无安全影响也须解释原因。
- **Privacy**：哪些路径/标识/元数据对服务端、peer、telemetry、日志可见；是否影响删除/脱敏/过期能力。
- **Assumptions**：每条带 `*invalidated if:*`；**Risks**：每条带 `*mitigation:*`。
- **Alternatives Considered**：≥2 个备选各带具体拒绝理由。特别地，1.1 的扁平 `working_dirty` 侧表必须说明：它正是 Lore modified-file-tracking LEP 在 Alternatives 中**显式否决**的「flat path-based dirty set」（Lore 否决理由是其 merkle staged anchor 需子树遍历集成），但对 Libra 成立——Libra 以 Git index 为骨架、无 merkle staged anchor，子树 diff 由 Git tree object 天然提供。

### 3.1 Phase 0：速赢项

这些项独立、增量、不会触碰 Git 对象格式，应优先落地。

| 编号 | 项目 | 为什么做 | 落地建议 | 风险 |
|---|---|---|---|---|
| 0.1 | `libra completions <shell>` | CLI 人体工学，Lore 已有 | 用 `clap_complete` 生成；补 docs/compat/tests | 低 |
| 0.2 | 429/503/`Retry-After` 退避 | 对齐 Lore `SlowDown`，避免云端打爆 | `D1Client`、`RemoteStorage`、`https_client` 统一指数退避 + full-jitter，含 `max_retries`/`max_delay`/`total_deadline` 上限（防尾延迟无界），`Retry-After` 超 `max_delay` 时钳制并记 warning；只对幂等动作（GET/exists/按内容 hash 的 PUT）自动重试，非幂等动作（D1 INSERT、finalize、URL 分配）须带 idempotency-key 或「先查后写」（参照 `update_object_index_once`）；退避/失败日志须脱敏——URL 过 `redact_url_credentials`、不回显完整响应体与 presigned 签名（D1 现有 `format!("D1 API error: {}", body)` 与 `{:?}` 须改） | 低（含脱敏改造） |
| 0.3 | 取数即校验 | 远端对象不能盲信 | 缓存写入前按当前 hash format 校验 OID | 中，需覆盖 SHA-1/SHA-256 |
| 0.4 | `fsck --heal` | 从 durable tier 修复缺失/损坏对象 | 重取、校验、落盘；不得伪造对象；v1 必须预留 intentional-absence 跳过位——即使 2.5 obliteration 状态机尚未落地，heal 也只重建「本应存在」的对象，对每个待修复对象动作前先查 object index 的有意缺失标记，命中即跳过且不发起远端重取 | 中，需和 obliteration 状态前向兼容 |
| 0.5 | `flush(sync_data)` / `--sync-data` | 明确磁盘耐久性 | loose object 和父目录 fsync | 低 |
| 0.6 | `Storage::exist_batch` | 批量去重预检查 | `Storage` trait 现为 4 方法（get/put/exist/search）；默认实现（逐对象 exist）无性能收益，去重预检查的实际价值在 `remote.rs`/`tiered.rs` 的批量 override，批量远端请求须复用 0.2 的退避/限流；`publish_storage` 不实现该 trait，无需改动 | 低 |
| 0.7 | rolling logs / `logfile info` | 生产日志可控 | `tracing-appender` 滚动策略 | 低 |
| 0.8 | `--offline/--local/--remote` | 控制取数来源 | dispatch context 带 read policy | 中，需清晰错误 |
| 0.9 | 全局资源限制 | 防止大仓库/CI 资源失控 | `--max-connections`、文件数/大小/压缩/线程/search 限制 | 中 |
| 0.10 | store/cache 可调参数 | 暴露已有 LRU 能力 | reserved config 或 `cache configure` | 低 |

推荐顺序：0.2 → 0.3 → 0.4。`fsck --heal` 会走远端重取路径，必须继承退避和校验逻辑。

### 3.2 Phase 1：基础项

这些项直接提升日常体验，并为 Phase 2/3 铺路。

| 编号 | 项目 | 落地性分析 | 依赖 |
|---|---|---|---|
| 1.1 | dirty-set cache、`libra dirty`、`status --cached`、`status --check-dirty` ✅ 已落地 | `working_dirty`(+meta) 表（migration 2026070202）+ 属主 API `internal::dirty::DirtyCache`；新鲜度键 index 尾部校验和指纹 + HEAD OID（staged 快照并存，`--cached` 免 HEAD 树加载达成 O(dirty)），任何改索引/HEAD 的命令免费隐式失效（§7.1.1 回退条款）；`--scan` TOCTOU 防护（前后指纹复核，不一致中止留旧快照）+ 扫描锁（陈锁可窃）；`--cached` 疑问即降级全量 + 提示；`--check-dirty` O(dirty) 复核并 prune；人工标记 over-report-only。默认 status 字节不变（JSON 无新键，测试钉住）。快照语义（扫描后纯工作树编辑需标记/重扫）已文档化；逐命令 carry-over 与 watcher（1.11）为后续增量 | migration |
| 1.2 | `restore --ours/--theirs` ✅ 已落地 | 已有 index stages 1/2/3 可读，属于低风险高价值项 | 门禁已确认：merge/rebase/cherry-pick 均写 stages 1/2/3（merge.rs:815-829、rebase.rs:3629-3646、cherry_pick.rs:1165-1171）。核心 --ours/-2/--theirs/-3/--merge/--conflict/--ignore-unmerged 早已实现；本轮补 Git-fidelity：modify/delete 缺失 stage 在默认 no-overlay 下删除工作树文件（exit 0）、`--overlay` 下报错；rebase 下 --ours=onto/新基、--theirs=被重放提交（Git 语义 swap，读 stage 逐字，无需特判，仅文档）。非冲突 pathspec 仍为 unmerged-only（有意差异，不复制 Git 的 stage-0 fallthrough 以免静默回退 dirty 文件）|
| 1.3 | diff3 conflict markers、`merge --dry-run`、`--restart` ✅ 已落地 | diff3：Git 兼容配置 `merge.conflictStyle`（merge/diff3，非法值/读失败硬错），merge+cherry-pick 共享行级渲染器输出 `||||||| base` 祖先块（rebase 独立整文件实现暂不支持）。`--dry-run`（Libra 扩展）：预演 ff/up-to-date/clean/conflict 而零写入——含对象库（`try_merge_blob_contents` 以 persist=false 仅内存计算自动合并 blob）；干净退出 0、会冲突退出 1（结果信号，非真实冲突的 128）。`--restart`（移植 Lore `branch merge restart`）：复用 `restore_pre_merge_state`（与 --abort 共享崩溃安全顺序）后对**记录的 target 提交**确定性重跑（原合并选项不重放，文档化） | 1.2 |
| 1.4 | positional diff、whitespace flags ✅ 已落地 | 实况：`A..B`/`A...B`（merge-base）与 -w/-b/--ignore-space-at-eol/--ignore-blank-lines 早已实现；本项落地 `diff A`/`diff A B`/`--staged <rev>`/`--` 分隔符 + Git 双歧义错误（退出 129，Libra CLI 约定）与 `--ignore-cr-at-eol`（strip-all 近似 + Git-exact blank 分类）。标题中的 `--diff3` 系笔误——Git 无此 diff flag，diff3 冲突风格已在 1.3 经 merge.conflictStyle 落地 | rev-parse |
| 1.5 | branch/repo metadata KV ✅ 已落地 | 统一 `metadata_kv` 表（migration 2026070201，scope/target/key/value/value_type 预留 1.10）+ 单一属主 API `internal::metadata::MetadataKv`（ON CONFLICT upsert、fail-closed `is_protected`）；repo 作用域 = config_kv `metadata.*`（双面入口）；branch delete/rename/copy 生命周期级联；CLI `libra metadata get/set/unset(clear)/list --branch\|--repo`。protect/archive 仅记录未执行——执行统一归 1.13 branch-policy | migration |
| 1.6 | `libra auth` v1 ✅ 已落地（OS keyring 诚实延后 2.7——行文自身指定 vault 为文件 fallback） | 生命周期同 PR 闭环：login（令牌仅 stdin/隐藏提示——**无 --token flag**，argv 泄历史）/status（绝不出密文，--host 可脚本化）/logout/clear（免解密撤销，键旋转后可用）；AES-256-GCM + 0600 全局 vault key，`auth.token.*` 对 config 全面封锁（含 unset）；读取侧 build_split 挂接（scope 命中 + https-only/loopback 豁免 + 不覆盖既有头 + sensitive 标记）；**https→http 降级重定向一律拒绝**（审阅 must-fix：reqwest 只在 host/port 变化剥凭据）；host 归一化先补 scheme 再解析（审阅 must-fix）；credential fill 全局回退（用户名钉定）；顺带修复既有 P1——`lazy_init_vault_for_scope("global")` 每调用旋转密钥毁全部既有全局密文（e2e 首跑暴露） | vault |
| 1.7 | OTLP telemetry ✅ 已落地（traces-only v1） | `otlp` feature + 四个 optional opentelemetry 依赖（默认二进制零影响：cargo-tree 空 + 常驻 compat guard 钉 default/optional/cfg 门控）；**结构性允许清单**——仅 `libra::telemetry` 目标可导出（Targets 每层过滤），v1 唯一 span = canonical 命令名 + 时长 + LBR-* 失败码（lore.md:725：无 URL/令牌/路径/ref/身份；Resource 空 builder 防 OTEL_RESOURCE_ATTRIBUTES 吸入）；门控 = feature ∧ 显式端点 ∧ !OTEL_SDK_DISABLED，无默认端点，https-only（loopback http 豁免）；http-proto + blocking reqwest（不用 tonic：init/flush 在无 runtime 的 main 线程——审阅实证的唯一站得住理由）；fmt 层排除遥测目标（LIBRA_LOG 输出字节不变——审阅 must-fix）；main() scopeguard 双出口 flush；已知限制文档化：~21 个 process::exit plumbing 命令丢 span、库内嵌者无遥测；wire test（mock collector 实收 + 无路径泄漏）进 CI（--features otlp 专行）；metrics/logs/子 span/gRPC/采样延后 | opentelemetry crates |
| 1.8 | `merge --autostash` ✅ 已落地 | Git-faithful 合并属主状态机：脏树（含 staged）在合并前推入 HELD stash 提交（不入 `stash list`——MERGE_AUTOSTASH 模型，sidecar `merge-autostash.json` 原子+fsync，OID 字符串存储 sha1/sha256 通吃，GC 根不变量记录于 dev doc）；合并结束（干净成功/up-to-date/squash/启动失败/--continue/--abort）时回贴；冲突时 HELD（跨 --restart 循环存活——restart 以 preserve_held_autostash 跳过陈旧回收）；回贴冲突则提升入 stash list + 通知（不丢失，回贴 all-or-nothing 且新增纯添加与未跟踪文件的碰撞守卫）；`merge.autostash` git-bool 配置（非法值硬错误）+ `--no-autostash` 覆盖；pull 合并路径搭载（rebase 路径保留旧 push/pop 包裹）；JSON 增量 `autostash: applied|stashed|kept`；顺带修复陈旧 compat guard（pull --help 早已暴露 --autostash/--commit 而 deny 表未更新） | stash |
| 1.9 | `log --trailer`（含 `--only-trailers` 展示）✅ 已落地 | 共享 Git-faithful trailer 解析器 `internal::log::trailer`（末段块定位+首段排除、alnum/dash key 字符集、续行记双非、注释透明、25% 规则、cherry-pick 行仅入 raw 块）；`log --trailer KEY[=VALUE]`（AND 过滤）与 `--only-trailers`（展示）为 Libra 扩展；`--json log` 增量 `trailers` 字段；shortlog `--group=trailer:` 改走共享解析器（收紧对齐 git）。**顺带修复三个写侧 bug**：`-s`+`--trailer` 现同块、`append_trailers` 恒空行分隔、`--cleanup=strip` 折叠连续空行而非删除全部段落分隔（用户 trailer 块不再在写入时被毁）。`%(trailers)` pretty 占位符为后续项（1.10 复用解析器） | log parser |
| 1.10 | typed metadata 命令族 ✅ 已落地（file 作用域除外） | 类型值 `--numeric`/`--binary`（1.5 预留的 value_type 列，零迁移；repo 拒绝类型旗标——config 无该列，显式后续项）；`--revision` 作用域 = 不可变 trailer 块（1.9 解析器，requested-key-as-recognized hook）+ 可变 notes 层（`refs/notes/metadata` 单 JSON 文档/提交，notes 优先，key 大小写不敏感，本地不推送，全文档 ≤1MiB）——本行「revision 用 trailers/notes」即 §3.6:268 统一表红线对本作用域的显式豁免（不开新表、单一属主 API 保持）；本增量无收敛/替换（§272 空满足，无退役窗口需求）。file 作用域延后独立设计轮：无现存 side-tree 机制，且 204「file 用 side-tree」与 268 红线互相矛盾，需先裁决（记录于 metadata dev doc） | 1.5、1.9 |
| 1.11 | 无头 `libra service` + notification v1 ✅ 已落地（UDS/监视器/透传延后有因） | 环回专属（解析期字面环回 IP + 绑定期直构 SocketAddr + 每端点对端校验，绝不开对外 TCP 端口）；notification v1 = `{seq,type,at,data}` SSE 总线，at-most-once（滞后收 resync、seq 随重启归零——权威态只在 SQLite，§7.9）；dirty/automation 承载走 0600 令牌门（**事件流同样门禁**——其它本机 uid 不受信）+ 256KiB 体积上限；标记经 1.1 校验属主 API（逃逸整批拒绝、只会过报）；§7.10 kill-9 行实测（标记存活/锁回收/stale status）。UDS（或-分支已满足）、监视器（加速器，免新重依赖）、repo 透传、MCP、守护化、§7.7 重放、code_ui 重基：延后有因（dev doc 表）；1.6 依赖读法（本地令牌已满足最小访问控制）已记录待裁决 |
| 1.12 | `branch diff` ✅ 已落地 | 纯 CLI 糖：`BranchSubcommand::Diff` 经共享 `delegate_to_diff`（diff_plumbing 抽取）转发 `--old/--new`（免歧义步行）或三点粘连（`--merge-base`，复用引擎 merge-base 与 NoMergeBase）；默认 subject=当前分支、base=其 upstream（无则报错+提示）；tip-to-tip（不涉工作树）、与 `diff A..B` 字节一致（测试钉住）；未知侧转分支 UX（levenshtein 建议）；保留字防护——flags 使 clap 落回位置参数时 `new_branch=='diff'` 一律拒绝（绝不静默建名为 diff 的分支，逃生口 `switch -c diff`），审阅者以 spike 实证 args_conflicts_with_subcommands 不会自动报错 | 1.4 |
| 1.13 | `branch reset` ✅ 已落地 | `BranchSubcommand::Reset`：with_operation_log 单事务内（顺序等价 lore.md:635——单原子事务下 CAS 读与 protect 判定次序语义等价）fail-closed 重查 protect/archive（垃圾值视为受保护；哨兵字符串穿透 DbErr 保留 LBR-POLICY-001 类型化错误）+ 重查 checked-out（并发 switch 不能造成幻影 staged diff）→ 引用更新 + `insert_single_entry` 分支 reflog（不伪造 HEAD 条目）；index/工作树零触碰（字节级测试钉住）；无 `--force`——显式 `metadata unset` 解除（可审计）；**update-ref 同步纳管**（其事务内同查 protect/archive，更新与删除都拒——否则是策略旁路，审阅 must-fix；其余保持 plumbing 语义可动 checked-out 分支）；新稳定码 LBR-POLICY-001（Conflict 类别，docs/error-codes.md 同步）；metadata 通知三处措辞更新；同参 5s 去重窗拒绝（文档化）；main 允许 reset（默认分支锁护删除/改名身份，不锁尖端移动——刻意决定已记录） | 1.5 |
| 1.14 | 文件大小写变更处理 ✅ 已落地 | 基底 `utils::path_case`：fold 近似（char::to_lowercase，文档化与 NTFS/APFS 表差异，miss 方向 fail-open）+ `core.casehandling`（`error` 默认/warn/allow，非法值硬错误）+ 有效大小写不敏感判定（显式 `core.ignorecase` git-bool > 运行时探针（dev+ino 确认——canonicalize 在 macOS 返回查询拼写不可用，审阅 must-fix）> false）；init 全平台真实探针写 ignorecase（替换 Windows 硬编码）；mv 大小写改名一等公民（同 inode+fold 判定、免 --force、绕过 force-remove 数据毁灭分支、直接 rename 优先+两步回退、目录 case 改名不再嵌套）；add 双胞胎预防（error 整体拒绝 LBR-CASE-001+mv 提示/warn 跳过警告/allow 静默，任何模式都不产生索引双胞胎）；switch/checkout 两处（审阅 must-fix：checkout 有自己的 restore 副本）树物化预检——在 HEAD 更新与任何工作树写之前原子拒绝（实测修复：守卫在 restore 内太迟，HEAD 已移动）。延后有因（dev doc）：status 咨询、scan 冲突记录、Unicode NFC/NFD（APFS 亦规范化不敏感）、clone 初始 checkout/merge/reset 树写入者接线、真实大小写不敏感 FS 上 warn 后的抖动调和 | 1.1 |
| 1.15 | 低层 in-memory revision tree ✅ 已落地（Git-plumbing 形态；MCP-first handle 延后有因） | 行文「或」允许二选一：既有 plumbing 已覆盖 80%（update-index --cacheinfo/write-tree/read-tree/hash-object -w/update-ref 均在），v1 补齐两处真实缺口——(1) `libra commit-tree`（tree+parents+message→commit 对象，零 index/worktree/HEAD/ref 副作用；消息经 format_commit_msg 前导 \n 分隔——审阅 must-fix：git-internal to_data 不加分隔符；-m/-F 可混用组序拼接；空消息拒绝=D 先例、恒不签名、无日期覆盖——三者文档化为有意差异+后续项）；(2) `--index-file` scratch 索引重定向上 update-index/write-tree/read-tree（GIT_INDEX_FILE 等价物；缺失文件=空索引→canonical empty tree；组合环路端到端测试钉住共享索引字节不动）。「in-memory」字面诚实：v1 scratch 是临时文件；真·内存态即延后的 MCP 有状态 handle（MCP 服务器今日 28 工具全一次性，引入首个跨调用状态需生命周期/驱逐/授权设计轮——dev doc 留草图） | 1.10 |
| 1.16 | revision ordinal index ✅ 已落地（find --metadata 延后有因） | 迁移 2026070301：`revision_ordinal`(+meta) 逐 ref FIRST-PARENT 链 1..N 编号（决定性=tip 纯函数，重建复现同一投影，测试钉住）；每次读同事务 ensure_fresh（指纹 = tip OID + **refs/replace 摘要**——审阅 must-fix：replace 不动 tip 却改有效链；快进 APPEND 不重编号、重写/replace 变更全量重建——1.1 never-lie）；非首父提交无序号（显式未命中）；`libra revision find -n/number/index --rebuild`（rebuild 兼清扫已删 ref，池死锁教训：分支列举在事务前）；`find --metadata` 延后（查询语义未决 + 每查询逐提交 notes 读，dev doc 留草图） |

Phase 1 的优先建议：先做 1.2、1.3、1.4 这组冲突和 diff 体验，再做 1.5、1.10 元数据基石，随后做 1.1 和 1.11 服务化能力。

### 3.3 Phase 2：组合与规模

| 编号 | 项目 | 落地性分析 | 风险 |
|---|---|---|---|
| 2.1 | per-worktree HEAD/index/refs 隔离 | 价值最高，但会触碰所有 ref/HEAD/index resolver；需要 per-worktree `instance_id` 和迁移 | 高 |
| 2.2 | sparse view filter | v1 不必等 2.1，可先用 repo-level materialized set；后续再做 per-worktree view | 高，误判删除风险 |
| 2.3 | object alternates / shared store | 五块工作：(1) `clone --reference/--shared/--dissociate` 三 flag 与解析；(2) alternates 来源记录与 resolver；(3) gc 的 alternates-aware 可达性（绝不删仍被共享 store 引用者）；(4) `--dissociate` 对象回拷断链；(5) 写前 OID/权限/来源校验 + interop。建立链接前须校验两仓库 `core.objectformat` 一致（SHA-1 不能引用 SHA-256 对象库），gc/fsck 对悬空 alternates 返回可操作错误。按 §3.0 五件套与 schema+migration+回滚 验收，不以周数承诺 | 中 |
| 2.4 | layers | 本地 overlay，不进入 commit，可作为组合能力入口 | 中 |
| 2.5 | index-flagged obliteration | 引入 `Live → Obliterating → Obliterated`（不可逆）两阶段状态机，状态记录在 SQLite 侧表/object index（非 Git 对象内）。**触发边**：授权 + 引用计数评估通过后 `Live→Obliterating`；先持久化 intentional-absence tombstone **再**物理删 payload，完成后 `Obliterating→Obliterated`。**不变量**：保留可寻址登记项使引用它的 commit/tree 仍可遍历，物理删除只作用于 payload，绝不删除引用计数未归零的共享对象（对照 Lore §19.6 保留 ADDRESS 删 PAYLOAD）。**中断恢复**：进程在 Obliterating 崩溃后须能从侧表续做或补写 tombstone，绝不停留在「payload 已删但标记 Live」。**fsck 区分**：新增 `IntentionalAbsence` 诊断类别，与 `Missing` 可区分，且默认不把退出码改为非 0；heal/backup/gc 对 `Obliterating` 与 `Obliterated` 两态都跳过重取（因先写 tombstone 后删 payload，关闭 heal 把中间态误判为 Missing 而复活的竞态窗口）| 高 |
| 2.6 | 统一 sequencer | 现状是四套散落状态存储：merge `merge-state.json`、revert `revert-state.json`（运行时仍是 JSON，且存在已注册但无源码读取的 `revert_sequence` 迁移孤儿表）、cherry-pick `cherry_pick_state`、rebase `rebase_state`（后两者命令内 `CREATE TABLE IF NOT EXISTS` 懒建，`cherry_pick_state` 同时又有 `MigrationRunner` 注册，构成懒建/迁移双轨）。收敛为单一 `SequenceState` 必须：(1) 由单一 owner 模块独占该表、禁止命令内懒建 DDL，全部走 `sql/migrations/`；(2) 给出四套旧存储→新表的迁移与回退，删除或并入 `revert_sequence` 孤儿表，并覆盖「迁移时存在未完成 in-progress 序列」边界；(3) 建立完整且对称的跨序列互斥——今天 merge 与 rebase 已用 `ensure_no_cherry_pick_in_progress` 挡 cherry-pick，但 cherry-pick/merge/revert 的启动都不互查（故 merge 冲突未解时仍可起 cherry-pick），统一后任一序列 in-progress 须以 `ConflictOperationBlocked` 拒绝其它序列 | 中高 |
| 2.7 | interactive auth + OS keyring | 在 token auth v1 后扩展 | 中 |
| 2.8 | commit/add lock enforcement | `lfs.lockEnforce=warn|block`，保持 LFS server 与 local lock store 边界清晰 | 中 |
| 2.9 | 后台 cache evictor | 独立可做 | 低 |
| 2.10 | push 协议精修 | `--atomic`、`--signed`、`--push-option`、`--follow-tags` 已支持（见 COMPATIBILITY.md push 行与 push.rs，已做服务端 capability 协商）；真正缺口仅为把当前作为 no-op 接受的 `--force-if-includes`、`--thin/--no-thin` 落为真实语义，并对真实 Git 服务端做互操作回归确认降级而非协议硬失败 | 中 |
| 2.11 | default shared-store | 2.3 后的小配置项 | 低 |

推荐顺序：2.3 → 2.2 → 2.1。object alternates 独立且高价值；sparse v1 不依赖 worktree 隔离；worktree 隔离虽然是并行 agent 的关键，但应等更小的规模能力先稳定。

### 3.4 Phase 3：Lore parity gated extensions

| 编号 | 项目 | 为什么推迟 | 进入条件 |
|---|---|---|---|
| 3.1 | file dependency graph | 新子系统，不能只做一次性表 | typed metadata 完成 |
| 3.2 | dependency-filtered clone/sync | 必须复用 sparse materialization 语义 | 2.2、2.3、3.1 |
| 3.3 | hydrating VFS | 需要 sparse、alternates 和可靠失败恢复；v1 只做整对象水合，不做 FastCDC range hydration | 2.2、2.3 |
| 3.4 | link/subtree composition RFC | Libra 已有不做 submodule 的产品边界，必须先 RFC | metadata、sparse、auth |

### 3.5 明确不做

Libra 不应复制这些 Lore 机制：

- BLAKE3 作为 Git 对象 ID。
- 320 字节 revision state、96 字节 node（49280 字节 node-block = 128 字节头 + 512 个 node）、mmap 零拷贝 node 格式。
- FastCDC 分块替代 Git blob 对象寻址。FastCDC 只能作为最后阶段的 LFS media 层增强，不能进入 Git object graph。
- 仓内 partition 作为读权限边界。
- Context/per-file identity 字段进入 Git tree。
- 移除 Git index。
- 在 Git 对象内做“只擦除某个引用”的 byte-level obliteration。
- 树节点内嵌 conflict/merge 标志位。
- C ABI 作为 Libra 第一产物。
- QUIC/gRPC 自研存储协议。
- SWFS 专有驱动（面向 Windows 的外部 VFS provider，经生成的 C 绑定消费，且在 Lore 当前构建中已注释禁用、并非在产公共能力面，无需借鉴）。
- 不把 Lore link/layer 的组合语义当作 submodule 的默认替代品。Lore 中 link（版本化、记入 revision）与 layer（本地、物化时叠加）是并列原语，并无「默认组合模型」，默认开启的只是 link 的 auto-follow，整体默认运作模式是 lazy fetch；Libra 的任何组合能力先经 §3.4 的 RFC 评估，不在本计划内默认引入。

原因统一是：这些会破坏 Libra 的 Git 兼容和 AI-agent-native 身份。用户可见能力可以借鉴，底层不能照搬。

### 3.6 收敛点与模块所有权

本维度红线：每个跨命令共享的可变状态必须有唯一 owner 模块与唯一写入入口，禁止在多个命令里各自懒建表或各写一份 JSON。新增持久化表只能经 `sql/migrations/` + `MigrationRunner` 注册，**禁止**在命令执行路径内 `CREATE TABLE IF NOT EXISTS` 懒建（现存 `cherry_pick_state`、`rebase_state` 的命令内懒建列为待清理技术债，2.6 收敛时一并消除；形状变更须先 `PRAGMA table_info` 探测再迁移，避免 `IF NOT EXISTS` 静默 no-op 漂移）。

| 收敛点 | owner 模块 | 单一写入入口要求 | 相关项 |
|---|---|---|---|
| ref/HEAD/reflog 更新 | `internal::branch` + `reference` model | 所有 ref 变更（reset、push、merge、rebase、agent 自动化）统一经 branch policy + CAS，禁止命令直接 UPDATE reference 表 | 1.13、2.1、4.1 |
| sequencer 状态 | 新 `SequenceState`（2.6） | merge/revert/cherry-pick/rebase 共用一张表一套 load/save/clear | 2.6 |
| typed metadata | repo=`config_kv`，其余走统一 metadata 表 | 一套读写 API，禁止每类元数据各开一张表 | 1.5、1.10 |
| auth token | `vault` 扩展的 token store | 仅一处存取，统一 host scope 校验 | 1.6、2.7 |
| media manifest/chunk | Libra media 层 | 仅一处 manifest 索引，GC/fsck/heal/obliterate 共用 | §6 |

退役策略：任何「收敛/替换」型变更（2.6、1.10）须声明旧存储的只读兼容窗口与终止版本、一次性幂等迁移、旧 DDL/旧文件读写代码与孤儿表（如 `revert_sequence`）的删除计划、以及旧库删除后仍可探测并给出升级提示。无退役计划的收敛变更不得合入。

## 4. Libra 方向的落地风险

### 4.0 关键假设（Assumptions）

每条前提若失效将直接废掉对应能力，须在动工前验证：

- **假设**：外部编辑器/文件监听对文件系统变更的检测足够可靠，使 `--check-dirty` 与 dirty-set 在不全量扫描时仍准确。*invalidated if:* OS 级变更通知不可靠到 dirty 状态长期陈旧——此时默认 `status` 仍走全量 reconcile（§4.1 已缓解），`--cached`/服务化快路径降级或禁用。
- **假设**：sparse view 规则可版本化并可回滚，out-of-view tracked 文件不会被普通工作区删除路径触及。*invalidated if:* 任一 merge/rebase/checkout 绕过 sparse-aware update——此时阻断 materialization（§4.1 已缓解）。
- **假设**：所有 heal/backup/gc 路径都能读取并尊重 obliteration 的 intentional-absence tombstone。*invalidated if:* 任一恢复路径不理解 tombstone——此时禁用该路径自动修复直至补齐。
- **假设**：`libra auth` token 始终携带 host scope 且远端按 scope 校验。*invalidated if:* 存在无 host scope 的历史 token 或远端不校验——此时拒绝保存/发送该 token（§4.1 已缓解）。
- **假设**：远端 `chunks/exists` 已按 repo/remote scope 隔离。*invalidated if:* 服务端退化为全局 hash 查询——此时客户端拒绝 chunked LFS，回退标准 LFS。

**风险（Risks，逐条缓解见 §4.1 矩阵）：**

- **dirty-set 与 Git index 双真相。** 所有 mutating command 必须维护一致性；默认 `status` 必须保留安全全量 reconcile。
- **worktree 隔离牵涉面大。** refs、HEAD、index、reflog、config、worktree list/prune/move 都会受影响，必须有迁移测试。
- **sparse 误判会导致数据损坏。** out-of-view tracked files 不能被当成删除；merge/rebase 必须能更新树对象而不物化文件。
- **obliteration 必须诚实。** Git 内容寻址会让同内容文件共享同一对象；擦除对象会影响所有引用；真实 Git 客户端读到缺失对象会失败。
- **auth 必须防 token 泄漏。** `libra auth` v1 必须同时实现 host scoping，不能先存 token 再以后补防泄漏。
- **feature gating 必须严格。** OTLP、VFS、LFS chunking 不能拖累默认 CLI 和 CI。

### 4.1 风险缓解矩阵

| 风险 | 最小缓解措施 | 不满足时的处理 |
|---|---|---|
| dirty-set 过期导致漏报 | 默认 `status` 继续全量 reconcile；缓存路径只在 `--cached` 或服务化集成中启用 | 不允许默认启用 |
| SQLite migration 破坏旧仓库 | 每个 migration 提供版本探测、备份和只读降级 | 停留在实验 feature-gate |
| branch protect 被绕过 | 所有 ref 更新统一经过 branch policy 检查，包括 reset、push、merge、agent 自动化 | 阻断命令并返回明确错误 |
| sparse/out-of-view 文件误删 | out-of-view 路径不由普通工作区删除路径处理，必须走 sparse-aware update | 阻断 materialization |
| shared store 对象污染 | alternates 只读优先；写入必须校验 OID、权限和来源 remote | 拒绝缓存写入 |
| auth token 泄漏 | keyring 优先、文件存储最小权限、日志脱敏、host scope 强制匹配 | 拒绝保存或发送 token |
| obliteration 误复活 | intentional absence 状态参与 fsck、backup、heal、gc | 禁用 heal/backup 自动修复 |
| FastCDC chunk 越权读取 | 所有 chunk 操作必须绑定 repo/media_oid/token scope，禁止全局 hash GET | 回退标准 LFS |

### 4.2 逐特性威胁模型与拒绝测试要求

凡涉及 credential、remote、shared store、obliteration、locking、FastCDC 的特性，进入实现前须在其设计条目下补一张六栏威胁模型小表，并复用 Libra 已有安全原语不得重造：日志脱敏走 `redact_url_credentials`；凭证落盘走 `vault`（继承其威胁模型：防仓库级读，不防整机失陷）；拒绝路径返回既有 `LBR-AUTH-001`（缺凭证）/`LBR-AUTH-002`（权限拒绝），无现成码须新增 `StableErrorCode` 变体并同步 `docs/error-codes.md`。

| 栏目 | 必须回答 |
|---|---|
| 资产 | 被保护对象（token、unseal key、chunk bytes、manifest、obliteration tombstone、lock 记录） |
| 信任边界 | 数据跨越的边界（本机↔远端、repo↔repo、agent↔人工、客户端声明↔服务端校验） |
| 威胁 | 具体攻击（token 泄漏/重放、跨 repo 侧信道、未 finalize 读取、tombstone 复活、lock 绕过） |
| 强制校验入口 | 唯一收口函数/中间件（不得旁路，与 §7.2 单一入口同构） |
| 拒绝错误码 | 命中威胁时返回的稳定错误码 |
| 拒绝测试 | 至少一个集成测试断言「攻击输入被拒绝且不泄漏存在性/内容」 |

首批必须填满的特性：1.6 auth（token 撤销/过期/重放/host 不匹配拒绝）、2.5 obliteration（授权 + tombstone 完整性 + heal/backup/gc/fetch/clone 统一查 tombstone 收口 + 已删对象拒绝重建）、2.8 lock enforcement（block 模式以服务端 lock 为权威、local store 仅离线建议、`unlock --force` 须授权审计、stale lock 保守拒绝）、§6 FastCDC chunk（见 §6.7 防侧信道矩阵）。任一栏写 N/A 须给理由，禁止裸 N/A。

### 4.3 数据保留与撤销（Retention / Revocation）

禁止裸 N/A。各类含身份/路径/凭证的数据须给出保留窗口、清理触发与撤销语义：

| 数据类别 | 保留/撤销策略 | 默认值（可配置） |
|---|---|---|
| audit event | 最小保留窗口 + 滚动清理；清理动作自身写一条 audit | 90 天 / `audit.retentionDays` |
| token（1.6/2.7） | 支持过期时间与显式撤销（`auth logout`/revoke），撤销后本地与 keyring 同步清除，host-scope 记录留存备审 | `auth.tokenTtl` |
| D1/R2 备份 | 保留窗口 + 最少保留份数；超期清理不得删除仍被 live refs/manifest 引用者 | 30 天 / ≥3 份 |
| obliteration tombstone（2.5/§6.8） | intentional-absence 须长期保留以阻止 heal/backup 复活；tombstone 本身不参与 retention 清理 | 永久（不可配置，合规约束） |
| FastCDC manifest `created_by`（§6.3） | 仅记录客户端版本与能力集，不含用户身份/主机名/邮箱；随 manifest obliterate 一并删除 | — |

撤销语义：任何 token/credential 的撤销必须幂等且可审计；撤销后若仍能用于远端写入即视为缺陷。备份与 audit 的保留清理必须对 `Obliterated` 状态保守，禁止误复活（与 §4.1、§7.7 一致）。

## 5. 推荐推进路线

1. 先做 Phase 0 中的 backoff、verify-on-cache、`fsck --heal`，提高存储可靠性。
2. 同步补 `completions`、resource knobs、read policy flags，提高 CLI 可用性。
3. 优先落地 `restore --ours/--theirs`、diff3、`merge --dry-run`，因为现有 index stages 已经提供数据基础。
4. 建 branch/repo typed metadata 基石，再做 branch protect/archive/reset/diff 和 file/revision metadata。
5. 做 object alternates，再做 sparse v1，最后再推进 per-worktree HEAD/index/refs 隔离。
6. Hydrating VFS、obliteration 放到明确依赖满足后，不要提前开工。
7. LFS FastCDC 作为最后支持的特性，必须等 §6 的服务端协议、能力协商、鉴权、GC、fsck/heal 设计冻结后再实施。

## 5.1 推进前置门禁（新增）

- 文档与兼容性：变更必须同步 `docs/commands/*.md`、`COMPATIBILITY.md`、`docs/error-codes.md`、`tests/INDEX.md`。
- 运行模型：新增能力默认走 feature-gate；每个阶段先灰度发布再默认启用。
- 数据模型：新增持久化都必须给出 `migration + 回退步骤 + 验证脚本`。
- 错误契约：同一行为必须保持 `--json` 与 `stderr` 输出结构稳定。
- 可观测性：关键流程都必须输出 trace 事件（操作、范围、耗时、失败码）。
- 兼容性：默认命令行为不得因为 Lore parity 发生破坏性变化；任何不兼容模式必须显式开启并写入文档。
- 安全性：涉及 credential、remote、shared store、obliteration 的 PR 必须包含拒绝用例和日志脱敏用例。

## 6. 最后支持的特性：LFS FastCDC chunking

### 6.1 为什么必须最后做

LFS FastCDC chunking 的目标是把大文件按内容定义边界切成 chunk，在多个版本、多个 clone、多个客户端之间复用相同 chunk，从而降低传输、存储和水合成本。这个能力接近 Lore 的 binary-first 优势，但它不能早做，原因是：

- **它不是纯客户端功能。** 只在本地 cache 分块只能节省本机磁盘，无法让另一台机器复用 chunk，也无法让远端做断点续传和按需水合。
- **标准 Git LFS server 不理解 Libra chunk manifest。** 普通 LFS 协议只认识一个 pointer 对应一个完整 media object；直接上传 manifest 会破坏互操作。
- **它依赖前置能力。** 需要 auth/token host scoping、verify-on-cache、`fsck --heal`、object index、远端退避、shared store、sparse/hydration 语义、GC 和权限边界先稳定。
- **它会放大安全风险。** 如果远端允许“知道 chunk hash 就能下载”，chunk hash 会变成读能力，等价于绕过 repo/branch/file 权限。
- **它会影响运维生命周期。** GC、backup、restore、obliteration、audit、quota、retry、range fetch 都必须理解 chunk manifest，否则会误删、复活或无法修复数据。

因此 FastCDC 在路线图中排在最后：先完成 Phase 0–3 的基础能力，再把它作为 Libra-aware LFS/media 协议扩展实施。

### 6.2 基本约束

FastCDC 设计必须遵守以下约束：

- **Git blob 不变。** Git object graph 仍然只保存标准 LFS pointer 或普通 blob；FastCDC chunk 绝不成为 Git object ID。
- **标准 LFS 兼容优先。** 对不支持 Libra 扩展的远端，必须回退到标准 Git LFS 完整 media object 上传/下载。
- **chunking 只存在于 Libra 私有 media 层。** Libra 可以在自己控制的 R2/S3/Worker/D1 或 Libra-aware LFS endpoint 中保存 chunk manifest 和 chunk objects。
- **远端能力必须显式协商。** 客户端不能假设远端支持 chunked LFS，也不能把 Libra manifest 偷塞给普通 LFS server。
- **读写都必须鉴权。** chunk 查询、上传、下载、manifest 读取、GC 标记都必须绑定 repo、remote、object、identity 和 token scope。

### 6.3 对象模型

FastCDC media 层建议引入三类对象：

| 对象 | 标识 | 内容 | 存储位置 |
|---|---|---|---|
| LFS pointer | Git blob OID | 标准 LFS pointer，保持 Git/LFS 兼容 | Git object store |
| media manifest | `media_oid` 或 `manifest_id` | 文件大小、完整 media hash、chunk 列表、chunker 版本、压缩/加密/校验信息 | Libra media metadata：SQLite/D1/Worker API |
| chunk object | `chunk_hash` | chunk bytes，可选压缩 | R2/S3/local chunk store |

manifest 至少包含：

- `version`：manifest schema 版本。
- `algorithm`：例如 `fastcdc-v1`。
- `media_oid`：完整 LFS media object 的 hash，用于兼容和端到端校验。
- `media_size`：完整文件大小。
- `chunks[]`：每个 chunk 的 `offset`、`length`、`chunk_hash`、`encoded_length`、`compression`、`crc32c` 或强校验 hash。
- `created_by`：客户端版本和能力集，便于迁移。
- `fallback_oid`：可选，指向标准完整 media object；用于非 Libra 客户端或旧远端 fallback。

chunk hash 可以使用 SHA-256 或 BLAKE3，但不能暴露为 Git object ID。为减少项目复杂度，建议优先使用与 Libra 当前 object format 一致的强 hash，并在 manifest 中记录算法。

`media_oid` 必须恒为 SHA-256，与标准 Git LFS pointer（`oid sha256:...`，见 `src/utils/lfs.rs`，`LFS_HASH_ALGO = "sha256"`）严格一致，独立于仓库 `core.objectformat`——否则 SHA-1 仓库会算出与标准 LFS 不兼容的 `media_oid`，破坏 fallback 与端到端校验。`chunk_hash` 可在 manifest 的 `algorithm` 字段自描述（SHA-256 或 BLAKE3）；压缩与寻址正交——`chunk_hash` 对未压缩字节计算（Lore media 层可能用 Oodle/Lz4 而非仅 Zstd），这与 Git 对 `blob <size>\0` 包裹后做 SHA 的寻址函数根本不同，也是 FastCDC 不能进入 Git object graph、chunk-only 不可与只认不透明完整 media object 的标准 LFS server 直接互通的根本原因。

### 6.4 远端能力协商

客户端在执行 LFS 上传/下载前必须探测远端能力。建议新增 Libra media capability endpoint，或在 Libra-controlled Worker/API 中提供等价能力：

```text
GET /libra/media/v1/capabilities
Authorization: Bearer <token>
```

响应示例：

```json
{
  "version": "1",
  "chunked_lfs": true,
  "chunk_algorithms": ["fastcdc-v1"],
  "hash_algorithms": ["sha256"],
  "max_chunk_size": 8388608,
  "max_manifest_size": 10485760,
  "supports_batch_exists": true,
  "supports_range_read": true,
  "supports_standard_lfs_fallback": true
}
```

协商规则：

- 如果远端没有 capability endpoint，按标准 Git LFS 处理。
- 如果 `chunked_lfs=false`，按标准 Git LFS 处理。
- 如果算法不兼容，按标准 Git LFS 处理。
- 如果远端支持 chunked LFS，但当前 repo policy 禁用，按标准 Git LFS 处理。
- 客户端必须在日志和 `--json` 输出中标明使用了 chunked LFS 还是 fallback。

协商安全默认（永不半写入）：capabilities 返回客户端不识别的更高 `version` → 视为不支持 chunked LFS，走标准 LFS；endpoint 超时或返回 5xx → 继承 §0.2 退避重试，重试耗尽后回退标准 LFS 并在 `--json` 标明 fallback 原因；远端 `supports_standard_lfs_fallback=false` 而本地又无完整 fallback object → 阻断操作并报可操作错误，禁止静默 chunk-only 上传。

### 6.5 上传协议

上传流程建议如下：

1. 客户端按 FastCDC 切块，计算完整 media hash 和每个 chunk hash。
2. 客户端请求远端批量查询缺失 chunk。
3. 客户端只上传远端缺失的 chunk。
4. 客户端上传 manifest。
5. 远端验证 manifest 引用的 chunk 全部存在，且 size/hash 匹配。
6. 远端将 manifest 与 LFS media OID 关联。
7. 如果远端要求标准 fallback，客户端同时上传完整 media object，或由服务端异步合成 fallback object。

建议 endpoint：

```text
POST /libra/media/v1/chunks/exists
POST /libra/media/v1/chunks/upload-url
PUT  <presigned chunk upload url>
POST /libra/media/v1/manifests
POST /libra/media/v1/manifests/{manifest_id}/finalize
```

`chunks/exists` 请求必须带 repo/remote/object scope，不能只按 `chunk_hash` 查询全局存在性，避免跨仓库侧信道泄漏。

`finalize` 必须是原子动作：只有 manifest、chunk、权限、quota、fallback policy 全部满足时，才把 `media_oid -> manifest_id` 标记为可读。

上传生命周期与幂等：manifest 须有显式状态 `Pending → Finalized`（再到 §2.5 的 `Obliterated`）。`Pending` 态的 chunk 不被任何 LFS pointer 可达，GC 不能按可达性回收，必须由超时清理识别超过 TTL（默认覆盖最大重试/续传时长）的 Pending manifest，连同其专属孤儿 chunk 一并回收（仅引用计数未被任何 Finalized manifest 共享者才物理删除）。`finalize` 是唯一原子提交点，用 `media_oid → manifest_id` 的 CAS 完成；重复 finalize 幂等（已 Finalized 且 `manifest_id` 一致返回成功，不一致按 tip 冲突拒绝）。任一阶段崩溃后重放为 `chunks/exists → 仅补缺 chunk → 重发 manifest → finalize`，因 exists 与 finalize 均幂等，不产生重复 payload。

### 6.6 下载和按需水合协议

下载流程建议如下：

1. 客户端按标准 LFS pointer 得到 `media_oid`。
2. 客户端查询 Libra manifest。
3. 如果 manifest 不存在或不支持，走标准 LFS 下载完整 media object。
4. 如果 manifest 存在，客户端按所需范围下载 chunk。
5. 客户端重组文件，并用完整 `media_oid` 做端到端校验。

建议 endpoint：

```text
GET  /libra/media/v1/manifests/by-media/{media_oid}
POST /libra/media/v1/chunks/download-url
GET  <presigned chunk download url>
```

range hydration 规则：

- hydrating VFS v1 不依赖 FastCDC，只做整对象水合。
- FastCDC 落地后，VFS 才允许按 chunk 或 byte range 拉取。
- 客户端必须缓存 manifest，并对每个 chunk 做 hash 校验。
- 完整文件落盘或提交前必须校验完整 `media_oid`，不能只信 chunk hash。

### 6.7 鉴权与隔离

服务端必须把每个 chunk 操作绑定到授权上下文：

- repo ID / remote URL。
- LFS media OID。
- branch 或 ref scope，若服务器支持 ref-level 权限。
- token identity 和 host scope。
- operation：read、write、delete、gc、obliterate。

禁止的设计：

- 只按 `chunk_hash` 提供公开 GET。
- 在不同 repo 之间泄漏“某 chunk 是否存在”。
- 允许未 finalize 的 manifest 被下载。
- 允许客户端声明 manifest 成功而服务端不验证 chunk 存在性。

防侧信道语义：`chunks/exists` 只在调用方对 (repo, media_oid) 有读权限时返回真实存在性；对无权 chunk，响应必须与「chunk 不存在」不可区分（同响应码、同时延特征），使攻击者无法通过探测 `chunk_hash` 判断他人 repo 是否含某内容。逐威胁拒绝测试矩阵（每条一个独立断言）：仅凭 `chunk_hash` 无 scope 的 GET 被拒；跨 repo 探测存在性返回与「不存在」不可区分；未 finalize 的 manifest 下载被拒；服务端对客户端声明的 manifest 强制校验每个 chunk 存在性与 size/hash；过期/越权 token 对 read/write/delete/gc/obliterate 各操作分别被拒且不泄漏存在性；fallback 路径下不暴露任何 chunk 级端点。

### 6.8 GC、fsck、heal、obliteration

FastCDC 必须同步扩展维护命令：

- `fsck`：验证 manifest schema、chunk 存在性、chunk hash、offset/length 连续性、完整 media hash。
- `fsck --heal`：缺失 chunk 从 fallback object 或远端副本重建；若无来源，报明确错误。
- `gc`：从 Git refs/LFS pointers 出发标记 live manifest，再标记 live chunks；不能删除仍被 manifest 引用的 chunk。
- `obliterate`：删除 media manifest 和相关 chunk 引用；若 chunk 被其他 media 共享，只删除授权对象的 manifest 引用，只有引用计数归零才物理删除 chunk。
- backup/restore：必须同时备份 manifest index 和 chunk objects；恢复时先恢复 chunk，再 finalize manifest。

特别约束：`fsck --heal` 和 backup 不能复活已处于 `Obliterated` 状态的 media/chunk。FastCDC 必须复用 obliteration 的 intentional-absence 状态。

### 6.9 标准 LFS fallback

为了保持互操作，必须保留 fallback：

- 对普通 Git LFS server：上传/下载完整 media object。
- 对 Libra-aware server 但禁用 chunked LFS 的 repo：上传/下载完整 media object。
- 对普通 Git 客户端：仍可通过标准 LFS pointer 获取完整 media object，前提是远端保留 fallback object。
- 如果 repo policy 选择“chunk-only，无完整 fallback object”，必须在文档和 CLI 输出中明确该仓库不再对普通 LFS 客户端完整兼容。

建议默认策略：**保留标准完整 LFS fallback object**。等 Libra-aware remote、GC、quota、obliteration 和 VFS range hydration 稳定后，再允许用户显式选择 chunk-only 策略。

互操作边界澄清：Libra LFS 使用 `.libra_attributes` 与内置 pointer/lock/batch client，**不**写 `.gitattributes`、**不**挂 git-lfs filter/hooks（有意差异，见 COMPATIBILITY.md 与 `_compatibility.md` D5）。因此「标准 LFS fallback」只保证 Libra 客户端 ↔ 标准/Libra-aware LFS server 之间 media object 的完整与互通；一个纯 `git`/`git-lfs` 客户端 clone 该仓库时不会识别哪些 blob 是 LFS pointer，也不会触发 smudge。若要对纯 git 客户端完整互操作，须把 `.gitattributes`/git-lfs filter bridge 作为单独前置项纳入 §6.10 门槛，不能默认其成立。

### 6.10 实施门槛

FastCDC 开工前必须满足（每条前置改为引用对应项的验收门禁，替换「已稳定/已定义」的主观措辞）：

- `libra auth` token+host scope+非交互 ⇒ 1.6 全部门禁通过；
- backoff/verify/heal 稳定 ⇒ 0.2/0.3/0.4 集成测试通过；
- object index 能表达 manifest/chunk/intentional-absence ⇒ 2.5 状态机 migration + 旧库测试 + heal 跳过测试通过；
- shared store/sparse/VFS v1 ⇒ 2.2/2.3/3.3 各自 v1 门禁通过；
- 文档明确标准 LFS 兼容/fallback/chunk-only 行为差异；
- 集成测试逐条覆盖 §6.7 禁止设计（见上方 §6.7 防侧信道与拒绝矩阵）。

## 附录 A：Lore 命令到 Libra 计划映射

| Lore 命令/能力 | Libra 当前类比 | Libra 计划 |
|---|---|---|
| global `--offline/--remote/--local/--sync-data/--cache` | 部分全局 flag | 0.8（`--offline/--local/--remote`）、0.5（`--sync-data`）、0.9（资源限制）、0.10（`--cache`）|
| global `--gc` / `--non-interactive` | `gc`（无全局触发 flag）；prompt 无统一抑制 | 待补：全局 `--gc` 触发 gc；`--non-interactive` 抑制所有交互（关联 1.6/2.7 auth 非交互）|
| `status --scan` + `status --check-dirty` + `dirty` + `stage --scan` | `status` + `add` | 1.1 |
| `stage --case` | 无直接等价 | 1.14 |
| `branch diff` | `diff A..B` | 1.12 |
| `branch reset` | 无直接等价 | 1.13 |
| `branch protect/archive/metadata` | 无直接等价 | 1.5、1.10 |
| `lore branch merge {start\|into\|resolve\|restart\|abort\|unresolve}`（resolve 接 `mine\|theirs` 子命令、restart 为同级动词、`start --dry-run` 及全局 `--dry-run`） | 部分 merge/cherry-pick/revert + index stage 1/2/3 | 1.2、1.3、2.6 |
| `revision metadata/find number/find metadata` | `log`、notes、`commit --trailer`（已具备） | 1.9（`log --trailer`）、1.10（typed metadata）、1.16（revision ordinal index：新增 SQLite 侧表映射 commit OID↔单调递增序号，支撑 `find number`/`find metadata`，含 migration 与 `--json`）|
| low-level revision API LEP | plumbing + MCP 部分 | 1.15 |
| file metadata/dependency/obliterate | notes 部分 | 1.10、3.1、2.5 |
| dependency-based clone/sync | sparse/partial clone 方向 | 3.1、3.2 |
| `auth` | basic auth 交互 | 1.6、2.7 |
| `layer` | 无直接等价 | 2.4 |
| `link` | submodule/product boundary | 3.4 RFC（link/subtree composition） |
| `service` / `notification` | `libra code` 局部事件流 | 1.11 |
| `completions` | 无 | 0.1 |
| `shared-store` | 无直接等价 | 2.3、2.11 |
| `logfile` | `LIBRA_LOG_FILE` 部分 | 0.7 |

以下行补齐附录遗漏的 Lore 一级/子命令，并标明哪些「现状已对位、无需新增」以免误判缺口：

| Lore 命令/能力 | Libra 当前类比 | Libra 计划 |
|---|---|---|
| `lock acquire/status/query/release` | `lfs lock/unlock/locks`（已支持，含 `--force`/`--id`） | 现有 LFS 锁面已对位文件锁；缺口仅在 commit/add 阶段强制（2.8），无需新增独立锁命令族 |
| `unstage` / `reset` / `diff` / `history` | `restore --staged` / `reset` / `diff` / `log`（均已支持） | 现状已对位，无新增 |
| `repository verify`（+ `verify fragment`） | `fsck` | 0.4（`fsck --heal`）|
| `repository metadata get/set/clear --binary/--numeric` | `config_kv`、notes | 1.10（typed metadata，需补 binary/numeric 值类型）|
| `repository instance list/prune` | worktree list/prune | 2.1（per-worktree `instance_id`）|
| `repository gc` / global `--gc` | `gc` | 现状已支持（全局 `--gc` 触发待补）|
| `repository store immutable query` | 无直接等价 | 暂不做（Lore 私有 immutable store 查询）|

## 7. 数据流与控制流正确性补充（改进版）

### 7.1 `dirty-set` 与 `status`/`stage` 数据流

建议把 dirty 系统定义为四段式状态流：`worktree 变更 -> 显式 dirty 标记或扫描检测 -> working_dirty 落盘 -> index/stage reconcile`。
Libra 与 Lore 的关键差异是默认语义：Lore 默认 `status` 读 dirty flags；Libra 为保持 Git 兼容，默认 `status` 应继续返回全量准确结果，缓存化路径必须显式启用。

控制流要求：

- `status` 默认执行当前 Libra/Git 兼容的安全 reconcile，保证外部编辑器直接修改的文件不会漏报；
- `status --cached` 只消费 `working_dirty`，输出必须标明 `freshness=cached`；
- `status --check-dirty` 只复核已缓存 dirty 集合，复杂度应与 dirty 集合大小相关，而不是与工作树大小相关；
- `status --scan`/`stage --scan` 必须进入“扫描 + 校验 + 原子提交”事务，失败时保持旧状态不变；
- `libra dirty <paths>` 只更新 dirty cache，不读文件内容，不修改 index；
- `stage --scan` 可以合并扫描与 staging，但必须在 staging 成功后再提交 dirty cache 更新；
- `restore --ours/--theirs` 必须在同一事务内更新 index 与 working tree 的关联关系，避免“文件系统已变更但索引未更新”。
- `status --scan`/`stage --scan` 的扫描结果在原子提交前对并发读者完全不可见：并发 `status --cached` 始终读一致的 `working_dirty` 快照（提交前旧集合、提交后新集合，无半更新中间态）；同一仓库同时只允许一个 scan 写入事务，第二个 scan 快速失败提示已有扫描在进行；
- 检测到 `working_dirty` 与 index 不一致时不仅本次回退全量 reconcile，还要把 `working_dirty` 标记 `stale` 并在 `--json`（`cache_state=stale`）与 stderr 提示运行 `status --scan` 重建；`stale` 期间 `--cached` 持续回退全量，`status --scan` 是唯一权威重建入口，重建成功后清除 stale 标记。

错误处理要求：

- 发现 dirty cache 与 index 不一致时，默认回退全量 reconcile，不静默相信缓存；
- 路径不存在、大小写冲突、符号链接类型变化必须返回路径级错误，不能用全局成功掩盖部分失败；
- `--json` 输出应包含 `mode`、`checked_paths`、`cached_paths`、`stale_paths`、`errors[]`。

#### 7.1.1 dirty 标志生命周期转移表

`working_dirty` 与 index 是双真相，每个 mutating 命令对 dirty 条目的转移必须固定且与 index 写入同事务提交：

| 操作 | 对 working_dirty 的转移 |
|---|---|
| `add`/`stage <path>` | 置 staged，不清除该路径 dirty（已暂存仍可被外部再改） |
| `commit` | 清除被提交路径的 dirty + staged；保留仅 dirty 未暂存路径 |
| `reset --hard` | 清除受影响路径 dirty |
| `reset`（混合/软） | 保留 dirty |
| `restore --worktree` | 清除被还原路径 dirty；`--ours/--theirs` 同事务更新 index 与 worktree 关联 |
| `switch`/`checkout`（普通） | 保留 dirty；`--discard-changes`/强制切换清除 |
| `merge`/`rebase`/`cherry-pick`/`revert` | 用增量标志操作保留既有 dirty，不整表重置 |
| `stash push` | 保存后清除工作区 dirty；`stash pop` 恢复对应路径 dirty |

任何命令若无法在同一事务内同时更新 index 与 `working_dirty`，必须放弃缓存更新并使下次 `status` 回退全量 reconcile。

### 7.2 分支元数据与保护控制流

`branch protect/archive/reset/metadata` 建议走统一的 metadata 更新入口，按以下顺序处理：

- 授权与 scope 判定；
- 乐观并发检查（branch pointer/CAS）；
- 元数据写入与 reflog 记录；
- 失败重试遵循幂等约束，不出现重复保护/误删历史。

`branch reset` 必须区分“移动 HEAD”与“更新工作树”；若工作树污染，应返回可恢复错误。

`branch reset` 分两阶段且边界明确：阶段 1（权威提交，原子）授权/protect 判定 → branch pointer CAS → 在 SQLite 事务内写 reference + reflog，这是唯一的「已生效」提交点；阶段 2（工作树物化，可重跑）在权威提交成功后更新工作树，若工作树污染或物化失败则返回可恢复错误并保持已移动的 HEAD 不回滚，提示用户重跑 `checkout`/`restore`。禁止在工作树更新失败时回滚 reference（否则 reflog 与实际不符）；污染检查必须在阶段 1 之前完成，污染时直接拒绝且不写 reference。

### 7.3 alternates / sparse / VFS 控制

`object alternates` 与 `sparse` 的控制边界建议统一为：

- 来源解析：决定对象来源于当前仓库还是共享存储；
- 策略层：out-of-view 路径在 merge/rebase 下的处理；
- 执行层：工作区落盘前先写 staging 缓存，再提交工作区，保障 crash-safe。

`sparse` 的关键风控：

- merge/rebase 时 out-of-view 路径只更新树对象，不执行工作区删除；
- out-of-view 文件删除仅记录为状态变更，不直接删除磁盘文件；
- sparse 规则变更必须记录版本并支持回滚。

### 7.4 FastCDC 与标准 LFS 的互操作控制流

FastCDC 的控制流固定三阶段：

1. 能力协商：协商失败或不匹配时强制标准 LFS fallback；不发生半写入。
2. 上传：扫描 -> 查询缺失 chunk -> 上传缺失 chunk -> manifest -> finalize 原子提交。
3. 下载：manifest 查询 -> 按需 chunk 拉取 -> `media_oid` 统一验签。

### 7.5 阶段验收与接口兼容清单

- Git 兼容：`status`、`diff`、`merge`、`rebase`、`push/pull` 及标准环境下 exit code 与错误消息保持可回归性。
- LFS 互操作：标准客户端在无 chunk 能力时可正常工作；完整 fallback object 需保留，chunk-only 需显式告警。
- 安全合规：token、host scope、密钥、日志脱敏、撤销与过期策略必须有验收测试。
- 可靠性：fsck/heal 与 backup/restore 的恢复路径要对 `Obliterated` 状态保持保守，禁止误复活。

### 7.6 性能与效率预算

| 路径 | 目标复杂度 | 关键约束 |
|---|---|---|
| `status`（默认全量 reconcile） | O(worktree paths) + O(changed-bytes hashed) | 必须遍历完整工作树（`list_workdir_files_split_safe`）防外部编辑器直改漏报；内容 hash（`calc_file_blob_hash`）只对疑似变更文件触发 |
| `status --cached` | O(dirty paths) | 不遍历完整工作树 |
| `status --check-dirty` | O(dirty paths + changed-size reads) | 内容读取只发生在需要确认的 dirty 文件 |
| `status --scan` / `stage --scan` | O(scanned paths) + O(changed-bytes hashed) | 单次遍历完成「扫描 + 校验 + 原子提交」事务，结果写 `working_dirty` 供 `--cached` 复用；失败回滚不改 dirty cache |
| `working_dirty` 维护（每个 mutating 命令）| O(touched paths) SQLite upsert | 仅写本次受影响路径，批量进同一事务；与 index 不一致时回退全量 reconcile |
| `Storage::exist_batch`（默认逐个）| O(batch) HEAD 往返（远端）| 默认逐个 HEAD 非 bounded，仅作正确性兜底 |
| `Storage::exist_batch`（远端覆盖）| O(batch) + bounded round trips | 远端须真正批量探测，并发受 `--max-connections` 上限 + 429/503 退避；`publish_storage` 不实现 |
| shared store read | O(alternates 链长) resolver + O(object bytes) verify | 来源按 本地→各 alternate 顺序探测命中即短路，链长设上限；读不复制（`--dissociate` 才落本地副本），落盘前按当前 hash format 全字节校验 OID |
| sparse materialization | O(view paths + changed out-of-view metadata) | out-of-view 文件不做无界扫描 |
| `fsck --heal` / 远端重取（含退避）| O(missing objects) × (O(object bytes) 取+校验) | 退避须有最大重试次数与总退避时长上限防尾延迟无界；并发受 `--max-connections` 约束；对 `Obliterated` 对象不重取 |
| FastCDC upload | O(file bytes) + O(chunks) metadata | chunk 大小、manifest 大小、并发数必须受配置限制 |

默认资源上限建议从保守值开始：远端并发、open file 数、manifest 大小、chunk upload 并发、scan path 数都必须可配置，并在达到上限时返回可操作错误。

#### 7.6.1 大仓库基准与回归门禁

为防止 Lore parity 改动悄悄拖垮规模性能，定义确定性合成基准仓库与回归阈值（具体数值为建议起点，落地时校准）：

| 基准仓库 | 规模 | 覆盖命令 | 回归门禁 |
|---|---|---|---|
| small | 1 万 文件 / 无大文件 | `status`、`status --cached`、`add .`、`commit`、`diff` | p95 回归 >10% 即 fail |
| large | 10 万 文件 / 多个 100MB+ LFS | `status --scan`、`fsck`、`exist_batch`、`clone --sparse` | p95 回归 >10% 即 fail |

- 基准仓库脚本确定性合成（L1 可跑、不依赖网络）；远端相关项（`exist_batch` round trips、退避）在 L2/L3 用 mock/真实远端补测。
- `status --cached` 相对默认 `status` 必须有可度量的常数级耗时（与 dirty 集合大小相关、与工作树大小无关），否则 1.1 dirty-set 不达标。
- 资源旋钮须对应具体配置项：`--max-connections`（远端并发，排队+退避不无界）、`--max-threads`、`--file-count-limit`/`--file-size-limit`（达上限返回可操作错误而非静默截断）、`LIBRA_STORAGE_CACHE_SIZE`（LRU）、media 配置（chunk upload 并发/manifest 大小受远端 capability 协商上限约束）。
- 缓存淘汰从当前 `TieredStorage::put` 同步删盘（持锁内联 `CachedFile` drop）演进到 2.9 后台 cache evictor，`put` 热路径不得被淘汰 I/O 阻塞。
- benchmark 跑独立 CI job，仅在标注 `perf` 的 PR 与 nightly 触发。

### 7.7 可靠性与容错要求

- 所有远端写入按“准备 -> 上传 -> finalize”提交，finalize 前失败必须可重试或可清理。
- 本地写入原子性（**现状缺口，须先补**）：当前 `LocalStorage::put`（src/utils/storage/local.rs:506）、`merge-state.json`（merge.rs:185）、`revert-state.json`（revert.rs:481）及 cherry_pick/rebase 状态都是直接 `fs::File::create`/`fs::write` 落最终路径，既非原子也不 fsync——崩溃会在最终路径留半截文件，破坏后续 reconcile 与 sequencer 恢复。须引入统一 `write_atomic(path, bytes)`（写临时文件 → flush → sync_all → rename → fsync 父目录），所有 `.libra/` 下持久写（loose object、index、refs/HEAD、所有 sequencer 状态）一律改走该助手；`--sync-data`（0.5）控制 fsync 同步，状态/refs 路径默认开启。该项为 Phase 0 阻塞项。
- 对象缓存写入必须先校验 hash，再进入共享 store。
- `fsck --heal` 只能从可信 durable tier 或标准 LFS fallback 恢复，不能从未验证 cache 伪造对象。
- 崩溃恢复协议：每个可中断长操作在 SQLite 记录 `status ∈ {pending,in_progress,finalizing,done,failed}`、`owner_instance_id`、`heartbeat_at`/`lease_expires_at`。`libra service` 启动时扫描非终态记录：停在 finalize 前且操作幂等 → 自动重放；已越过不可逆点或租约过期且语义不明 → 标记 `needs_attention` 暴露给 `libra agent doctor`/CLI 供人工处理（现有 `agent doctor` 仅「报告」stuck sessions/orphan checkpoints，须升级为「分流入口」）。所有恢复写入与 ref 推进必须经 §7.2 单一授权+CAS 入口，不得绕过 branch protect。

### 7.8 合规性与标准符合性要求

- Git 对象、pack、index、refs、LFS pointer 不得引入 Libra 私有不可解析字段。
- 标准 Git LFS fallback 是默认策略；chunk-only 是显式 opt-in 且必须标记为非完全互操作模式。
- 日志脱敏统一复用 `src/internal/ai/observed_agents/redaction.rs` 的 `Redactor` 与 `DEFAULT_RULES`（已含 OpenAI/Stripe/GitHub/AWS/Slack 等密钥模式），所有 credential/remote/shared-store/obliteration 日志路径先过 `Redactor::redact` 再落盘，禁止各命令自写正则。
- Credential 存储以 `src/internal/vault.rs` 既有加密模型为基线（root token AES-256-GCM + HKDF-SHA256，unseal key 落 `~/.libra/`，明确不防整机失陷），优先接 OS keyring；vault 当前仅覆盖 PGP/SSH 密钥，HTTP token 存储（1.6）为新增写路径。
- audit event 复用 `src/internal/ai/tools/registry.rs` 经 hardening 的 `append_audit`/`flush_audit` sink，字段对齐 `PublishSyncRun`（schema_version/started_at/finished_at/status/cli_version），并含 `actor`、`operation`、`repo`、`remote`、`ref/path`、`object/media_id`、`result`、`error_code`、`timestamp`，新增 `auth_scope`、`approval_source`（人工/agent/自动化，agent 动作回溯到 `src/internal/ai/permission/` 批准记录）；审计记录追加写（append-only，0600），普通 VCS 命令不得删改；破坏性操作（obliterate、`lfs unlock --force`、`branch reset`/绕过 protect 的尝试、token clear）必须强制产生审计事件，且不含 token 明文或被擦除内容。
- obliteration 文档必须明确 Git 内容寻址的限制：同一对象可能被多个 path/ref 共享，物理删除会影响所有引用。

### 7.9 隐私评估与无状态性/确定性（Privacy / Statelessness / Determinism）

#### 隐私可见性（禁止裸 N/A）

| 数据 | 对谁可见 | 脱敏/限制 | 对删除-过期能力影响 |
|---|---|---|---|
| audit `actor`/`ref/path`/`remote` | 本机 audit sink；上报时含服务端 | 经 `Redactor` 过滤已知密钥；audit 默认仅本机，`path` 记仓库相对路径，上报需显式开启 | 受 §4.3 保留期约束 |
| OTLP telemetry span（1.7） | telemetry 后端 | feature-gated 默认关闭；只导出操作名/范围/耗时/失败码，禁含 remote URL、token、绝对路径、ref 名、用户邮箱；collector 端点必须用户显式配置并支持 TLS 校验 | 关闭即不离开本机 |
| manifest `created_by`（§6.3） | Libra-aware 远端 media 服务端 | 仅客户端版本与能力集，不含用户标识，不用于访问决策 | 随 manifest obliterate 删除 |
| auth token（1.6/2.7） | 仅本机 keyring/受限文件 | 明文不入 log/trace/审计/错误消息，错误只暴露 host scope 与 `LBR-AUTH-*` | §4.3 撤销/过期 |

#### 无状态性与确定性

- **Statelessness**：`libra service`（1.11）与 dirty-set 缓存（1.1）的全部权威状态落 SQLite，进程内仅缓存；崩溃/重启后从 `.libra/libra.db` 恢复，未完成的扫描/staging 事务回滚到旧状态。任何能力不得依赖仅存于内存的隐式状态。
- **Determinism**：FastCDC 切块边界由 `(algorithm 版本, min/avg/max 参数, 输入字节)` 唯一确定；manifest 字段顺序固定、`chunks[]` 按 offset 升序；同输入同算法版本必产生逐字节一致的 manifest，保证去重与可复现校验。

### 7.10 故障注入测试矩阵

把 §7.7 不变量逐条映射到注入点（crash 时机）+ 断言，每行须有对应集成测试（fail-point 或提前中断 future 实现注入）：

| 故障点 | 注入手段 | 恢复后断言 |
|---|---|---|
| loose object 写入（rename 前） | rename 前 panic | 最终路径无半截对象；仅残留 `.tmp`，可被 gc/clean 清除 |
| sequencer 状态写入 | 状态 json 写一半中断 | merge/revert/cherry-pick/rebase 状态要么完整可读要么干净缺失；命令报可操作错误而非 panic |
| 远端 finalize 前崩溃 | finalize 调用前杀进程 | 无 `media_oid→manifest` 可读映射；已上传 chunk 为孤儿，gc 可回收；重试幂等 |
| 远端 finalize 后崩溃 | finalize 返回后杀进程 | 重放 finalize 为 no-op（幂等键），不产生重复 |
| 上传中途 SIGKILL | 上传 N 个 chunk 后杀 | 重新运行只补传缺失 chunk（exists 预检），不重传已有 |
| cache 写入校验失败 | 注入 hash 不匹配的远端响应 | 拒绝写入共享 store，返回校验错误，不污染缓存 |
| service 进程被杀重启 | kill -9 后重启 | 从 SQLite 恢复：未完成操作被重放或显式标记需人工处理，绝不静默丢失 |
| heal/backup 遇 Obliterated | 对已 Obliterated 对象触发 heal | 不复活，返回 intentional-absence 状态 |
