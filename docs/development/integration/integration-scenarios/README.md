# 黑盒 CLI 集成测试场景（按场景拆分）

每个 `cli.*` / `live.*` 场景一份可执行文档，与 [`integration-scenarios.yaml`](integration-scenarios.yaml) 和 `tools/integration-runner/src/scenarios/<id>.rs` 一一对应。
计划总则、§3.3.1 隔离模板、§2.3 覆盖矩阵、PR 协议见 [`integration-test-plan.md`](../integration-test-plan.md)。

修改某个 Git 兼容命令时，优先编辑**本目录下对应该命令组的场景文件** + yaml 元数据 + runner 实现。下方「命令 → 场景映射」给出每个命令到其 owner 场景的直接索引，改命令时据此定位需要同步更新的集成测试文件。

## 命令 → 场景映射（Command → Scenario Map）

> **维护规则**：改动 `src/cli.rs` / `src/command/<cmd>.rs` 的任一 Git 兼容命令时，必须同步更新本表对应行的 **owner 场景**（`<id>.md` + `integration-scenarios.yaml` + `tools/integration-runner/src/scenarios/<file>`），并跑 `check-plan` + `run --only <owner-id>`。新增命令时：新增一行 + 至少一个 `cli.<cmd>-smoke` 场景 + §2.3 矩阵行。runner 文件名 = 场景 id 去掉 `cli.`/`live.` 前缀后把 `-` 换成 `_` 再加 `.rs`（如 `cli.commit-status-log` → `commit_status_log.rs`）。
>
> 本表是 `tools/integration-runner/src/scenarios/` 拆分（每场景一文件）的导航面：每个命令都能一步定位到要改的集成测试代码。

| 命令组 | 命令 | owner 场景（改命令时优先更新） |
|--------|------|--------------------------------|
| Setup | `init` | `cli.init-basic`（+ `init-directory-and-quiet` / `init-branch-and-format-options` / `init-bare-and-shared` / `init-template` / `init-vault` / `init-from-git-repository`） |
| Setup | `clone` | `cli.clone-fetch-pull-local`、`cli.fetch-depth-local` |
| Setup | `config` | `cli.config-basic-kv`（+ `config-scopes` / `config-set-input-and-encryption` / `config-get-default-and-patterns` / `config-list-variants` / `config-unset-compat-flags` / `config-import-path-edit` / `config-key-generation` / `config-git-compat-mode`） |
| Working Tree | `status` | `cli.commit-status-log` |
| Working Tree | `add` | `cli.commit-status-log` |
| Working Tree | `rm` | `cli.clean-rm-mv-lfs-basic` |
| Working Tree | `mv` | `cli.clean-rm-mv-lfs-basic` |
| Working Tree | `restore` | `cli.restore-reset-diff` |
| Working Tree | `clean` | `cli.clean-rm-mv-lfs-basic` |
| Working Tree | `stash` | `cli.stash-bisect-worktree` |
| Working Tree | `lfs` | `cli.clean-rm-mv-lfs-basic` |
| Working Tree | `worktree` | `cli.stash-bisect-worktree` |
| History | `log` | `cli.commit-status-log` |
| History | `shortlog` | `cli.grep-blame-describe-shortlog` |
| History | `show` | `cli.object-readback` |
| History | `show-ref` | `cli.object-readback`、`cli.show-ref-exclude-existing`、`cli.clone-fetch-pull-local` |
| History | `for-each-ref` | 已注册命令；暂未纳入专属正向 runner 场景 |
| History | `ls-files` | 已注册命令；暂未纳入专属正向 runner 场景 |
| History | `ls-remote` | `cli.clone-fetch-pull-local` |
| History | `ls-tree` | `cli.ls-tree-smoke` |
| History | `diff` | `cli.restore-reset-diff` |
| History | `grep` | `cli.grep-blame-describe-shortlog` |
| History | `blame` | `cli.grep-blame-describe-shortlog` |
| History | `describe` | `cli.grep-blame-describe-shortlog` |
| History | `notes` | `cli.notes-smoke` |
| Branching | `commit` | `cli.commit-status-log`（仓库 hook lifecycle：Cargo `compat_libra_hooks_lifecycle`） |
| Branching | `branch` | `cli.branch-switch-checkout` |
| Branching | `switch` | `cli.branch-switch-checkout`（`post-checkout`：Cargo `compat_libra_hooks_lifecycle`） |
| Branching | `checkout` | `cli.branch-switch-checkout`（`post-checkout`：Cargo `compat_libra_hooks_lifecycle`） |
| Branching | `tag` | `cli.tag-basic` |
| Branching | `merge` | `cli.merge-rebase-cherry-revert-smoke`、`cli.merge-conflict-continue`（hooks：Cargo `compat_libra_hooks_lifecycle`） |
| Branching | `rebase` | `cli.merge-rebase-cherry-revert-smoke`、`cli.rebase-conflict-continue`（hooks：Cargo `compat_libra_hooks_lifecycle`） |
| Branching | `reset` | `cli.restore-reset-diff` |
| Branching | `cherry-pick` | `cli.merge-rebase-cherry-revert-smoke` |
| Branching | `revert` | `cli.merge-rebase-cherry-revert-smoke` |
| Remote | `remote` | `cli.clone-fetch-pull-local`、`cli.push-local-file-remote-rejected` |
| Remote | `fetch` | `cli.clone-fetch-pull-local`、`cli.fetch-depth-local` |
| Remote | `pull` | `cli.clone-fetch-pull-local`（`pull --rebase` hooks/JSON 隔离：`command_test::test_pull_rebase_runs_pre_rebase_before_moving_local_history`） |
| Remote | `push` | `cli.push-local-file-remote-rejected`、`live.github-create-push-clone-fetch` |
| Remote | `open` | `cli.open-smoke` |
| Maintenance | `db` | `cli.schema-upgrade-observable` |
| Maintenance | `gc` | `cli.gc-smoke` |
| Maintenance | `prune` | `cli.gc-smoke` |
| Maintenance | `fsck` | `cli.object-readback`（并作为多数 mutating 场景的状态断言） |
| Maintenance | `cat-file` | `cli.object-readback`、`cli.sha256-object-readback` |
| Maintenance | `hash-object` | `cli.object-readback`、`cli.sha256-object-readback` |
| Maintenance | `archive` | `cli.archive-smoke` |
| Maintenance | `verify-pack` | `cli.verify-pack-smoke` |
| Maintenance | `rev-parse` | `cli.object-readback` |
| Maintenance | `rev-list` | `cli.object-readback` |
| Maintenance | `symbolic-ref` | `cli.reflog-symbolic-ref` |
| Maintenance | `reflog` | `cli.reflog-symbolic-ref` |
| Maintenance | `bisect` | `cli.stash-bisect-worktree` |
| Cross-cutting | `--json` / `--machine` / `--quiet` / `--color` / `--progress` | `cli.cross-cutting-flags` |

**无独立场景（有意排除，见计划 §2.2 / §2.3）**：`index-pack`（隐藏内部命令，仅作为 `cli.verify-pack-smoke` 的 fixture 辅助）；`hooks`（隐藏兼容命令，由专属 cargo 测试覆盖）；`stats`（Libra-only 只读工作区文件统计扩展，非 Git 命令，无自有参数，由 `tests/command/stats_test.rs` 专属覆盖）；`cloud` / `publish` / `code` / `code-control` / `automation` / `usage` / `graph` / `sandbox` / `agent` / `package`（AI/Cloud 扩展，不属于本黑盒版本管理计划）。新增 **Git 兼容** 命令时不得落到此列表——必须新增 owner 场景。

## Wave 1

- [`cli.config-basic-kv`](cli.config-basic-kv.md)
- [`cli.config-scopes`](cli.config-scopes.md)
- [`cli.config-set-input-and-encryption`](cli.config-set-input-and-encryption.md)
- [`cli.config-get-default-and-patterns`](cli.config-get-default-and-patterns.md)
- [`cli.config-list-variants`](cli.config-list-variants.md)
- [`cli.config-unset-compat-flags`](cli.config-unset-compat-flags.md)
- [`cli.config-import-path-edit`](cli.config-import-path-edit.md)
- [`cli.config-key-generation`](cli.config-key-generation.md)
- [`cli.config-git-compat-mode`](cli.config-git-compat-mode.md)
- [`cli.init-basic`](cli.init-basic.md)
- [`cli.init-directory-and-quiet`](cli.init-directory-and-quiet.md)
- [`cli.init-branch-and-format-options`](cli.init-branch-and-format-options.md)
- [`cli.init-bare-and-shared`](cli.init-bare-and-shared.md)
- [`cli.init-template`](cli.init-template.md)
- [`cli.init-from-git-repository`](cli.init-from-git-repository.md)
- [`cli.init-vault`](cli.init-vault.md)
- [`cli.commit-status-log`](cli.commit-status-log.md)
- [`cli.branch-switch-checkout`](cli.branch-switch-checkout.md)
- [`cli.restore-reset-diff`](cli.restore-reset-diff.md)
- [`cli.stash-bisect-worktree`](cli.stash-bisect-worktree.md)
- [`cli.tag-basic`](cli.tag-basic.md)
- [`cli.merge-rebase-cherry-revert-smoke`](cli.merge-rebase-cherry-revert-smoke.md)
- [`cli.merge-conflict-continue`](cli.merge-conflict-continue.md)
- [`cli.rebase-conflict-continue`](cli.rebase-conflict-continue.md)
- [`cli.grep-blame-describe-shortlog`](cli.grep-blame-describe-shortlog.md)
- [`cli.clean-rm-mv-lfs-basic`](cli.clean-rm-mv-lfs-basic.md)
- [`cli.reflog-symbolic-ref`](cli.reflog-symbolic-ref.md)
- [`cli.open-smoke`](cli.open-smoke.md)
- [`cli.cross-cutting-flags`](cli.cross-cutting-flags.md)

## Wave 2

- [`cli.schema-upgrade-observable`](cli.schema-upgrade-observable.md)
- [`cli.clone-fetch-pull-local`](cli.clone-fetch-pull-local.md)
- [`cli.fetch-depth-local`](cli.fetch-depth-local.md)
- [`cli.push-local-file-remote-rejected`](cli.push-local-file-remote-rejected.md)
- [`cli.object-readback`](cli.object-readback.md)
- [`cli.show-ref-exclude-existing`](cli.show-ref-exclude-existing.md)
- [`cli.ls-tree-smoke`](cli.ls-tree-smoke.md)
- [`cli.sha256-object-readback`](cli.sha256-object-readback.md)
- [`cli.gc-smoke`](cli.gc-smoke.md)
- [`cli.archive-smoke`](cli.archive-smoke.md)
- [`cli.verify-pack-smoke`](cli.verify-pack-smoke.md)

## Wave 3

- [`live.github-create-push-clone-fetch`](live.github-create-push-clone-fetch.md)

## 参数覆盖表

跨场景的命令 flag 矩阵：[`_parameter-tables.md`](_parameter-tables.md)
