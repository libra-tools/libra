# `libra merge`

将一个目标合并到当前分支。

## 概要

```text
libra merge [--ff | --ff-only | --no-ff] [-s ours | -X <ours|theirs>] [--allow-unrelated-histories] [--log[=<n>] | --no-log] [--squash | --no-commit] [-m <msg>] [--autostash | --no-autostash] [--no-edit] [--stat | -n | --no-stat] [--verify-signatures | --no-verify-signatures] [--no-rerere-autoupdate] [--no-gpg-sign] [--dry-run] <branch>
libra merge --continue
libra merge --abort
libra merge --restart
```

## 说明

`libra merge <branch>` 会解析本地分支、提交哈希，或 `refs/remotes/origin/main` 这样的远程跟踪引用。

如果当前分支可以快进，Libra 会将分支指针移动到目标提交，并恢复索引和工作树。如果分支已经分叉，Libra 会使用 merge base 执行单头三方合并。

默认三方策略支持 `-X ours` / `-X theirs`：只在冲突 hunk/路径选择指定一侧，双方无冲突变更仍全部保留。它不同于 `-s ours`；后者会创建双父 merge commit（目标已经是当前分支祖先时除外），但完整保留当前 HEAD tree。其它 strategy/strategy option 会在参数解析阶段拒绝。

没有共同祖先的历史默认仍被拒绝。显式传入 `--allow-unrelated-histories` 时，Libra 使用虚拟空 merge base：不相交的 root tree 正常合并，重叠新增正常冲突，且 conflict state 可跨 `--continue` / `--abort` / `--restart` 恢复，不会写入伪造的 base object。

干净的三方合并会创建双父合并提交、更新 HEAD、重建索引、恢复工作树，并写入 merge reflog 条目。有冲突的三方合并会向工作树写入行级冲突标记（与 Git 一致——仅把发散的 hunk 包在 `<<<<<<< HEAD` / `=======` / `>>>>>>>` 之间，共享上下文留在标记外；二进制或 modify/delete 路径回退整文件标记），写入未合并的索引 stage，保存 Libra merge 状态，并返回 `LBR-CONFLICT-002`，同时给出 `libra merge --continue` 和 `libra merge --abort` 的提示。

### 冲突标记风格（`merge.conflictStyle`）

标记格式遵循 Git 兼容的 `merge.conflictStyle` 配置键（仅配置——与 Git 一致，`merge` 无 CLI 风格参数）：`libra config merge.conflictStyle diff3`。`merge`（默认/未设置）为上述双标记风格；`diff3` 额外在 `||||||| base` 标记与 `=======` 分隔符之间输出共同祖先内容；其它值（含未实现的 `zdiff3`）在需要渲染冲突时直接报错（退出 128），绝不静默回落默认风格。该配置同时被 `libra merge` 与 `libra cherry-pick` 的行级文本冲突尊重；二进制与 modify/delete 冲突保持两段式整文件呈现（Git 亦不为其输出 base 块），`libra rebase` 目前始终渲染无 base 块的整文件标记、不受此配置影响。

Libra 仍未实现 octopus merge、`ours` 以外的 merge strategy、`ours`/`theirs` 以外的 strategy option，或交互式消息编辑（`--edit`/启动编辑器）。签名验证（`--verify-signatures`）已支持，但仅限本仓库 vault PGP key（无外部 GPG keyring）。

### 会改变历史的 merge 默认值

未传对应 CLI 标志时，Libra 按 local → global → system 级联读取 Git 兼容默认值：`merge.ff=true|false|only` 分别允许快进、强制双父 merge commit、仅允许快进（`--ff`/`--no-ff`/`--ff-only` 优先；`only` 与 `--ff-only` 只拒绝真正分叉的历史——可快进的 `--squash`/`--no-commit` 仍被允许，与 Git 一致）；`merge.log=true|false|<n>` 在自动生成的 merge 消息中追加最多 20 条或 `<n>` 条目标侧提交 subject。`--log[=<n>]` / `--no-log` 覆盖配置并 last-one-wins，bare `--log` 为 20；显式 `-m` 会抑制仅来自配置的 `merge.log`，但显式 `--log` 仍会把 shortlog 追加到自定义消息。解析后的消息会记录进 merge state，冲突或 `--no-commit` 后用 `merge --continue` 收尾时原样提交；`merge.verifySignatures=true|false` 控制 tip 签名验证（正反 CLI 标志优先），验证在解析出的目标上、任何变更（包括 autostash 创建）之前执行——被拒绝的 merge 不写任何内容（无 stash 条目、无对象）。无效或不可读的 local/global 值在修改 HEAD/index/工作树/merge state 前以 `LBR-CLI-002` 或 `LBR-IO-001` 失败；local/global 加密值先解密，不可读或不支持的 system scope 跳过。

### `--dry-run`（Libra 扩展）

`libra merge --dry-run <branch>` 预演合并结果而**不写任何东西**——不动 HEAD、索引、工作树、reflog、merge 状态与对象库（自动合并的 blob 仅在内存中计算）。因为只读，脏工作树也可预演（注意预演不校验工作树干净度，真实合并仍可能拒绝）。结果：fast-forward / 已最新 / 干净三方合并 → 退出 0；会冲突 → 输出 `Would conflict in: <paths>` 并退出 1（结果信号，非真实冲突的 128）。`--json` 下带 `"dry_run": true`（冲突时另有 `"would_conflict": true`），真实合并的输出不含这两个键（schema 冻结）。

### `--restart`（Libra 扩展，移植 Lore `branch merge restart`）

`libra merge --restart` 一步「推倒重来」：像 `--abort` 一样恢复合并前状态（**丢弃**已做的冲突解决），随后立刻对**记录的目标提交**重跑同一个合并（即使分支已移动也确定重现），重新生成冲突标记与 merge 状态。recovery-critical 的 `--allow-unrelated-histories` 会重放；原 `-m`/`--no-ff` 等展示/策略选项不重放。要求**有冲突**的合并：对已暂存的 `--no-commit` 干净合并会拒绝（用 `--continue` 完成或 `--abort` 丢弃）；无合并进行中时报错（均退出 128）。

## 选项

| 选项 | 说明 |
|--------|-------------|
| `<branch>` | 要合并的目标分支、提交或远程跟踪引用。 |
| `-m, --message <MSG>` | 覆盖合并提交消息（默认 `Merge <branch> into <head>`）。 |
| `--ff` | 允许可行的快进，覆盖 `merge.ff=false|only`。 |
| `--ff-only` | 仅当当前分支可快进时才合并，否则失败。 |
| `--no-ff` | 即使可以快进也强制生成双父合并提交。 |
| `-s ours`, `--strategy=ours` | 以双父提交记录合并关系，但完整保留当前 HEAD tree；不同于 `-X ours`。其它 strategy 被拒绝。 |
| `-X ours`, `-X theirs`, `--strategy-option=<ours\|theirs>` | 只在冲突 hunk/路径偏向指定一侧；双方无冲突变更仍保留。可重复，最后一个值生效；不能与 `-s ours` 组合。 |
| `--allow-unrelated-histories` | 以虚拟空 merge base 允许没有共同祖先的历史；冲突 `--restart` 会保留此许可。 |
| `--log[=<N>]` | 向 merge 消息追加最多 N 条目标侧 subject；bare `--log` 为 20。覆盖 `merge.log`，并可追加到显式 `-m`；与 `--no-log` last-one-wins。 |
| `--no-log` | 禁用 merge 消息 shortlog，覆盖 `merge.log` 和更早的 `--log`。 |
| `--squash` | 生成合并后的索引/工作树但不创建提交、不移动 HEAD；随后用普通 `libra commit` 收尾。 |
| `--no-commit` | 执行合并并暂存结果但停在提交之前；随后用 `libra merge --continue` 收尾。 |
| `--no-edit` | 接受自动生成的合并消息而不启动编辑器。Libra 从不为 merge 打开编辑器，故此为对齐 Git 而接受的 no-op。 |
| `--stat` | 合并完成后显示 diffstat（合并前 HEAD 与新提交之间的变更）。Git 默认显示；Libra 默认不显示，故用 `--stat` 主动开启。与 `--no-stat`/`-n` 构成 last-wins 切换。仅人类输出。 |
| `-n`, `--no-stat` | 合并结束时不显示 diffstat（Libra 默认）。与 `--stat` 构成 last-wins 切换。 |
| `--no-progress` | 不显示进度条。为对齐 Git 而接受的 no-op：Libra 的 merge 从不渲染进度条。 |
| `--verify-signatures` | 验证被合并分支 tip 的 PGP 签名，未签名或签名无效则中止；覆盖 `merge.verifySignatures`。仅能验证本仓库 vault PGP key 所签。 |
| `--no-verify-signatures` | 不验证被合并提交的签名，覆盖 `merge.verifySignatures=true`；与正向标志 last-wins。 |
| `--no-rerere-autoupdate` | 合并后不更新 rerere 索引。为对齐 Git 而接受的 no-op：Libra 无 rerere。（Git 的 `--rerere-autoupdate` 未公开。） |
| `--no-gpg-sign` | 不对合并提交 GPG 签名。为对齐 Git 而接受的 no-op：Libra 的 merge 从不签名。（Git 的 `-S`/`--gpg-sign` 未实现。） |
| `--continue` | 在冲突已解决并暂存后完成进行中的合并。 |
| `--abort` | 恢复合并前的 HEAD、索引和工作树。 |
| `--autostash` / `--no-autostash` | 合并前保存本地 tracked 变更，并在结束时分别恢复 staged index 与 unstaged worktree 层；发生冲突时 held 在 `stash list` 之外，直到 `--continue`/`--abort`。恢复冲突会先保存到普通 stash list 并提示，变更不会丢失。配置项为 `merge.autostash`（布尔；无效值硬错误）；不保存 untracked 文件。`--json` 增加 `autostash: applied\|stashed\|kept`。 |
| `--dry-run` | Libra 扩展：预演合并结果而不写任何东西（见上文）。干净预演退出 0，会冲突退出 1。与 `--continue`/`--abort`/`--restart`/`--squash`/`--no-commit` 互斥。 |
| `--restart` | Libra 扩展：像 `--abort` 一样恢复合并前状态（丢弃解决工作）后，立刻对记录的目标提交重跑同一合并（见上文）。不接受分支与合并选项。 |
| `--json` | 输出结构化成功信封。 |
| `--machine` | 以一行紧凑 JSON 输出同一结构化信封。 |
| `--quiet` | 抑制人类可读的成功输出。 |

## 常用命令

```bash
libra merge feature-x
libra merge -X ours feature-x
libra merge -s ours obsolete-history
libra merge --allow-unrelated-histories imported-root
libra merge --log=10 feature-x
libra merge refs/remotes/origin/main
libra merge --continue
libra merge --abort
libra merge --dry-run feature-x
libra merge --restart
libra merge --json feature-x
```

## 冲突生命周期

当合并发生冲突时：

1. 编辑包含冲突标记的文件。
2. 使用 `libra add <path>` 暂存每个已解决路径。
3. 运行 `libra merge --continue` 创建双父合并提交。

在继续之前运行 `libra merge --abort` 可将分支、索引和工作树恢复到合并前提交。当存在 merge 状态时，`libra status` 会显示进行中的合并目标，以及 continue/abort 命令。

## 人类可读输出

快进：

```text
Fast-forward
```

干净三方合并：

```text
Merge made by the 'three-way' strategy.
```

Ours strategy：

```text
Merge made by the 'ours' strategy.
```

已经是最新：

```text
Already up to date.
```

`--continue` 后：

```text
Merge completed.
```

`--abort` 后：

```text
Merge aborted.
```

冲突错误会通过 Libra 的标准结构化错误信封打印到 stderr，并包含恢复提示。

## JSON / Machine 输出

成功输出保留历史上的 `files_changed` 数值字段，并仅在相关时添加 merge 生命周期字段。

```json
{
  "ok": true,
  "command": "merge",
  "data": {
    "strategy": "three-way",
    "old_commit": "abc1234...",
    "commit": "def5678...",
    "files_changed": 2,
    "up_to_date": false,
    "parents": ["abc1234...", "fedcba9..."]
  }
}
```

`-s ours` 使用 `strategy: "ours"`、`files_changed: 0` 并报告两个 parent。已经最新的合并使用 `strategy: "already-up-to-date"`、`commit: null`、`files_changed: 0` 和 `up_to_date: true`。

`--abort` 设置 `aborted: true`；`--continue` 设置 `continued: true`。冲突失败会在 stderr 上返回带有 `LBR-CONFLICT-002` 的错误信封。

## 参数对比：Libra vs Git vs jj

| 参数 | Libra | Git | jj |
|-----------|-------|-----|----|
| 分支目标 | `<branch>`（单个目标） | `<commit>...`（一个或多个） | N/A（使用 `jj new`） |
| 快进 | 支持 | 支持 | N/A |
| 单头三方合并 | 支持 | 支持 | N/A |
| Continue / abort | `--continue`, `--abort` | `--continue`, `--abort` | N/A |
| Octopus merge | 不支持 | 支持 | N/A |
| 仅快进 | `--ff-only` | `--ff-only` | N/A |
| 强制合并提交 | `--no-ff` | `--no-ff` | N/A |
| Squash | `--squash` | `--squash` | N/A |
| 不提交 | `--no-commit` | `--no-commit` | N/A |
| 提交消息 | `-m <msg>` | `-m <msg>` | N/A |
| 不编辑 | `--no-edit`（no-op；从不编辑） | `--no-edit` | N/A |
| 合并后 diffstat | `--stat`（打印）；`-n` / `--no-stat`（默认：不打印） | `--stat`（默认） / `-n` / `--no-stat` | N/A |
| 不显示进度条 | `--no-progress`（no-op；从不渲染） | `--no-progress` | N/A |
| 禁用签名验证 | `--no-verify-signatures`（默认；关闭 `--verify-signatures`） | `--no-verify-signatures` | N/A |
| 不更新 rerere | `--no-rerere-autoupdate`（no-op；无 rerere） | `--no-rerere-autoupdate` | N/A |
| 不 GPG 签名 | `--no-gpg-sign`（no-op；从不签名） | `--no-gpg-sign` | N/A |
| Ours strategy | `-s ours` | `-s ours` | N/A |
| 冲突侧偏好 | `-X ours/theirs` | `-X ours/theirs` | N/A |
| 无关历史 | `--allow-unrelated-histories` | 支持 | N/A |
| Merge 消息 shortlog | `--log[=<n>]` / `--no-log` | 支持 | N/A |
| 其它自定义 strategy/option | 不支持 | 支持 | N/A |
| 验证签名 | `--verify-signatures`（仅 vault-key PGP） | `--verify-signatures` | N/A |
| JSON 输出 | `--json` / `--machine` | 不支持 | N/A |

## 错误处理

| 场景 | StableErrorCode | 退出码 |
|----------|-----------------|------|
| 缺少分支 / 动作 | `LBR-CLI-001` | 129 |
| 无法解析目标引用 | `LBR-CLI-003` | 129 |
| 无法加载合并目标/当前提交/树 | `LBR-REPO-002` | 128 |
| 未传 `--allow-unrelated-histories` 的无关历史 | `LBR-REPO-003` | 128 |
| 不支持的 `-s` / `-X` 值或不兼容的 strategy 组合 | `LBR-CLI-002` | 129 |
| `--verify-signatures`：tip 未签名、签名无效或 vault 不可用 | `LBR-REPO-003` | 128 |
| 合并冲突 | `LBR-CONFLICT-002` | 128 |
| 脏工作树或暂存更改 | `LBR-CONFLICT-002` | 128 |
| 未跟踪文件会被覆盖 | `LBR-CONFLICT-002` | 128 |
| 合并已在进行中 | `LBR-CONFLICT-002` | 128 |
| 对 `--continue` / `--abort` 没有进行中的合并 | `LBR-REPO-003` | 128 |
| `--continue` 仍有未解决的冲突 stage | `LBR-CONFLICT-002` | 128 |
| 无法读取 merge 状态或索引 | `LBR-IO-001` | 128 |
| 无法保存状态、索引、树、提交、HEAD 或工作树 | `LBR-IO-002` | 128 |
