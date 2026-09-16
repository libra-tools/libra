# Libra 集成测试计划（可落地执行版）

> 目标：把当前仓库的集成测试能力整理成一份“今天就能跑”的执行手册。
> 原则：只写仓库里真实存在的命令、测试文件和 gate；未实现能力统一标记为 `BASELINE_GAP-*`。

---

## 0. TL;DR（AI agent / Reviewer 必读，5 分钟版）

**默认阻断门**：Wave 0 + Wave 1 + Wave 2 全绿。Wave 3/4/5/6 按需启用。

**测试引用规范**（跨 agent 沟通统一使用）：

- 文件级：`code_ui_remote_lease_matrix`（即 `cargo test --test` 后跟的名字）
- 测试级：`code_ui_remote_lease_matrix::lease_expires_after_ttl`（三段式）
- 不要用文件相对路径或行号——rename / refactor 会失效；三段式稳定

**4 条最常用命令**：

```bash
# Wave 0：编译 + 格式 + lint + 文档一致性
cargo +nightly fmt --all --check && \
  cargo clippy --all-targets --all-features -- -D warnings && \
  cargo test --no-run --all-targets --features test-provider,test-network,test-live-ai,test-live-cloud && \
  cargo test --test compat_matrix_alignment -- --test-threads=1

# Wave 1：命令层 + 兼容性
cargo test --test command_test -- --test-threads=1

# Wave 2：Code UI 矩阵（最小子集）
cargo test --features test-provider --test code_ui_scenarios -- --test-threads=1

# Wave 4：Live AI（需 DEEPSEEK_API_KEY；务必先设成本闸门，见 §9.3）
cargo test --test ai_agent_test -- --test-threads=1
```

**3 个最常踩的坑**：

1. `cargo test --features A,B --test X --test Y` 的 features 是 union——不要把不需要某 feature 的测试和需要的合并到一条命令，否则前者也会被错误地带上 feature 编译。
2. `set -a; source .env.test; set +a` 会把密钥导出到当前 shell——后续工具/日志/截图都可能泄密。优先 `env $(grep -v '^#' .env.test | xargs) cargo test ...`。
3. 默认所有 wave 都加 `--test-threads=1`。当前仓库 64 个测试带 `#[serial_test::serial]`，并发会触发竞态。

**改完代码该跑哪些 wave**：见 §3.3 Path → Wave 表。
**review/handoff 协议**：见 §9 PR / Review 协议。

---

## 1. 现状基线（已存在资产）

| 资产 | 现状 | 证据 |
|---|---|---|
| 命令层集成测试汇总入口 | 已存在 | `tests/command_test.rs` + `tests/command/*.rs` |
| 兼容性专项测试 | 已存在 | `tests/compat/*.rs` + `Cargo.toml` `[[test]]` 注册 |
| Code UI Web process harness | 已存在（non-TTY） | `tests/harness/code_session.rs` |
| Code UI 事件流 harness | 已存在 | `tests/harness/event_stream.rs` |
| Code UI 数据驱动矩阵 runner | 已存在 | `tests/code_ui_remote_{lease,sse,state,security,generation,approval,model_generation}_matrix.rs` |
| Code UI 场景回归 | 已存在 | `tests/code_ui_scenarios.rs` |
| MCP 双入口回归 | 已存在 | `tests/code_mcp_dual_entry_test.rs` |
| resume 回归 | 已存在 | `tests/code_resume_test.rs` |
| codex runtime 回归 | 已存在 | `tests/code_codex_runtime_test.rs` |
| 网络层集成测试 | 已存在 | `tests/network_remotes_test.rs`（`test-network`） |
| Cloud live 集成测试 | 已存在 | `tests/cloud_storage_backup_test.rs`、`tests/publish_live_test.rs`（`test-live-cloud`） |
| 文档/兼容一致性 Rust 守卫 | 已存在 | `tests/compat/matrix_alignment.rs` |
| 集成计划一致性 Rust 守卫 | 已存在 | `tests/compat/matrix_alignment.rs` |

---

## 2. 本计划范围

### 2.1 纳入范围（本版必须可执行）

1. 单机可执行的 L1/L2 集成回归。
2. 通过 feature/env gate 启用的 L3 live 回归（AI / Cloud）。
3. 明确的波次执行顺序、命令、通过标准、产物归档方式。

### 2.2 暂不纳入（转 `BASELINE_GAP`）

1. 多机编排调度器与远程分发自动化。
2. 自定义 `FA-*` 测试 ID 体系（当前仓库无对应测试命名体系）。
3. `tests/integration/scenarios/*.yml` 驱动器（当前仓库无该目录/运行器）。

### 2.3 Command Coverage Matrix

`tools/integration-runner check-plan` 以本表作为命令覆盖索引：每一行的 `Owner scenarios` 必须只引用 `docs/development/integration/integration-scenarios/integration-scenarios.yaml` 中已经注册的 `cli.*` / `live.*` 场景。修改 Git 兼容命令、命令参数、输出契约或可观测错误时，必须同步更新 YAML、对应场景文档、runner 实现和本表，确保 `check-plan`、`list`、默认本地 wave 以及相关集成测试方案保持一致。

| 覆盖域 | 主要命令/能力 | Wave | 当前状态 | Owner scenarios |
|---|---|---:|---|---|
| Config 基础读写 | `config set/get/list/unset`、默认 local scope、JSON envelope、global DB 隔离 | 1 | 已实现 | `cli.config-basic-kv`, `cli.config-scopes`, `cli.config-unset-compat-flags` |
| Config 角色诊断与确认式恢复 | `config doctor --global-schema` 默认只读；`--repair --confirm` 的格式 attestation、私有 Unix 路径、SQLite 一致性备份、WAL/并发拒绝、事务回滚和旧 reader 拒写 | 1 / 1F | 已实现（Cargo-only，隔离 fixture；非 runner scenario） | Cargo: `compat_global_config_schema_future`（完整具名 target 包含 doctor 与 `global_schema_repair_`）；`db_migration_test`（`global_schema_repair_`）；故障门加 `--features test-upgrade` ， `old_reader_oracle_test` 另需 hash-pinned binary + `LIBRA_ENABLE_OLD_READER_ORACLE=1` + `--ignored`；具名命令与固定 hash 见 `plan-20260910.md` MIG-05 / MIG-06 |
| Config 输入、密钥与兼容入口 | `--stdin`、`--encrypt`、`--plaintext`、`--import`、`path`、`edit`、Git 兼容隐藏 flag、SSH/GPG key 生成 | 1 | 已实现 | `cli.config-set-input-and-encryption`, `cli.config-get-default-and-patterns`, `cli.config-list-variants`, `cli.config-import-path-edit`, `cli.config-key-generation`, `cli.config-git-compat-mode` |
| Init 参数矩阵 | 普通仓库、目标目录、quiet、初始分支、object/ref format、bare/shared、template、from-git、vault | 1 | 已实现 | `cli.init-basic`, `cli.init-directory-and-quiet`, `cli.init-branch-and-format-options`, `cli.init-bare-and-shared`, `cli.init-template`, `cli.init-from-git-repository`, `cli.init-vault` |
| Core 写入闭环 | `status`、`add`、`commit`、`log`、message source、amend、dry-run、porcelain v2、rename/typechange；commit hook 顺序/消息修改/caller-env 隔离/逃逸阀/沙箱边界 | 1 | 已实现 | Runner: `cli.commit-status-log`; Cargo: `compat_libra_hooks_lifecycle` |
| log 消息筛选 | `log --grep` 仅查询subject/body/trailer，排除committer后首个gpgsig/gpgsig-sha256的PGP/SSH签名头且保留真实消息前导空白；大小写/invert与human/JSON/machine一致 | 1 | Cargo-only，隔离真实CLI fixture；非runner场景新增覆盖 | Cargo: `command_test::command::log_test::log_grep_signed_commit_uses_message_only`；lib: `command::log::tests::log_grep_ignores_embedded_signature_headers` |
| 分支与工作区切换 | `branch`、`switch`、`checkout`、detach、path checkout、远端分支可见性、`switch --guess`/`--no-guess` DWIM、worktree-scoped `switch -`/`checkout -` 分支与 detached previous-target 切换及 fail-closed 缺失/删除来源、未支持 switch flag 的负向路径、符号引用行为；`post-checkout` argv/show-current/already-on/逃逸阀 | 1 | 已实现 | Runner: `cli.branch-switch-checkout`; Cargo: `compat_previous_branch_shortcut`, `compat_libra_hooks_lifecycle` |
| 工作区恢复与差异 | `diff`（含 P1-08a raw/compact/filter/full-index/prefix review metadata、P1-08b `-S`/`-G` pickaxe，以及 P1-08c bare/regex-valued `--color-words`、`--word-diff-regex`、Myers/MyersMinimal/Patience/Histogram/Anchored 与算法简写）、`restore`（含真实 `--overlay`/`--no-overlay` 切换）、`reset` 的五种模式（P1-07c `--merge`/`--keep` 的精确保留/拒绝/回滚矩阵在 Cargo compat），以及 restore/reset `--pathspec-from-file` 缺失文件、无效 algorithm/`-G` regex 和无效 word regex 的负向路径 | 1 | 已实现 | Runner: `cli.restore-reset-diff`; Cargo: `command_test::test_diff_algorithms`, `command_test::test_diff_word_diff_modes`, `compat_diff_review_options`, `ai_libra_vcs_safety_test`, `compat_noninteractive_history_controls` |
| 工作流命令 | `stash`、`bisect`、`worktree` 当前参数面及未支持 Git 参数的负向路径 | 1 | 已实现 | `cli.stash-bisect-worktree` |
| 历史与引用检查 | `tag`、`notes`、`reflog`、`symbolic-ref`、`grep`、`blame`、`describe`、`shortlog` | 1 | 已实现 | `cli.tag-basic`, `cli.notes-smoke`, `cli.reflog-symbolic-ref`, `cli.grep-blame-describe-shortlog` |
| 离线迁移与备份 | `fast-export` multi-ref/range/tag/notes/quoting、`fast-import` inline/C/R/N/tag 与原子 ref/note 发布、`bundle create --all/--branches/--tags`/checksum/`unbundle`；Libra↔Libra、真实 Git 双向与 SHA-256 | 1 | 已实现（P1-11 独立 Cargo 黑盒 target；真实 Git 缺失时仅 interop 子段 gate skip） | Cargo: `compat_import_export_roundtrip` |
| 历史编辑 | `merge`（含 P1-07b `-s ours`、`-X ours/theirs`、unrelated histories、CLI shortlog）、`rebase`（含 P1-07a autostash/exec/update-refs/fork-point）、cherry-pick/revert（含 P1-07c last-wins hunk-level `-X`、revert cleanup conflict round-trip）、P2-01 plain-mail `am` replay/continue/skip/abort/interruption rollback，以及 conflict/continue/abort 状态；merge message/post 与 rebase blocking/advisory hook lifecycle、pull-rebase pre hook、JSON 隔离及原子性 | 1 | 已实现 | Runner: `cli.merge-rebase-cherry-revert-smoke`, `cli.merge-conflict-continue`, `cli.rebase-conflict-continue`; Cargo: `compat_noninteractive_history_controls`, `compat_libra_hooks_lifecycle`, `compat_mail_am_basic`, `command_test::test_pull_rebase_runs_pre_rebase_before_moving_local_history` |
| 邮件 plumbing | P2-02 `mailinfo <msg> <patch> < mail` 的 bounded decode/body+patch split/output safety；P2-03 `format-patch` 的 `-1`/root/config/thread/MIME/diff/upstream suppression 与 Libra↔Git `am` 双向互操作；P2-04 明确 `send-email` 不暴露、不读凭据、不执行 SMTP，产物交给外部 mailer | 1 | 已实现（Cargo 黑盒/文档 contract targets） | Cargo: `compat_mailinfo_basic`, `compat_mail_am_basic`, `compat_format_patch_mail_roundtrip`, `compat_matrix_alignment::send_email_policy_is_explicit_and_non_sending` |
| 文件级命令与 LFS 本地能力 | `clean`、`rm`、`mv`、`lfs track/untrack/ls-files`、本地 lock 负向路径 | 1 | 已实现 | `cli.clean-rm-mv-lfs-basic` |
| 其他 CLI 外壳能力 | `open`、root `--json/-J`、`--machine`、`--quiet`、颜色/progress/exit-code-on-warning | 1 | 已实现 | `cli.open-smoke`, `cli.cross-cutting-flags` |
| 安装器短别名 | IX-01 默认相对 `lba -> libra`、same-version 缺失修复/幂等、`--no-alias`/`LIBRA_NO_ALIAS=1`、既有 regular/foreign symlink 保护、无 symlink 能力回退 | 1 | 已实现（Cargo 驱动 POSIX 完整 installer smoke） | Cargo: `compat_install_alias` |
| pkt-line 命令错误边界 | fetch/clone/ls-remote/pull 的 marker 协议错误与普通 IO/网络错误归类；真实空 HTTP advertisement 四命令路径和 clone discovery→fetch POST | 1 | Cargo library 场景已实现；执行证据见 plan-20260901.md PKT-10 | Cargo library: `command::ls_remote::pkt_line_boundary_tests`（25 项；非 cli.* runner scenario） |
| push receive-pack 状态报告 | 未识别状态行、畸形/截断帧、状态报告缺flush（含空响应及unpack/ng拒绝）、固定诊断及本地tracking引用保持 | 1 | Cargo library场景；实际验收见plan-20260901.md PKT-09 | Cargo library: `command::push::test` 中4个PKT-09具名门（非cli.* runner scenario） |
| Git/SSH advertisement 读取边界 | 双客户端长度下界/flush/空payload/上限/标头与payload EOF及普通IO/超时分类；真实TCP Git取对象广告与fetch/clone/pull错误转换 | 1 | Cargo library场景；完整命令与SSH清理仍未验证 | Cargo library: `pkt_line_client_` 十个PKT-08具名门（非cli.* runner scenario） |
| Schema 与本地协议 | schema 建链自动升级、local clone/remote/ls-remote/fetch/pull（含 refspec 精确映射、remotes.default、rename namespace、symref、pull-rebase hook/JSON child 隔离）、shallow fetch、拒绝 file remote push | 2 | 已实现 | Runner: `cli.schema-upgrade-observable`, `cli.clone-fetch-pull-local`, `cli.fetch-depth-local`, `cli.push-local-file-remote-rejected`; Cargo: `command_test::test_pull_rebase_runs_pre_rebase_before_moving_local_history` |
| 对象读取与树遍历 | `rev-parse`、`show-ref` / `show-ref --branches` / `show-ref --no-branches` / `show-ref --no-tags` / `show-ref --hash[=<n>]` / `show-ref --no-hash` / `show-ref --abbrev[=<n>]` / `show-ref --no-abbrev` / `show-ref --dereference` / `show-ref --no-dereference` / `show-ref --verify` / `show-ref --no-verify` / `show-ref --exists` / `show-ref --no-exists` / `show-ref --head` / `show-ref --no-head` / `show-ref --exclude-existing[=<pattern>]`、`for-each-ref --points-at`、`cat-file`、`hash-object --stdin` / `--path` / `--no-filters`、`show`、`rev-list` / multi revision / `A..B` / `^A` / `A...B` / `rev-list --count` / `rev-list -n` / `rev-list --skip` / `rev-list --since` / `rev-list --after` / `rev-list --until` / `rev-list --before` / `rev-list --merges` / `rev-list --no-merges` / `rev-list --min-parents` / `rev-list --max-parents` / `rev-list --no-min-parents` / `rev-list --no-max-parents` / `rev-list --first-parent` / `rev-list --author` / `rev-list --committer` / `rev-list --grep` / `rev-list -- <path>` / `rev-list --left-right` / `rev-list --left-only` / `rev-list --right-only` / `rev-list --cherry-pick` / `rev-list --cherry-mark` / `rev-list --cherry` / `rev-list --parents` / `rev-list --children` / `rev-list --timestamp`、`fsck`、sha256 object format；`ls-tree` 默认/递归/子目录/`--full-name`/`--full-tree` 路径场景 | 2 | 已实现 | `cli.object-readback`, `cli.show-ref-exclude-existing`, `cli.ls-tree-smoke`, `cli.sha256-object-readback` |
| 维护命令 | `gc`、`prune`、`archive`（tar/zip、`--prefix`、`--output`、`--list`、`TREEISH <path>...` pathspec）、`verify-pack <idx>...` / `verify-pack --pack` / `verify-pack -v` / `verify-pack -s`、内部 `index-pack --stdin` / `--keep` / `--progress` / `--no-progress` fixture | 2 | 已实现 | `cli.gc-smoke`, `cli.archive-smoke`, `cli.verify-pack-smoke` |
| GitHub live remote | `gh` 创建/清理私有临时 repo、`push` refspec/tag/delete/force/mirror、真实 clone/fetch/pull | 3 | 已实现，需显式 live gate | `live.github-create-push-clone-fetch` |


PKT-10 的 pkt-line 边界场景使用默认 Cargo library 测试：
`cargo test --lib command::ls_remote::pkt_line_boundary_tests`。
模块包含 25 个具名门；其中
`pkt_line_discovery_empty_response_regression_fetch_clone_lsremote_pull`
通过各命令实际 `execute_safe` 路径核对空 advertisement 的 GET、`LBR-NET-002`、
exit 128、提示及 pull 的 fetch phase；
`pkt_line_matrix_parametrized_https_marker_maps_net_002`
核对 clone 两次 discovery GET 和带 wanted OID 的 fetch POST。
本地 HTTP fixture 运行生产 HttpsClient，不宣称覆盖 TLS 握手或证书验证；
git/ssh/https URL carrier 矩阵只验证 mapper。原始 Git/SSH framing 与严格异步
header grammar 属于 PKT-08/13。fixture 使用临时仓库、env/cwd/hash_kind 锁、
有界请求/等待与 server shutdown；无新增 feature gate 或 live remote。
这些库测试不是 integration-runner 的新 cli.* 场景，完整运行及发布证据保存在
`plan-20260901.md` 对应任务卡；文档映射检查为

```bash
cargo test --test compat_matrix_alignment
```

PKT-09 的状态报告回归使用既有 Cargo library 入口：

```bash
cargo test --lib pkt_line_push_
cargo test --lib validate_receive_pack_response
```

其中4个新增具名门登记于 plan-20260901.md PKT-09；真实push fixture经本地HTTP
完成receive-pack discovery和delete-only POST，核对畸形/截断响应返回LBR-NET-002
及本地tracking ref不变。测试不宣称TLS、SSH或真实远端回滚覆盖；YAML notes同步
Cargo-only范围，没有新增cli.* runner场景。

**剩余覆盖缺口**：默认本地 wave 已覆盖当前 runner 注册的 `cli.*` 场景；需要真实 GitHub 远端的 `live.*` 场景不进入默认阻断门，只能在具备 `gh` 登录态和仓库创建/删除权限时运行。新增或修改 Git 兼容命令时，必须把对应场景加入本表、YAML、场景文档和 runner registry；如果当前 runner 尚未实现，必须在 YAML 和文档中保留明确的未实现状态，而不能只在本表声明覆盖。

---

## 3. 执行前准备（Wave 0）

### 3.1 必须通过

```bash
cargo --version
rustup show active-toolchain

# 格式 + lint（CI 阻断门，本地先过；少跑一遍能省 30 分钟返工）
cargo +nightly fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings

# 编译基线（两条都必须 pass，feature 代码路径才被覆盖）
cargo test --no-run --all-targets
cargo test --no-run --all-targets --features test-provider,test-network,test-live-ai,test-live-cloud

# 文档/兼容性一致性
cargo test --test compat_matrix_alignment -- --test-threads=1
```

### 3.2 Live 测试前置（可选）

```bash
cp .env.test.example .env.test
chmod 600 .env.test
```

加载方式（所有 live wave 共用）：

```bash
# 推荐：仅注入 cargo 子进程，不污染当前 shell
env $(grep -v '^#' .env.test | xargs) cargo test ...

# 备用（注意：会把密钥导入当前 shell，跑完用 `unset` 清掉）：
set -a; source .env.test; set +a
```

### 3.3 Path → Wave 映射

修改某路径后**最小**应跑的 wave 集合（Wave 0 始终默认，下表省略）。

| 修改路径 | 必跑 Wave | 推荐补充 |
|---|---|---|
| `src/command/*.rs`（非 cloud / code） | 1 | + 2，若涉及 `mod.rs` 共享 helper |
| `src/command/cloud.rs` | 1 | + 5（需 D1/R2 env） |
| `src/command/code.rs`、`src/command/code_control*.rs` | 1, 2 | + 4，若动到 provider 接线 |
| `src/internal/ai/agent/**`、`src/internal/ai/orchestrator/**` | 2 | + 4 |
| `src/internal/ai/providers/**` | 2 | + 4 |
| `src/internal/ai/tools/**` | 2 | — |
| `src/internal/ai/intentspec/**`、`workflow_objects.rs` | 2 | + 4 |
| `src/internal/ai/mcp/**` | 2 | — |
| `src/internal/protocol/**`、`src/git_protocol.rs` | 1 | + 3 |
| `src/utils/client_storage.rs`、`src/utils/d1_client.rs` | 1 | + 5 |
| `src/internal/model/**`、`sql/*.sql` | 1, 2 | — |
| `Cargo.toml`、`.env.test.example` | 0, 1, 2 | 强制跑 `compat_matrix_alignment` |
| `docs/**`、`README.md` | 仅 Wave 0（docs 一致性） | — |
| `tests/**` | 仅相关 wave | 若动 `tests/harness/**`，跑全 Wave 2 |

**用法**：

```bash
git diff --name-only origin/main...HEAD | \
  xargs -n1 -I{} echo "{}" | \
  awk '...'   # 后续可由 integration runner 的 pick-waves 子命令自动化（见 BASELINE_GAP-INTEG-006）
```

在该脚本落地前，作者手动对照本表，PR 描述里在 `## Test Plan` 段列出实际跑的 wave 集合（见 §9.1）。

---

## 4. 执行波次

## 4.1 Wave 1：命令与兼容性（必跑）

```bash
cargo test --test command_test -- --test-threads=1

cargo test --test compat_stash_subcommand_surface -- --test-threads=1
cargo test --test compat_bisect_subcommand_surface -- --test-threads=1
cargo test --test compat_worktree_delete_dir -- --test-threads=1
cargo test --test compat_checkout_alias_help -- --test-threads=1
cargo test --test compat_matrix_alignment -- --test-threads=1
cargo test --test compat_branch_lossy_wrapper_guard -- --test-threads=1
```

通过标准：全部 green，无 skip/fail。

## 4.2 Wave 2：Code UI 与本地自动化控制（必跑）

```bash
# test-provider 矩阵与场景
cargo test --features test-provider \
  --test harness_self_test \
  --test code_ui_scenarios \
  --test code_ui_remote_lease_matrix \
  --test code_ui_remote_sse_matrix \
  --test code_ui_remote_state_matrix \
  --test code_ui_remote_security_matrix \
  --test code_ui_remote_generation_matrix \
  --test code_ui_remote_approval_matrix \
  -- --test-threads=1

# Code 路径专项
cargo test --features test-provider \
  --test code_cli_dispatch_test \
  --test code_provider_boot_test \
  --test code_tool_acl_test \
  --test code_mcp_dual_entry_test \
  --test code_resume_test \
  --test code_codex_default_web_test \
  --test ai_code_ui_headless_test \
  --test code_codex_runtime_test \
  -- --test-threads=1
```

通过标准：全部 green。

## 4.3 Wave 3：网络层（可选，建议 nightly）

```bash
cargo test --features test-network --test network_remotes_test -- --test-threads=1
```

通过标准：green。若外网抖动，允许重试一次后再判定。

## 4.4 Wave 4：Live AI（可选，按成本启用）

> **成本警告**：本 wave 调用真实 LLM。开跑前必须设置成本闸门 env（见 §9.3）。
> 闸门 env 当前**未实现**自动 fail-fast，靠人工监控；详见 `BASELINE_GAP-INTEG-005`。

```bash
# DeepSeek live（ai_agent_test / ai_chat_agent_test 内部按 DEEPSEEK_API_KEY gate）
cargo test --test ai_agent_test -- --test-threads=1
cargo test --test ai_chat_agent_test -- --test-threads=1

# Code UI live model generation（ignored + 明确开关）
LIBRA_RUN_LIVE=1 cargo test --features test-provider \
  --test code_ui_remote_model_generation_matrix \
  -- --ignored --test-threads=1
```

通过标准：
1. 若未配置 key，测试允许输出 skipped。
2. 若已配置 key，失败视为真实回归。
3. 单次 wave LLM 调用总数超过 `LIBRA_TEST_MAX_CALLS`（建议 200）时，必须中断，记录 issue 而非继续重试。

## 4.5 Wave 5：Live Cloud（可选，按环境启用）

```bash
# D1/R2 live gate（依赖 LIBRA_D1_* + LIBRA_STORAGE_*）
cargo test --features test-live-cloud --test cloud_storage_backup_test -- --test-threads=1

# publish live gate（额外要求 LIBRA_ENABLE_TEST_LIVE_CLOUD=1）
LIBRA_ENABLE_TEST_LIVE_CLOUD=1 cargo test --features test-live-cloud \
  --test publish_live_test publish_live -- --test-threads=1
```

通过标准：
1. 缺少 live 环境变量时允许 skipped。
2. 环境齐全后失败视为真实回归。

## 4.6 Wave 6：性能 smoke（可选）

```bash
LIBRA_RUN_PERF=1 cargo test --features test-provider \
  --test code_ui_perf_smoke_test \
  -- --ignored --test-threads=1
```

通过标准：全部 green；用于趋势观察，不作为默认阻断门。

---

## 5. 归档与报告（落地可执行）

推荐每轮执行都产生日志目录：

```bash
RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_DIR="target/integration-runs/$RUN_ID"
mkdir -p "$RUN_DIR"
```

执行命令时统一保留日志：

```bash
cargo test --test command_test -- --test-threads=1 2>&1 | tee "$RUN_DIR/wave1-command.log"
```

最小交付物：

1. `wave1-command.log`
2. `wave2-code-ui.log`
3. `wave3-network.log`（若执行）
4. `wave4-live-ai.log`（若执行）
5. `wave5-live-cloud.log`（若执行）
6. `summary.md`（人工汇总 pass/fail/skip 与失败链接）

---

## 6. 出口标准（Definition of Done）

### 6.1 本计划“可落地执行”判定

满足以下全部条件即可判定本计划可执行：

1. Wave 0 命令全部在当前仓库可运行。
2. Wave 1 / Wave 2 的命令只引用真实存在的测试目标并可执行。
3. Wave 3 / 4 / 5 明确采用 feature/env gate，不再依赖虚构脚本或虚构命令。
4. 文档内不存在仓库未实现的必跑命令。

### 6.2 作为发布阻断的最低门

默认阻断门仅包含：

1. Wave 1 全绿。
2. Wave 2 全绿。

Wave 3/4/5 作为 nightly 或 release 前增强门。

---

## 7. 集成测试机器配置文件设计（用于多机编排）

### 7.1 设计目标（GAP-INTEG-001 的实施前置）

1. 用单一配置文件描述测试节点的能力、资源与执行边界，统一 `Wave -> Node` 分配。
2. 把 `env-file`、`features`、`provider` 与日志产物绑定，支持 `Wave 4 / 5` 的差异化路由。
3. 统一产物 schema，便于多节点结果汇总与回放。

### 7.2 推荐文件与示例（建议路径：`tools/integration-runner/config/nodes.yaml`）

```yaml
version: 1
runbook:
  run_id_hint: "YYYYMMDDTHHMMSSZ"
  artifact_root: "target/integration-runs/{run_id}"
  default_env_file: ".env.test"
  default_wave_timeout_sec: 1800
  default_test_threads: 1
  log_retention_days: 14

nodes:
  - id: "local-01"
    role: "controller"
    transport:
      mode: "local"
      shell: "bash -lc"
      workdir: "/data/ci/libra"
      concurrency: 1
    features: ["test-provider", "test-network", "test-live-ai", "test-live-cloud"]
    waves: [1, 2, 3]
    providers:
      - provider_id: "deepseek"
        models: ["deepseek-v4-flash"]
        required_env: ["DEEPSEEK_API_KEY"]
      - provider_id: "kimi"
        models: ["moonshot-v1-32k"]
        required_env: ["MOONSHOT_API_KEY"]
        optional_env: ["MOONSHOT_BASE_URL"]
    gate_env:
      LIBRA_CODE_TEST_PROVIDER: "deepseek"
      LIBRA_CODE_TEST_MODEL: "deepseek-v4-flash"
      LIBRA_ENABLE_TEST_PROVIDER: "1"

  - id: "live-ai-01"
    role: "live-ai-runner"
    transport:
      mode: "ssh"
      host: "10.0.0.22"
      user: "ci"
      workdir: "/home/ci/libra"
      ssh_key_ref: "CI_AGENT_KEY"
      concurrency: 1
    features: ["test-provider", "test-live-ai"]
    waves: [4]
    providers:
      - provider_id: "deepseek"
        models: ["deepseek-v4-flash"]
        required_env: ["DEEPSEEK_API_KEY"]
      - provider_id: "kimi"
        models: ["moonshot-v1-32k"]
        required_env: ["MOONSHOT_API_KEY"]
        optional_env: ["MOONSHOT_BASE_URL"]
    gate_env:
      LIBRA_RUN_LIVE: "1"
      LIBRA_ENABLE_TEST_PROVIDER: "1"

  - id: "cloud-01"
    role: "live-cloud-runner"
    transport:
      mode: "ssh"
      host: "10.0.0.23"
      user: "ci"
      workdir: "/home/ci/libra"
      ssh_key_ref: "CI_AGENT_KEY"
      concurrency: 1
    features: ["test-live-cloud"]
    waves: [5]
    required_env:
      - "LIBRA_D1_ACCOUNT_ID"
      - "LIBRA_D1_API_TOKEN"
      - "LIBRA_D1_DATABASE_ID"
      - "LIBRA_STORAGE_ENDPOINT"
      - "LIBRA_STORAGE_BUCKET"
      - "LIBRA_STORAGE_ACCESS_KEY"
      - "LIBRA_STORAGE_SECRET_KEY"

runtime:
  env_file:
    source: ".env.test"
    target: ".env.test"
    target_permission: "0600"
    redacted_reporting: true
  commands:
    preflight: "cargo test --test compat_matrix_alignment -- --test-threads=1"
    run_wave_tpl: "cargo run --manifest-path tools/integration-runner/Cargo.toml -- run --waves {wave}"
  reporting:
    format: "jsonl"
    summary_path: "target/integration-runs/{run_id}/nodes.jsonl"
```

### 7.3 约束与验收项

1. `nodes[*].id` 唯一。
2. `mode: local` 仅允许 `workdir/shell/concurrency`；
   `mode: ssh` 必须显式包含 `host/user/workdir`。
3. `waves` 与 `features` 要一致：`wave:4` 需具备 `test-live-ai`，`wave:5` 需具备 `test-live-cloud`。
4. `providers[].required_env` 缺失或为空值表示该 provider 在该节点上不可执行；对应波次必须禁止调度。
5. `.env.test` 下发后必须验证目标文件权限为 `0600`；日志不能输出其原文内容。
6. 每个 `(node, wave)` 输出一条 `jsonl` 结果，字段至少包含：
   - `run_id`、`node`、`wave`、`status`、`return_code`、`duration_ms`、`log_path`、`first_error_line`

### 7.4 示例汇总行

```json
{"run_id":"20260516T0001Z","node":"live-ai-01","wave":4,"status":"passed","return_code":0,"duration_ms":382000,"log_path":"target/integration-runs/20260516T0001Z/wave4/live-ai-01.log","first_error_line":null}
{"run_id":"20260516T0001Z","node":"cloud-01","wave":5,"status":"failed","return_code":101,"duration_ms":795000,"log_path":"target/integration-runs/20260516T0001Z/wave5/cloud-01.log","first_error_line":"E0426: D1 bind failed with 403"}
```

### 7.5 与现有计划关系

- 本节是 `BASELINE_GAP-INTEG-001` 的控制面先行设计；不改变当前 Wave 1~6 现状执行方式。
- 落地 GAP 后，上述 `node.waves` 作为执行映射输入，Wave 命令应由统一 runner 生成，减少手工拼接。

## 8. BASELINE_GAP（后续扩展项）

以下能力不再伪装成“已可执行”，统一登记为后续工程任务。

### BASELINE_GAP-INTEG-001：多机调度器缺失

- 现状：辅助脚本目录已移除；`tools/integration-runner` 已覆盖本地 runner、plan 检查和已实现场景，但还没有远程节点调度器。
- 需要补充：
  1. 远程节点 preflight 子命令
  2. 远程节点 run-wave 子命令
  3. 远程节点 report 子命令

### BASELINE_GAP-INTEG-002：场景 YAML 驱动缺失

- 现状：`tests/integration/scenarios/*.yml` 与对应 runner 不存在。
- 需要补充：
  1. `tests/integration/harness/run.py`（或 Rust 等价实现）
  2. `tests/integration/scenarios/` 场景文件与断言 DSL

### BASELINE_GAP-INTEG-003：自定义 `FA-*` 测试 ID 体系未落地

- 现状：仓库没有 `FA-*` 对应测试目标。
- 决策：当前以真实 `cargo test --test <name>` 为唯一执行单位；后续如引入 `FA-*`，必须同步落地到实际 runner 与报告工具。

### BASELINE_GAP-INTEG-004：四节点并发预算编排未落地

- 现状：无 semaphore 调度器、无远程节点资源协调实现。
- 需要补充：
  1. 明确调度器实现与超时策略
  2. 节点健康检查与配额采集

### BASELINE_GAP-INTEG-005：Live test 成本闸门未实现

- 现状：`LIBRA_TEST_MAX_CALLS` / `LIBRA_TEST_BUDGET_USD` 仅写在文档里，测试代码未读这两个 env，超额不会 fail-fast。
- 需要补充：
  1. 在 `tests/harness/` 增加 budget counter，被所有 Live provider 调用前置 hook 拦截。
  2. 超限时输出可定位的 panic 信息，包含已用 / 上限 / 当前测试名。
- 优先级：P0（防止 AI agent 自动 retry 烧钱）。

### BASELINE_GAP-INTEG-006：自动 pick-waves 未实现

- 现状：§3.3 Path → Wave 映射目前靠人工对照。
- 需要补充：
  1. 输入 `git diff --name-only`，输出推荐 wave 集合。
  2. 与 `compat_matrix_alignment` 共享同一份映射表（建议落到 `tools/integration-runner/config/path-wave-map.toml`）。
- 优先级：P1。

### BASELINE_GAP-INTEG-007：`tests/INDEX.md` TODO 行收尾

- 现状：`tests/INDEX.md` 已建骨架，Wave 1~6 主要测试已索引；剩余 ~50 个测试目标标记为 TODO，待逐条补一句话功能描述与 `src` 关联。
- 需要补充：每条 TODO 替换为 `target | wave | one-line purpose | relevant src`，并移到对应 Wave 表。
- 优先级：P1（分散到日常 PR 中渐进收尾即可，单 PR 一次性补全反而难审）。

---

## 9. PR / Review 协议（AI 交叉开发约束）

本项目由 Claude + Codex 交叉开发与 review。以下约束必须遵守，避免双方互改导致死锁、flake 嘴炮、密钥泄露。

### 9.1 PR 描述必须包含 `## Test Plan` 段

```
## Test Plan
- New:      <target>::<fn>  // e.g. code_ui_remote_lease_matrix::lease_expires_after_ttl
- Modified: <target>::<fn>
- Deleted:  <target>::<fn>
- Waves run locally: 0, 1, 2 (+ 4 if applicable)
- jsonl summary: <inline ≤ 50 KB, or GitHub artifact URL>
- Commit SHA at run time: <sha>
```

**测试引用统一用三段式** `<cargo --test 名>::<test_fn 名>`。
文件路径仅作辅助；rename/refactor 不影响三段式 ID。

### 9.2 Reviewer 行为约束

1. **不要直接代笔测试**：若覆盖不足，留 review comment 提议补，由原作者补。这避免两个 agent 互相覆盖对方写的测试。
2. **复跑义务**：reviewer 在本地至少跑 Wave 1+2 一次，把 jsonl 摘要贴到 review。
3. **复现要求**：若 reviewer 报告失败，必须附 (`commit_sha`, `wave`, 完整 cargo invocation, log head 50 行 + tail 50 行)。缺字段的失败报告无效。
4. **flake 处理**：怀疑 flake 先查 `tests/flaky_quarantine.toml`（§9.5）。不在 quarantine 中的测试连挂 2 次才能开 flaky issue。

### 9.3 Live test 成本闸门

跑 Wave 4 / Wave 5 前**必须**设置：

```bash
export LIBRA_TEST_MAX_CALLS=200      # 单次 wave LLM 调用总数硬上限
export LIBRA_TEST_BUDGET_USD=2.0     # 软上限：仅 warn，不 fail
```

当前这两个 env **未被测试代码读取**（见 `BASELINE_GAP-INTEG-005`）。在 GAP 落地前，作者必须人工监控用量，超 200 次或耗时 > 10 min 立即中止。

### 9.4 PR 规模

- 单 PR 改动 src + tests 合计建议 ≤ 800 行（不计自动生成）。
- 超过上限：拆分；或在 PR 描述里说明无法拆分的原因，并请求第三个 agent（另一个模型）旁路 review。

### 9.5 Flake 隔离清单

新增/维护：`tests/flaky_quarantine.toml`

```toml
# 已知 flaky 但不阻断 wave 的测试。
[[entries]]
test    = "<target>::<fn>"
reason  = "<一句话>"
issue   = "<URL>"
last_seen_commit = "<sha>"
quarantined_at   = "<YYYY-MM-DD>"
```

- 每次把测试加入 quarantine，必须同时开 issue 跟踪。
- 修复后必须从 quarantine 移除并在 PR 描述说明；不允许"修了但忘记移除"。
- quarantine 文件由 `compat_matrix_alignment` 校验：每条 `test` 必须能定位到对应 `#[test]` 函数。

### 9.6 本计划自检命令

`cargo test --test compat_matrix_alignment integration_test_plan_references_existing_targets_and_features -- --exact` 执行以下校验：

1. 计划里所有 `--test <name>` 必须对应 `tests/<name>.rs` 或 `Cargo.toml [[test]]` 条目。
2. 计划里所有 `--features <flag>` 必须出现在 `Cargo.toml [features]`。
3. 计划里所有 `LIBRA_*` / `*_API_KEY` / `*_BASE_URL` env 名必须出现在 `.env.test.example`（`LIBRA_TEST_*` 由本计划自身引入，豁免）。
4. `flaky_quarantine.toml` 里每条 `test` 三段式必须可解析为现有测试。

CI 在 Wave 0 调用此脚本，失败即阻断 PR。

---

## 10. 维护规则

1. 新增集成测试文件时，必须把执行命令补到本计划相应 Wave，并在 `tests/INDEX.md` 加一行索引。
2. 删除/重命名测试目标时，必须同步更新本计划命令、`tests/INDEX.md`、以及 `flaky_quarantine.toml` 中对应条目。
3. 未实现能力必须用 `BASELINE_GAP-*` 标记，不允许写成默认可执行步骤。
4. 若引入新的 live gate 环境变量，必须同步更新 `.env.test.example`、本计划 Wave 说明、`compat_matrix_alignment` 的 env 规则（如需）。
5. 修改 §3.3 Path → Wave 映射，须同步更新 `tools/integration-runner/config/path-wave-map.toml`（如已落地）。

### PKT-12 SSH advertisement error gates (Cargo-only)

The eleven `pkt_line_client_` PKT-12 library gates cover shared inner/outer error
formatting, native non-zero SSH exits with fixed status/guidance, successful/failed
output collection, preservation of collected output when cleanup fails, and real
ordinary-error child reaping. PKT-11 captures stderr for terminal and
non-terminal callers, so the real-command gate no longer requires non-terminal
stdin. The original PKT-12 prerequisite remains historical run evidence only.
Unix child gates include fifty malformed-frame command cases and five native
exit-255/empty-advertisement cases across ls-remote/fetch/clone/pull/push. The
existing `transport_timeout_uses_push_idle_timeout` gate covers push classifier
priority and ordinary errors. These are Cargo-only cases, not new cli.* runner
scenarios. Actual run evidence belongs in plan-20260901.md; Unix process tests do
not claim Windows execution coverage.

### PKT-12 existing SSH host-key CLI cases

`command_test::command::fetch_test::test_fetch_ssh_host_key_failure_is_reported`
and `command_test::command::push_test::test_push_ssh_host_key_failure_is_reported`
retain their names and serial lanes. Each runs the real CLI in human, JSON and
machine modes. Under PKT-11 these cases pin exit 128 / `LBR-NET-001`, fixed
host-verification guidance, and no raw host-key stderr in either output stream.
Their complete network-hint vectors differ: fetch checks network connectivity;
push checks the remote URL and network connectivity. Push also verifies no remote
ref was created. The earlier PKT-12 NET002/incomplete-header/SSH255 and protocol
hint expectations, including the rejected wrong-hint assertion and corrected
rerun, remain historical evidence in plan-20260901.md. These are local fake-SSH
CLI tests, with current source and execution acceptance recorded separately.

## SSH capture validation scope

The twelve named PKT-11 library gates retain the original plan names. They cover
fixed diagnostics and metadata-only tracing, both service argument lists, three
non-zero-exit paths, malformed-advertisement cleanup and all six capture paths
under stderr floods. The flood gate also covers retained-prefix/full-stream
digest accounting, collector cancellation, and oversized advertisement and push
response rejection through actual clone/delete-only push commands, NET001, distinct
network hints, decoded JSON and unchanged local tracking refs. The host-trust gate
uses a current-thread runtime without a yield after setup; timeout precedence is
also checked immediately after config writes. Both exercise asynchronous transport
configuration without a blocking nested-runtime join. The host-trust gate covers native exit 255, typed primary
error preservation through a secondary cleanup warning, the internal discovery
carrier and an actual local fake-SSH clone command. Existing fetch/push CLI cases
retain their names and test human, JSON and machine output plus remote-ref safety.
The terminal gate uses a real local PTY. The passphrase gate creates an encrypted
local key without an agent and exercises a simulated SSH failure; it does not
claim live OpenSSH network authentication. Actual run IDs and results belong in
plan-20260901.md after execution; the existence of these tests is not acceptance.

### SSH host identity and diagnostic collection

SSH host identity changes retain a distinct fixed warning: the change may
indicate interception or legitimate key rotation. Verify the new fingerprint
through a trusted channel before replacing an existing known_hosts entry; do not
bypass host-key checking. Unknown and changed host keys both use LBR-NET-001,
but their fixed messages and guidance differ.

A stderr collection timeout does not by itself discard complete protocol output
and an observed local exit status. Non-zero exit status and primary read errors
still fail the operation. Unavailable diagnostics produce only a fixed debug
notice, without fabricated empty-stream counts or digests. Stdout collection or
process-wait failures retain their normal error handling.

### SSH limits and host-classification boundaries

These fixed 16 MiB advertisement and receive-pack response limits apply only to
Libra's SSH transport. The HTTPS and Git transports do not impose this particular
cap. If the server provides an HTTPS endpoint, use its HTTPS remote URL when an
SSH advertisement exceeds the cap; this does not require a read-only user to
change the server's refs. Otherwise, ask the repository maintainer to reduce the
advertised ref set. The streamed fetch pack remains outside this aggregate cap.

Host-trust classification requires an incomplete first header with no stdout
bytes observed, local exit 255 and a recognized retained stderr pattern. Once
any stdout byte arrives, including a partial header, host-like stderr cannot
select host-specific guidance. Failures after a complete advertisement retain
fixed generic diagnostics. The pre-advertisement pattern remains a diagnostic
heuristic, not fingerprint verification.

A successful discovery whose child waits for a request normally incurs the full
100 ms native-exit observation window, once per discovery operation. This is
separate from the two-second direct-child cleanup budget; no benchmark or
arbitrary-descendant cleanup guarantee is implied.

## Task shell Rustup regression (FIX-PKT-04)

The default lib suite covers `internal::ai::sandbox::runtime::tests::{task_shell_preserves_default_rustup_home,task_shell_preserves_explicit_rustup_environment,task_shell_rustup_home_derivation_is_conservative}` on Unix. They exercise original-home derivation, explicit precedence, missing/non-UTF-8 roots, non-task directories and continued task-local HOME/Cargo/XDG/log paths. The existing `internal::ai::orchestrator::executor::tests::execute_dag_syncs_cargo_project_without_treating_lockfile_or_target_as_scope_creep` runs the real Cargo fixture in a controlled subprocess without inherited RUSTUP_HOME/CARGO, preserves explicit toolchain selection, disables automatic installation with `RUSTUP_AUTO_INSTALL=0` on rustup 1.28+, and retains the lockfile/target/scope assertions. This exposes the installed-product environment even under a Cargo-launched suite. These are inline Cargo cases, not new integration-runner scenario IDs or proof of bwrap/macOS/Windows execution. Run the related sandbox/orchestrator/shell modules and compatibility guards, then the full default suite for the shared runtime change.
