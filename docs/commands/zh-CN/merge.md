# `libra merge`

将一个目标合并到当前分支。

## 概要

```text
libra merge [--ff | --ff-only | --no-ff] [-s ours | -X <ours|theirs>] [--allow-unrelated-histories] [--log[=<n>] | --no-log] [--squash | --no-commit] [-m <msg>] [--no-verify] [--autostash | --no-autostash] [--no-edit] [--stat | -n | --no-stat] [--verify-signatures | --no-verify-signatures] [--no-rerere-autoupdate] [--no-gpg-sign] [--dry-run] <branch>
libra merge --continue [-m <msg>] [--no-verify]
libra merge --abort
libra merge --restart
```

## 说明

`libra merge <branch>` 会解析本地分支、提交哈希，或 `refs/remotes/origin/main` 这样的远程跟踪引用。

如果当前分支可以快进，Libra 会将分支指针移动到目标提交，并恢复索引和工作树。如果分支已经分叉，Libra 会使用 merge base 执行单头三方合并；当历史留下不止一个 merge base 时，改用由它们递归折叠出的虚拟祖先（见下文）。

默认三方策略支持 `-X ours` / `-X theirs`：只在冲突 hunk/路径选择指定一侧，双方无冲突变更仍全部保留。它不同于 `-s ours`；后者会创建双父 merge commit（目标已经是当前分支祖先时除外），但完整保留当前 HEAD tree。其它 strategy/strategy option 会在参数解析阶段拒绝。

### 交叉合并历史（多个 merge base）

两条互相合并过的分支会留下**多个** merge base，彼此之间没有谁更好。任取其一会报出历史本可解释的冲突，因此 Libra 按 Git recursive 策略折叠它们：

1. merge base 按 object id 升序排序，从左到右两两折叠，每次折叠本身又是一次三方合并（递归地拥有自己的 merge base）。
2. 折叠出的单一 tree —— **虚拟祖先** —— 作为真实合并的 base。

折叠内部的判定与真实合并不同，这一点与 Git 一致：

- `-X ours` / `-X theirs` **不生效**：虚拟祖先是合成输入，用户并未要求对它偏袒。
- 内容冲突不上抛，而是**作为内容**记录下来；冲突标记每递归一层加宽两个字符，使嵌套冲突绝不会被误读成外层合并产生的冲突。
- change/delete 保留 base 版本——「已修改」与「已删除」之间没有中点。
- 二进制内容（与 Git 判据一致：前 8000 字节含 NUL，或单个输入超过 1023 MiB）不做行级合并：祖先取 base 的内容；无 base 时取空 blob。
- 符号链接，以及两侧**类型不同**的条目，一律保留 base 版本；无 base 时该路径在祖先中直接不存在。
- 结果 mode 遵循 Git 规则：两侧一致或 ours 未改时取对侧 mode，否则取 ours。
- 嵌套超过 20 层，或同一层要折叠超过 32 个 merge base 时——在加载任何 base 之前先按裸 id 计数，折叠过程中再按收集到的候选祖先计数——报 `LBR-UNSUPPORTED-001` 拒绝而不是继续（折叠的工作量随宽度平方增长；交叉合并只有两个）。`-s ours` 与 `--ff-only` 从不折叠，也从不因宽度被拒（分叉的 `--ff-only` 一如既往以 non-fast-forward 拒绝）。

合成 commit 的 parents 是**到目前为止折叠过的全部真实 merge base**（Git 每折叠一步串一个双父虚拟提交）；可达历史相同，object id 不同。虚拟祖先的 tree 与合成 commit 会作为普通 loose object 写入，但它们是**一次性对象**：有意不写入 merge state，因而不是 GC root。`libra maintenance run --task gc` 随时可以回收它们（包括冲突合并仍在进行时）——没有任何东西依赖它们存活，`libra merge --restart` 会从真实 merge base 重新算出同一个祖先。因此 `merge-state.json` 只在 merge base 是单个真实提交时才记录 `base`。

`libra merge --dry-run` 预演交叉合并遵循与其它预演相同的契约——不写对象库、索引、工作树、HEAD、reflog、merge 状态与 autostash sidecar：折叠把 blob 留在内存里，不落任何虚拟 tree 或 commit。（该契约**不**覆盖 CLI 在任何命令运行之前做的仓库级例行维护，例如打开数据库时的 schema 自动升级；见 `docs/development/commands/merge.md`。）

没有共同祖先的历史默认仍被拒绝。显式传入 `--allow-unrelated-histories` 时，Libra 使用虚拟空 merge base：不相交的 root tree 正常合并，重叠新增正常冲突，且 conflict state 可跨 `--continue` / `--abort` / `--restart` 恢复，不会写入伪造的 base object。

干净的三方合并会创建双父合并提交、更新 HEAD、重建索引、恢复工作树，并写入 merge reflog 条目。有冲突的三方合并会向工作树写入行级冲突标记（与 Git 一致——仅把发散的 hunk 包在 `<<<<<<< HEAD` / `=======` / `>>>>>>>` 之间，共享上下文留在标记外；二进制或 modify/delete 路径回退整文件标记），写入未合并的索引 stage，保存 Libra merge 状态，并返回 `LBR-CONFLICT-002`，同时给出 `libra merge --continue` 和 `libra merge --abort` 的提示。

### 冲突标记风格（`merge.conflictStyle`）

标记格式遵循 Git 兼容的 `merge.conflictStyle` 配置键（仅配置——与 Git 一致，`merge` 无 CLI 风格参数）：`libra config merge.conflictStyle diff3`。`merge`（默认/未设置）为上述双标记风格；`diff3` 额外在 `||||||| base` 标记与 `=======` 分隔符之间输出共同祖先内容；其它值（含未实现的 `zdiff3`）在需要渲染冲突时直接报错（退出 128），绝不静默回落默认风格。**多 merge base 的合并是例外**：递归虚拟祖先自身的内容依赖该风格（Git 在每一层递归同样传入它），因此该值在合并开始前就被解析——非法值会拦下一个本来会干净完成的交叉合并。该配置同时被 `libra merge` 与 `libra cherry-pick` 的行级文本冲突尊重；二进制与 modify/delete 冲突保持两段式整文件呈现（Git 亦不为其输出 base 块），`libra rebase` 目前始终渲染无 base 块的整文件标记、不受此配置影响。

### 目录/文件冲突（D/F 冲突）

一侧保留（或修改）**文件** `foo`、另一侧把 `foo` 变成**目录**（删除文件并在 `foo/` 下新增路径）时，两者无法共用一个路径。Libra 遵循 Git recursive 策略（`merge-ort.c` 的 `unique_path`）：目录保留 `foo`，文件按持有它的分支写到唯一名字下——文件在我方时为 `foo~HEAD`，在对方时为 `foo~<branch>`（分支名中的 `/` 替换为 `_`）；该名字已被本次合并的任一输入或其结果占用——无论占用者是文件还是目录，仅 merge base 有过的路径也算，与 Git `unique_path` 检查的集合一致——时追加 `_0`、`_1`……。合并以冲突停止（`LBR-CONFLICT-002`），并先打印 Git 的提示行：

```text
CONFLICT (file/directory): directory in the way of foo from HEAD; moving it to foo~HEAD instead.
```

被移走的文件就是未合并路径：索引在新名字下记录文件所在侧的 stage 2（我方）或 stage 3（对方），若 merge base 在 `foo` 处跟踪的是文件则再记 stage 1；没有 stage 0 条目，`merge-state.json` 的 `conflicted_paths` 列出的是新名字。目录内容照常合并。与 Git 相同有两种形态：仅一侧*新增*的文件是纯粹的 file/directory 冲突——原样写到新名字，`--dry-run` 报为 `file-directory`；merge base 已跟踪、且文件侧*修改过*的文件是一个只是换了位置的 modify/delete 冲突——新名字下带 base（stage 1）与修改侧，`--dry-run` 报为 `modify-delete` 并附 `original_path`，同时打印 Git 的第二行（`CONFLICT (modify/delete): foo~HEAD deleted in <branch> and modified in HEAD.  Version HEAD of foo~HEAD left in tree.`），文件按该行所说原样留在新名字下。一侧未动、另一侧换成目录的文件则是普通的干净删除：无冲突、无提示。只含*空* tree 的目录仅在 merge base 在该路径上什么都没有时才算挡路——Git 会把这样的新目录原样采纳；base 已有该路径时 Git 会遍历该目录、发现没有文件，于是文件留在原地（普通 modify/delete）。两种情形均与 `git merge` 对人工构造 tree 的实测一致。`--json`/`--machine` 下不打印这些提示：stdout 保持机器可读，冲突由 stderr 上的错误信封承载。策略选项（`-X ours` / `-X theirs`）只裁决内容 hunk：目录之下的 modify/delete 仍是冲突，与 Git 一致。合并绝不*穿过*工作树里的符号链接写入或删除——被忽略的 `foo -> 别处` 挡在要写的 `foo/…` 前面、或压在合并要删除的已跟踪文件之上时，整个合并在任何改动前被拒绝；恰好占着移位文件名字的符号链接则被文件替换，而*已跟踪*的符号链接 `foo` 让位给目录 `foo/` 时像普通文件一样移到 `foo~HEAD`。只有当前已跟踪的路径才会被删除：碰巧沿用历史名字的未跟踪文件会留下。若要保留被移走的文件，编辑它、用 `libra add foo~…` 暂存后 `libra merge --continue`；或运行 `libra merge --abort`：它把 `foo` 恢复为合并前的样子，并同时移除被移走的副本与合并创建的目录（合并腾空的目录会被清理，嵌套的 `foo/a/` 不会挡住文件回位）。目前无法「丢弃」被移走的文件：`libra rm` 不接受未合并路径，只能用 `--abort` 得到不含该文件的结果（Git 用 `git rm foo~HEAD` 解决这一情形）。

合并后**空无一物**的目录（其下每个条目都被文件侧删除、另一侧未动）不算挡路：文件留在原路径，与 Git 一致。递归的交叉合并折叠（见上）内部同样适用该规则且不询问用户：文件在虚拟祖先中移到 `foo~Temporary merge branch 1`（或 `2`），与 Git 在 `call_depth > 0` 时完全一致，因此祖先 tree 永远不会在同一名字下同时持有 blob 与子树。

此处只处理两侧**已跟踪**内容之间的冲突。工作树里会被合并覆盖的**未跟踪** `foo` 或 `foo~HEAD` 由既有的 untracked-overwrite 检查预先拒绝；**被忽略**的文件一律视为可弃并被替换——无论它占着这些名字，还是挡在合并要创建的目录位置上（会被替换成目录）；与 `git merge` 的处理完全一致，因此不要把重要内容放在会被合并写入的被忽略路径上。rename 检测引起的冲突在 rename 支持落地前不在范围内。

### 子模块（`160000` gitlink 条目）

Libra 定位 monorepo 客户端，永不合并 submodule 内容。三路合并对 gitlink 分两档处理：

- **合并需要对该 gitlink 做裁决**——任一侧记录的 commit id 与 merge base 不同（包括某一侧新增或删除该条目）。合并在**写入任何内容之前**被拒绝（不写 merge state、不动索引与工作树、HEAD 不移动），错误码 `LBR-UNSUPPORTED-001`，消息包含路径：

  ```
  error: merge would have to merge the submodule (gitlink) entry 'vendor': Libra does not support submodules
  ```

  请在 Libra 之外解决 submodule 指针，或从参与合并的分支中移除该 gitlink 条目。

- **三侧记录的 commit id 完全一致**——没有任何决策要做，指针原样写入合并结果。（此前这类条目会被静默丢弃，等于把 submodule 从合并结果树里删掉。）

`libra rebase` 与 `libra cherry-pick` 共用同一道校验与同一措辞，仅把 `merge` 换成 `rebase` / `cherry-pick`。

Libra 仍未实现 octopus merge、`ours` 以外的 merge strategy、`ours`/`theirs` 以外的 strategy option，或交互式消息编辑（`--edit`/启动编辑器）。签名验证（`--verify-signatures`）已支持，但仅限本仓库 vault PGP key（无外部 GPG keyring）。

### 会改变历史的 merge 默认值

未传对应 CLI 标志时，Libra 按 local → global → system 级联读取 Git 兼容默认值：`merge.ff=true|false|only` 分别允许快进、强制双父 merge commit、仅允许快进（`--ff`/`--no-ff`/`--ff-only` 优先；`only` 与 `--ff-only` 只拒绝真正分叉的历史——可快进的 `--squash`/`--no-commit` 仍被允许，与 Git 一致）；`merge.log=true|false|<n>` 在自动生成的 merge 消息中追加最多 20 条或 `<n>` 条目标侧提交 subject。`--log[=<n>]` / `--no-log` 覆盖配置并 last-one-wins，bare `--log` 为 20；显式 `-m` 会抑制仅来自配置的 `merge.log`，但显式 `--log` 仍会把 shortlog 追加到自定义消息。解析后的消息会记录进 merge state，冲突或 `--no-commit` 后用 `merge --continue` 收尾时原样提交；`merge.verifySignatures=true|false` 控制 tip 签名验证（正反 CLI 标志优先），验证在解析出的目标上、任何变更（包括 autostash 创建）之前执行——被拒绝的 merge 不写任何内容（无 stash 条目、无对象）。无效或不可读的 local/global 值在修改 HEAD/index/工作树/merge state 前以 `LBR-CLI-002` 或 `LBR-IO-001` 失败；local/global 加密值先解密，不可读或不支持的 system scope 跳过。例外：schema 比当前 Libra 二进制更新的全局配置库会在一次性去重警告后被跳过而不失败（见 `LBR-CONFIG-001`）。

### `--dry-run`（Libra 扩展）

`libra merge --dry-run <branch>` 预演合并结果而**不写任何东西**——不动 HEAD、索引、工作树、reflog、merge 状态与对象库（自动合并的 blob 仅在内存中计算）。因为只读，脏工作树也可预演（注意预演不校验工作树干净度，真实合并仍可能拒绝）。结果：fast-forward / 已最新 / 干净三方合并 → 退出 0；会冲突 → 输出 `Would conflict in: <paths>` 并退出 1（结果信号，非真实冲突的 128）。`--json` 下带 `"dry_run": true`（冲突时另有 `"would_conflict": true`、`conflicted_paths` 与 `conflict_kinds`——每个冲突路径一个 `{"path", "kind", "original_path"?}` 对象，`kind` 为 `content` / `modify-delete` / `file-directory`，`original_path` 仅目录/文件移位时出现并记录原路径，此时 `path` 是文件将被写到的带 `~` 后缀的名字），真实合并的输出不含这些键（schema 冻结）。

### `--restart`（Libra 扩展，移植 Lore `branch merge restart`）

`libra merge --restart` 一步「推倒重来」：像 `--abort` 一样恢复合并前状态（**丢弃**已做的冲突解决），随后立刻对**记录的目标提交**重跑同一个合并（即使分支已移动也确定重现），重新生成冲突标记与 merge 状态。recovery-critical 的 `--allow-unrelated-histories` 会重放；原 `-m`/`--no-ff` 等展示/策略选项不重放。要求**有冲突**的合并：对已暂存的 `--no-commit` 干净合并会拒绝（用 `--continue` 完成或 `--abort` 丢弃）；无合并进行中时报错（均退出 128）。

## 选项

| 选项 | 说明 |
|--------|-------------|
| `<branch>` | 要合并的目标分支、提交或远程跟踪引用。 |
| `-m, --message <MSG>` | 覆盖合并提交消息（默认 `Merge <branch> into <head>`）。也可与 `--continue` 同用，覆盖冲突合并开始时记录的消息——这是 Libra 扩展：Git 的 `--continue` 不接受参数，而 Libra 的 merge 从不打开编辑器。 |
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
| `--no-verify` | 本次 merge 跳过全部 `.libra/hooks`；与 `--continue` 一起使用时绕过待执行的 commit/消息/post hooks。 |
| `--no-edit` | 接受自动生成的合并消息而不启动编辑器。Libra 从不为 merge 打开编辑器，故此为对齐 Git 而接受的 no-op。 |
| `--stat` | 合并完成后显示 diffstat（合并前 HEAD 与新提交之间的变更）。Git 默认显示；Libra 默认不显示，故用 `--stat` 主动开启。与 `--no-stat`/`-n` 构成 last-wins 切换。仅人类输出。 |
| `-n`, `--no-stat` | 合并结束时不显示 diffstat（Libra 默认）。与 `--stat` 构成 last-wins 切换。 |
| `--no-progress` | 不显示进度条。为对齐 Git 而接受的 no-op：Libra 的 merge 从不渲染进度条。 |
| `--verify-signatures` | 验证被合并分支 tip 的 PGP 签名，未签名或签名无效则中止；覆盖 `merge.verifySignatures`。仅能验证本仓库 vault PGP key 所签。 |
| `--no-verify-signatures` | 不验证被合并提交的签名，覆盖 `merge.verifySignatures=true`；与正向标志 last-wins。 |
| `--no-rerere-autoupdate` | 为对齐 Git 而接受。rerere 已集成：`rerere.enabled` 开启时，冲突合并会记录每个冲突的 preimage 并在有匹配记录时回放已保存的解法；回放文件是否自动暂存跟随 `rerere.autoUpdate` 配置。逐次调用的覆盖未实现——暂存始终跟随配置。（Git 的正向 `--rerere-autoupdate` 未公开。） |
| `--no-gpg-sign` | 不对合并提交 GPG 签名。为对齐 Git 而接受的 no-op：Libra 的 merge 从不签名。（Git 的 `-S`/`--gpg-sign` 未实现。） |
| `--continue` | 在冲突已解决并暂存后完成进行中的合并。 |
| `--abort` | 恢复合并前的 HEAD、索引和工作树。 |
| `--autostash` / `--no-autostash` | 合并前保存本地 tracked 变更，并在结束时分别恢复 staged index 与 unstaged worktree 层；发生冲突时 held 在 `stash list` 之外，直到 `--continue`/`--abort`。恢复冲突会先保存到普通 stash list 并提示，变更不会丢失。配置项为 `merge.autostash`（布尔；无效值硬错误）；不保存 untracked 文件。`--json` 增加 `autostash: applied\|stashed\|kept`。 |
| `--dry-run` | Libra 扩展：预演合并结果而不写任何东西（见上文）。干净预演退出 0，会冲突退出 1。与 `--continue`/`--abort`/`--restart`/`--squash`/`--no-commit` 互斥。 |
| `--restart` | Libra 扩展：像 `--abort` 一样恢复合并前状态（丢弃解决工作）后，立刻对记录的目标提交重跑同一合并（见上文）。不接受分支与合并选项。 |
| `--json` | 输出结构化成功信封。 |
| `--machine` | 以一行紧凑 JSON 输出同一结构化信封。 |
| `--quiet` | 抑制人类可读的成功输出。 |

## 仓库 hooks

`pre-merge-commit` 会阻止自动 merge commit（含 `--continue`），但不在 fast-forward、
squash 或尚未继续的 `--no-commit` 结果上运行。自动 merge commit 随后运行
`prepare-commit-msg <file> merge`、`commit-msg <file>` 和 advisory `post-commit`；
消息 hooks 可修改 `.libra/COMMIT_EDITMSG`。merge/fast-forward 完成后
`post-merge` 以参数 `0` advisory 运行，squash 后参数为 `1`；already-up-to-date
和冲突结果不运行。`--no-verify` 跳过该 merge 生命周期的全部 hooks。pull 共用同一
merge 生命周期；需要
显式绕过时设置 `LIBRA_NO_HOOKS=1`。sandbox 与失败契约见
[仓库 hooks](repository-hooks.md)。

## 常用命令

```bash
libra merge feature-x
libra merge -X ours feature-x
libra merge -s ours obsolete-history
libra merge --allow-unrelated-histories imported-root
libra merge --log=10 feature-x
libra merge refs/remotes/origin/main
libra merge --continue
libra merge --continue -m "merge: reconcile release notes"
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

`--abort` 设置 `aborted: true`；`--continue` 设置 `continued: true`。冲突失败会在 stderr 上返回带有 `LBR-CONFLICT-002` 的错误信封。 `--dry-run` 额外带 `dry_run`、`would_conflict`、`conflicted_paths` 与 `conflict_kinds`（每个冲突路径一个 `{"path", "kind", "original_path"?}` 对象，`kind` 为 `content`、`modify-delete` 或 `file-directory`；`original_path` 仅在目录/文件移位时出现，记录文件原来的路径，此时 `path` 与 `conflicted_paths` 里都是带 `~` 后缀的目标名）。

## 参数对比：Libra vs Git vs jj

| 参数 | Libra | Git | jj |
|-----------|-------|-----|----|
| 分支目标 | `<branch>`（单个目标） | `<commit>...`（一个或多个） | N/A（使用 `jj new`） |
| 快进 | 支持 | 支持 | N/A |
| 单头三方合并 | 支持 | 支持 | N/A |
| 交叉合并（多个 merge base） | 递归虚拟祖先（折叠顺序：object id 升序；最大深度 20，每层最多 32 个 base） | 递归虚拟祖先（`-s recursive`/`ort`，深度无上限） | N/A |
| Continue / abort | `--continue`, `--abort` | `--continue`, `--abort` | N/A |
| Octopus merge | 不支持 | 支持 | N/A |
| 仅快进 | `--ff-only` | `--ff-only` | N/A |
| 强制合并提交 | `--no-ff` | `--no-ff` | N/A |
| Squash | `--squash` | `--squash` | N/A |
| 不提交 | `--no-commit` | `--no-commit` | N/A |
| 跳过全部 merge lifecycle hooks | `--no-verify` | `--no-verify` | N/A |
| 提交消息 | `-m <msg>` | `-m <msg>` | N/A |
| 不编辑 | `--no-edit`（no-op；从不编辑） | `--no-edit` | N/A |
| 合并后 diffstat | `--stat`（打印）；`-n` / `--no-stat`（默认：不打印） | `--stat`（默认） / `-n` / `--no-stat` | N/A |
| 不显示进度条 | `--no-progress`（no-op；从不渲染） | `--no-progress` | N/A |
| 禁用签名验证 | `--no-verify-signatures`（默认；关闭 `--verify-signatures`） | `--no-verify-signatures` | N/A |
| 不更新 rerere | `--no-rerere-autoupdate`（已接受；暂存跟随 `rerere.autoUpdate`） | `--no-rerere-autoupdate` | N/A |
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
| 三路合并需要裁决 `160000` gitlink（submodule） | `LBR-UNSUPPORTED-001` | 128 |
| 递归虚拟祖先嵌套超过 20 层或同层超过 32 个 base | `LBR-UNSUPPORTED-001` | 128 |
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
