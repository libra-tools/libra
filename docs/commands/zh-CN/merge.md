# `libra merge`

将一个或多个目标合并到当前分支。

## 概要

```text
libra merge [--ff | --ff-only | --no-ff] [-s ours | -X <option>...] [--allow-unrelated-histories] [--log[=<n>] | --no-log] [--squash | --no-commit] [-m <msg>] [--no-verify] [--autostash | --no-autostash] [--no-edit] [--stat | -n | --no-stat] [--verify-signatures | --no-verify-signatures] [--no-rerere-autoupdate] [--no-gpg-sign] [--dry-run] <branch>...
libra merge --continue [-m <msg>] [--no-verify]
libra merge --abort
libra merge --restart
```

## 说明

`libra merge <branch>...` 会解析一个或多个本地分支、提交哈希，或 `refs/remotes/origin/main` 这样的远程跟踪引用。一个目标保持下述单头行为；两个及以上目标进入 octopus 合并路径。

如果当前分支可以快进，Libra 会将分支指针移动到目标提交，并恢复索引和工作树。如果分支已经分叉，Libra 会使用 merge base 执行单头三方合并；当历史留下不止一个 merge base 时，改用由它们递归折叠出的虚拟祖先（见下文）。

开始新合并前，Libra 同时检查 merge 状态和索引。已有 merge 状态时仍提示 `merge --continue` 或 `--abort`；没有 merge 状态但索引仍有未解决条目时（例如冲突 squash 后），即使目标已经最新，或使用 `--dry-run`，也以 `LBR-CONFLICT-002` 拒绝（退出 128），HEAD、索引和工作树保持原样。先解决冲突、用 `libra add` 暂存，再运行普通 `libra commit`，然后才能开始新合并。

默认三方策略支持冲突侧、空白比较和 renormalize 三类 `-X` 选项。`-X ours` / `-X theirs` 只在冲突 hunk/路径选择指定一侧，双方无冲突变更仍全部保留。它不同于 `-s ours`；后者以 HEAD 和所有仍有效的目标作为 parents 创建 merge commit（所有目标均已可达时除外），但完整保留当前 HEAD tree。其它支持值见下文“输入归一化”；未列出的 strategy/strategy option 会在参数解析阶段拒绝。

### Octopus 合并（多个目标）

传入两个及以上目标时，Libra 会剔除重复目标以及能从其它目标到达的冗余目标，同时保持独立 tip 的命令行顺序。若所有剩余目标都已能从 `HEAD` 到达，结果是 `Already up to date.`；否则 octopus 路径永不 fast-forward：成功提交的第一个 parent 是 `HEAD`，后续 parents 是按命令行顺序排列的剩余目标。因此 `--ff-only` 会拒绝任何非 already-up-to-date 的 octopus 合并。

策略启动前，除非给出 `--allow-unrelated-histories`，Libra 要求 `HEAD` 与全部目标共享一段历史，与 Git 的 all-tip octopus 入口门一致。随后各目标的 tree 在内存中依次合并；每一轮都针对该目标与“已纳入的 merge-reference commits 的假想合并”计算共同祖先，包括 criss-cross 的全部 LCA，特意不采用全体 tip 的交集。只有所有轮次都干净后，Libra 才写 merge object、索引、tracked 工作树、merge 状态、ref 或 reflog。任一轮冲突时，命令以 `LBR-CONFLICT-002` 失败，指出目标与路径并包含 “Should not be doing an octopus”；`HEAD`、索引和工作树保持不变。此时不会创建可解决的冲突状态；需要逐个合并目标来解决冲突。

干净的 octopus 合并支持 `--no-commit`：状态会把每个目标保存为 GC root，`--continue` 创建同样 parent 顺序的多父提交，`--abort` 恢复合并前状态。`--squash`、`--dry-run`、`--autostash`、`-s ours`、`-X` 选项、无关历史许可、merge 消息 shortlog、hooks 与签名验证也适用；`--verify-signatures` 会在任何变更前验证每个已解析的目标。octopus 预演会报告干净的 `octopus` 结果，或只读的 would-conflict 结果。与单头 external merge driver 相同，受信任但未沙箱化的 driver 直接修改仓库时超出 Libra 的原子回滚边界。

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

没有共同祖先的历史默认仍被拒绝。显式传入 `--allow-unrelated-histories` 时，Libra 使用虚拟空 merge base：不相交的 root tree 正常合并，重叠新增正常冲突，且非 squash 合并的 conflict state 可跨 `--continue` / `--abort` / `--restart` 恢复，不会写入伪造的 base object。

默认情况下，干净的三方合并会创建双父合并提交、更新 HEAD、重建索引、恢复工作树，并写入 merge reflog 条目。有冲突的三方合并会向工作树写入行级冲突标记（与 Git 一致——仅把发散的 hunk 包在 `<<<<<<< HEAD` / `=======` / `>>>>>>>` 之间；渲染器会重新比较双方 postimage，把共同边缘与足够长的共同片段留在标记外；二进制或 modify/delete 路径回退整文件标记），写入未合并的索引 stage，除 `--squash` 外保存 Libra merge 状态，并返回 `LBR-CONFLICT-002`。非 squash 冲突给出 `libra merge --continue` 和 `libra merge --abort` 的提示；squash 冲突则提示解决、暂存路径后用普通 `libra commit` 创建单亲提交。squash 即使冲突也不移动 HEAD、不记录 merge 状态，因此 `merge --continue`、`--abort`、`--restart` 均报 `no merge in progress`。

### 冲突标记风格（`merge.conflictStyle`）

标记格式遵循 Git 兼容的 `merge.conflictStyle` 配置键（仅配置——与 Git 一致，`merge` 无 CLI 风格参数）：`libra config merge.conflictStyle diff3`。`merge`（默认/未设置）使用双标记风格，并重新 diff 双方 postimage：共同前后缀移到 marker 外，超过三行的共同片段拆开相邻冲突块，至多三行的共同片段留在同一冲突块内以避免碎片化；`diff3` 额外在 `||||||| base` 与 `=======` 之间输出共同祖先；`zdiff3` 保留完整 ancestor 块，只把双方共同前后缀移出 marker，不做 `merge` 风格的内部共同片段拆分；其它值在需要渲染内容合并时直接报错（退出 128），绝不静默回落。**多 merge base 的合并是例外**：递归虚拟祖先自身的内容依赖该风格（Git 在每一层递归同样传入它），因此该值在合并开始前就被解析——非法值会拦下一个本来会干净完成的交叉合并。该配置同时被 `libra merge`、`libra cherry-pick` 与 `libra revert` 的文本冲突尊重；由 text 回退处理的 NUL 内容与 modify/delete 冲突保持两段式整文件呈现，显式 binary driver 则保留 ours 原文且不写 marker。所有可识别输入行尾均为 CRLF 时，marker 行也使用 CRLF；否则使用 LF。精化只改变呈现，不改变真实冲突结论；唯一例外是双方 postimage 完全相同时可简化为干净结果。`libra rebase` 目前始终渲染无 base 块的整文件标记、不受此配置影响。

### 按路径选择 merge driver（`gitattributes`）

当一个路径的内容在两侧都发生变化时，Libra 通过既有的
`.gitattributes` / `.libra_attributes` 级联读取 `merge` 属性，并按 Git
低层合并语义分派：

- 裸 `merge` 使用普通行级文本三路合并；`-merge` 把文件视为不可拆分整体。
  未解决的 binary 冲突在工作树保留完整 ours 内容，索引写入 stage 1/2/3，
  且不生成冲突标记。
- 具名值先选择同名外部配置；没有外部配置时，`merge=text` 与
  `merge=binary` 选择对应内建 driver，`merge=union` 则在每个重叠区域依次
  保留 ours、theirs。若 xdiff 把 union 的任一输入判为二进制（例如含
  NUL），则回退整文件 binary 冲突，保留 ours 且不写 marker。
- 既没有同名外部配置、也不是已知内建名称时回退 `text`；不报错，也不会
  继续查询 `merge.default`。

路径没有 `merge` 属性时，`merge.default=<name>` 选择默认 driver。未设置
或未知名称回退 `text`；若存在 `merge.<name>.driver` 外部命令，则使用该
命令。Libra 会按严格的 local → global → system 级联一次性读取这些配置。
已配置的外部命令优先于同名内建 driver（因此 `merge=text` 也可被覆盖）；
布尔属性 `merge` 与 `-merge` 始终直接选择内建 text 与 binary driver。

`libra merge` 通过 `sh -c` 执行配置值，以保持 Git 所用的 POSIX 单引号
语义（因此 Windows 也需要在 `PATH` 中提供兼容的 `sh`）。这是受信配置
边界：命令不在 sandbox 中运行，也没有超时限制，因此不要从不可信来源
导入 `merge.*.driver`。占位符如下：

- `%O`、`%A`、`%B`：base、ours、theirs 的不可预测临时文件绝对路径；
  shell 安全路径保持 Git 的无 quote 展开，若路径继承了不安全的工作树目录名，
  则用单引号保护，避免目录名变成 shell 语法；路径使用原生编码字节，不经
  UTF-8 lossy 转换。driver 退出后读回 `%A` 作为结果，空文件也合法。
- `%L`：当前递归深度对应的冲突标记长度。
- `%P`、`%S`、`%X`、`%Y`：工作树相对路径以及 ancestor、ours、theirs
  标签；均按 Git 的单引号规则做 shell quote。`%%` 展开为字面量 `%`。

退出码 0 表示干净合并；1 至 128 表示冲突，并保留 driver 写入 `%A` 的
字节；更高退出码或 shell 无法启动是致命错误，此后 Libra 自身不再发布
由本次合并产生的 HEAD、索引或已跟踪工作树更新。由于 driver 不受沙箱
限制，配置命令直接修改仓库文件或元数据所产生的变化不在 Libra 的回滚
边界内。driver 的 stdout/stderr 会被丢弃；Libra 的错误只给出 driver
名、路径与配置键，不回显配置命令。输入文件位于工作树内 mode
0700 的目录中（文件为私有权限），名称不可预测，并在 driver 返回或子进程
中断后清理；若 Libra 自身在无法展开清理的状态下终止，残留目录仍以 0700
保持私有。`-X ours` / `-X theirs` 不覆盖外部 driver 的结果。递归虚拟祖先
仍调用同一 driver，并使用 `merged common ancestors` / `Temporary merge
branch …` 标签；`merge.<name>.recursive` 尚未实现。

`merge-file`、`cherry-pick`、`revert` 仍共用内建分派，但目前不执行外部
merge driver；`merge-file` 在 Libra 仓库之外没有属性/配置来源，恒用
`text`。

### 输入归一化

默认三方策略支持四个只影响比较的空白选项：`-X ignore-space-change`、
`-X ignore-all-space`、`-X ignore-space-at-eol`、
`-X ignore-cr-at-eol`。它们复用 `libra diff` 的行归一化器。内建
text/union merge 用 canonical 行比较，再回填原文；合并中新选出的行使用
ours 侧行尾。binary 输入和显式 binary driver 不会被改写。这些 xdiff
风格 flag 不改变传给外部 merge driver 的文件。

`merge.renormalize=true` 或 `-X renormalize` 会在内建或外部低层 driver
运行前，根据各路径的 `text` / `eol` 属性 canonicalize 三份内容输入。
`-text` 禁止转换，`text=auto` 转换非 binary 输入，已设置/有值的 `text`
或 `eol=lf|crlf` 启用文本转换。Libra 当前只实现内建行尾转换，不执行任意
clean/smudge filter 程序。合并结果回填原文并使用 ours 侧行尾；
`-X no-renormalize` 显式关闭配置默认值。

`-X` 可重复。最后一个 ours/theirs 选择与最后一个
renormalize/no-renormalize toggle 分别生效；多个空白模式选择最强比较规则
（依次为 `ignore-all-space`、`ignore-space-change`、
`ignore-space-at-eol`、`ignore-cr-at-eol`）。显式 renormalize toggle
会覆盖配置，即使配置值无效。否则只有真正需要三路合并时才严格读取
`merge.renormalize`：无效值会在修改 HEAD/索引/工作树/merge state 前
失败，fast-forward 与 already-up-to-date 不读取它。`libra pull` 的 merge
路径继承该配置，但不公开 `-X`。

### 目录/文件冲突（D/F 冲突）

一侧保留（或修改）**文件** `foo`、另一侧把 `foo` 变成**目录**（删除文件并在 `foo/` 下新增路径）时，两者无法共用一个路径。Libra 遵循 Git recursive 策略（`merge-ort.c` 的 `unique_path`）：目录保留 `foo`，文件按持有它的分支写到唯一名字下——文件在我方时为 `foo~HEAD`，在对方时为 `foo~<branch>`（分支名中的 `/` 替换为 `_`）；该名字已被本次合并的任一输入或其结果占用——无论占用者是文件还是目录，仅 merge base 有过的路径也算，与 Git `unique_path` 检查的集合一致——时追加 `_0`、`_1`……。合并以冲突停止（`LBR-CONFLICT-002`），并先打印 Git 的提示行：

```text
CONFLICT (file/directory): directory in the way of foo from HEAD; moving it to foo~HEAD instead.
```

被移走的文件就是未合并路径：索引在新名字下记录文件所在侧的 stage 2（我方）或 stage 3（对方），若 merge base 在 `foo` 处跟踪的是文件则再记 stage 1；没有 stage 0 条目，非 squash 合并的 `merge-state.json` 在 `conflicted_paths` 中列出新名字。目录内容照常合并。与 Git 相同有两种形态：仅一侧*新增*的文件是纯粹的 file/directory 冲突——原样写到新名字，`--dry-run` 报为 `file-directory`；merge base 已跟踪、且文件侧*修改过*的文件是一个只是换了位置的 modify/delete 冲突——新名字下带 base（stage 1）与修改侧，`--dry-run` 报为 `modify-delete` 并附 `original_path`，同时打印 Git 的第二行（`CONFLICT (modify/delete): foo~HEAD deleted in <branch> and modified in HEAD.  Version HEAD of foo~HEAD left in tree.`），文件按该行所说原样留在新名字下。一侧未动、另一侧换成目录的文件则是普通的干净删除：无冲突、无提示。只含*空* tree 的目录仅在 merge base 在该路径上什么都没有时才算挡路——Git 会把这样的新目录原样采纳；base 已有该路径时 Git 会遍历该目录、发现没有文件，于是文件留在原地（普通 modify/delete）。两种情形均与 `git merge` 对人工构造 tree 的实测一致。`--json`/`--machine` 下不打印这些提示：stdout 保持机器可读，冲突由 stderr 上的错误信封承载。策略选项（`-X ours` / `-X theirs`）只裁决内容 hunk：目录之下的 modify/delete 仍是冲突，与 Git 一致。合并绝不*穿过*工作树里的符号链接写入或删除——被忽略的 `foo -> 别处` 挡在要写的 `foo/…` 前面、或压在合并要删除的已跟踪文件之上时，整个合并在任何改动前被拒绝；恰好占着移位文件名字的符号链接则被文件替换，而*已跟踪*的符号链接 `foo` 让位给目录 `foo/` 时像普通文件一样移到 `foo~HEAD`。只有当前已跟踪的路径才会被删除：碰巧沿用历史名字的未跟踪文件会留下。对于非 squash 合并，若要保留被移走的文件，编辑它、用 `libra add foo~…` 暂存后 `libra merge --continue`；或运行 `libra merge --abort`：它把 `foo` 恢复为合并前的样子，并同时移除被移走的副本与合并创建的目录（合并腾空的目录会被清理，嵌套的 `foo/a/` 不会挡住文件回位）。目前无法「丢弃」被移走的文件：`libra rm` 不接受未合并路径，上述非 squash 恢复方式是 `--abort`（Git 用 `git rm foo~HEAD` 解决这一情形）。squash 冲突仍编辑并暂存同一个移位路径，随后用普通 `libra commit` 收尾；由于未记录 merge 状态，无法使用这些 merge 控制动作。

合并后**空无一物**的目录（其下每个条目都被文件侧删除、另一侧未动）不算挡路：文件留在原路径，与 Git 一致。递归的交叉合并折叠（见上）内部同样适用该规则且不询问用户：文件在虚拟祖先中移到 `foo~Temporary merge branch 1`（或 `2`），与 Git 在 `call_depth > 0` 时完全一致，因此祖先 tree 永远不会在同一名字下同时持有 blob 与子树。

此处只处理两侧**已跟踪**内容之间的冲突。工作树里会被合并覆盖的**未跟踪** `foo` 或 `foo~HEAD` 由既有的 untracked-overwrite 检查预先拒绝；**被忽略**的文件一律视为可弃并被替换——无论它占着这些名字，还是挡在合并要创建的目录位置上（会被替换成目录）；与 `git merge` 的处理完全一致，因此不要把重要内容放在会被合并写入的被忽略路径上。改名已会被检测（见下文「重命名」），但由改名*引发*的碰撞——改名目标正是挡路的目录，或该目录里的文件是改名源——目前仍按普通的「一删一增」处理。

### 子模块（`160000` gitlink 条目）

Libra 定位 monorepo 客户端，永不合并 submodule 内容。三路合并对 gitlink 分两档处理：

- **合并需要对该 gitlink 做裁决**——任一侧记录的 commit id 与 merge base 不同（包括某一侧新增或删除该条目）。合并在**写入任何内容之前**被拒绝（不写 merge state、不动索引与工作树、HEAD 不移动），错误码 `LBR-UNSUPPORTED-001`，消息包含路径：

  ```
  error: merge would have to merge the submodule (gitlink) entry 'vendor': Libra does not support submodules
  ```

  请在 Libra 之外解决 submodule 指针，或从参与合并的分支中移除该 gitlink 条目。

- **三侧记录的 commit id 完全一致**——没有任何决策要做，指针原样写入合并结果。（此前这类条目会被静默丢弃，等于把 submodule 从合并结果树里删掉。）

`libra rebase` 与 `libra cherry-pick` 共用同一道校验与同一措辞，仅把 `merge` 换成 `rebase` / `cherry-pick`。

Libra 仍未实现 `ours` 以外的 merge strategy、上述列表之外的 strategy option，或交互式消息编辑（`--edit`/启动编辑器）。签名验证（`--verify-signatures`）已支持，但仅限本仓库 vault PGP key（无外部 GPG keyring）。

### 重命名

一侧改名、另一侧原地修改同一个文件时，合并把它们当作同一个文件：Libra 对合并的每一侧各跑一次重命名检测（base→ours 与 base→theirs，与 `diff`/`status` 同一套引擎），另一侧的修改会跟随文件落到新路径。冲突同样呈现在新路径上，stage 1 记录 merge base 在*原*路径上的内容。

`merge.renames` 可关闭检测（回退到 `diff.renames`；设为 `false` 时改名重新表现为一删一增），`merge.renameLimit` 限制每侧参与昂贵相似度阶段的新增/删除路径数（回退到 `diff.renameLimit`，默认 **7000**）。`0` 及任何负数都表示「用默认值」而非「不限」。Git 对它真正支持的取值（`0` 与它自己的「未设置」哨兵 `-1`）行为相同，但**小于 `-1` 的值会触发 Git 内部断言并让进程 abort**；Libra 有意不复刻这一崩溃，而是把所有非正值都当作默认值接受。只有非整数才是错误。超限时合并照常完成：精确改名仍会配对，并打印一条 notice 说明其余部分被跳过。Libra 的超限判据是**逐侧**的——任一侧超过阈值即跳过昂贵阶段；Git 则按整个矩阵判断，并且只统计本次合并真正需要的源，因此在某些形态下 Git 仍会继续检测而 Libra 已经停下。

两个键都按**严格**方式读取：无法解析的值会在写入任何东西之前让合并失败——也早于 `--autostash` 保存你的改动。该检查的触发时机与 Git 解析这两个键的时机完全一致：快进、already-up-to-date、`-s ours`，以及**可快进**历史上的 `--squash` / `--no-commit` 都不受该值影响而正常成功；同一历史加 `--no-ff` 则是真合并，会失败。

改名是把同一样东西换个名字，因此两端必须是**同类**条目。如果对侧把原路径上的文件换成了符号链接，改名就用不上它：merge base 仍会跟随改名落到新路径并在那里以 modify/delete 冲突呈现，但那个符号链接**在原路径上幸存**，且**不报 rename/delete**——git 对该形态走独立的 type-change 分支，Libra 与之一致。

「目标被占用」只看**合并后仍然存在**的内容：merge base 有、但两侧都删掉的路径不算占用；对侧只是原样承接自 base 的路径同样不算——改名侧必然已经清空了目标之下的内容，这些路径会被合并删掉。两种情形改名都照常成立，`git merge` 亦然。真正挡路的是对侧在那里**新增或修改**过的内容：它会以自身条目或冲突的形式幸存下来。

改名目标嵌在该文件*原路径之下*（`old` 改名为 `old/new`）并不构成冲突：改名本身就腾出了这个名字，因此改名成立，另一侧的修改跟随文件落到 `old/new`。`git merge` 与 `git merge-tree --messages` 对该形态同样干净合并，不打印 `CONFLICT (file/directory)`。

两侧把同一文件改名到**同一路径**根本不是冲突：merge base 跟随该文件落到新路径，两侧内容在那里正常三路合并，因此双方一致的改名可以干净合并。

其余形态是 Git 的路径级改名冲突：

* **rename/rename**——两侧把同一文件改名到*不同*路径。两个目标都保留，通常写入**同一份**合并结果；部分未解决的二进制冲突会让两个目标各保留本侧原条目：仅当未干净合并结果的 hash 和 mode 均等于 ours 原条目时，才触发 Git 的此项回退。选择内容不会丢弃独立合并出的可执行位。合并报告 `CONFLICT (rename/rename): <old> renamed to <a> in HEAD and to <b> in <branch>.`，**原路径按删除收口**。git 则把 merge base 以未合并状态留在原路径的 stage 1——git 自己的源码注释说那是只为兼容其回归测试而保留的历史行为、「一致的做法」是删掉它。Libra 采纳一致的做法。保留它还会让该冲突脱离常规暂存流程——原路径没有工作区文件，而 `libra add`、`libra rm`、`libra restore --staged` 都按已暂存条目匹配路径，没有一条够得着它。它并非**不可解决**：`libra read-tree HEAD` 后接 `libra add -A .`，或 `libra update-index --cacheinfo`，都能清掉；但两者都很粗暴——前者整体替换索引、丢弃此前已暂存的全部解决成果，后者属 plumbing 层。
* **rename/delete**——一侧改名、另一侧删除。新路径保留改名侧的内容，base 记在其旁，合并报告 `CONFLICT (rename/delete): <old> renamed to <new> in HEAD, but deleted in <branch>.` 即使改名没有改动任何内容，这仍然是冲突。
* **rename/add**，以及改名目标被另一次改名占用——先跑改名自身的三路合并，其结果再与对侧放在该路径上的内容相撞，于是目标路径以**无共同祖先的 add/add** 冲突呈现。当改名自身的合并不干净时，还会额外报告 `CONFLICT (rename involved in collision): rename of <old> -> <new> has content conflicts AND collides with another path; this may result in nested conflict markers.`

改名相关合并写出的冲突标记比普通内容冲突**长一个字符**，且每一侧的标签为 `<分支>:<路径>` 而非仅分支名——只有真正做了改名的那一侧用目标路径，另一侧仍用源路径。两者均与 Git 一致。`merge.conflictStyle=diff3` 下共同祖先的标记同样带源路径限定；Libra 写作 `base:<路径>`，沿用其 diff3 输出一贯的 `base` 标签，而 Git 在该位置写的是祖先提交的缩写，这是有意保留的输出差异。

最外层合并会推断目录级改名：旧目录下所有已跟踪路径都已移走，且某个目标目录获得唯一最高的文件改名票数时，对侧在旧目录下的**新增**路径会跟随目录改名落到新目录；对 merge base 已有路径的修改，本来就由普通逐文件改名带到新路径。最高票平局属于目录改名分裂：新增路径以已解决的 stage 0 留在旧名，但合并停下让用户确认布局；可直接 `merge --continue` 提交该布局，也可 `merge --abort` 恢复旧 HEAD。

`merge.directoryRenames=false` 关闭推断；`true` 自动移动受影响的新增路径并报告 `Path updated: ...`；默认值 `conflict` 会把路径移到建议位置、保留为未合并路径，并报告 `CONFLICT (file location): ...`。建议目标另有独立内容时，普通 add/add 冲突会保住两侧。关闭 `merge.renames` 也会一并关闭目录推断。

Libra 接受 `conflict`，以及 `true`/`false` 的 Git 兼容布尔拼写（包括 `yes`/`no`、`on`/`off` 与数值布尔）；不属于这三种逻辑模式的值会在仓库写入前以 `LBR-REPO-003` 拒绝。这是有意的 fail-closed 安全差异：Git 当前会忽略未知的 `merge.directoryRenames` 值并保留默认行为。JSON/machine 模式不打印上述人读消息；建议落位或分裂的预演以 `directory-rename` 作为冲突种类。

### 会改变历史的 merge 默认值

未传对应 CLI 标志时，Libra 按 local → global → system 级联读取 Git 兼容默认值：`merge.ff=true|false|only` 分别允许快进、强制 merge commit、仅允许快进（`--ff`/`--no-ff`/`--ff-only` 优先；`only` 与 `--ff-only` 只允许单头可快进历史，非 already-up-to-date 的 octopus 永不快进）；`merge.log=true|false|<n>` 在自动生成的 merge 消息中追加最多 20 条或 `<n>` 条目标侧提交 subject。`--log[=<n>]` / `--no-log` 覆盖配置并 last-one-wins，bare `--log` 为 20；显式 `-m` 会抑制仅来自配置的 `merge.log`，但显式 `--log` 仍会把 shortlog 追加到自定义消息。非 squash 合并将解析后的消息记录进 merge state，冲突或 `--no-commit` 后用 `merge --continue` 收尾时原样提交；squash 不记录 merge state，提交消息由随后普通 `libra commit` 提供；`merge.verifySignatures=true|false` 控制 tip 签名验证（正反 CLI 标志优先），验证在每个已解析目标上、任何变更（包括 autostash 创建）之前执行——被拒绝的 merge 不写任何内容（无 stash 条目、无对象）。无效或不可读的 local/global 值在修改 HEAD/index/工作树/merge state 前失败：无法解析的值，`merge.ff` 与 `merge.verifySignatures` 报 `LBR-CLI-002`，`merge.autostash`、`merge.conflictStyle`、`merge.renames`、`merge.renameLimit`、`merge.directoryRenames`、`merge.renormalize` 报 `LBR-REPO-003`；完全读不出来的值报 `LBR-IO-001`；local/global 加密值先解密，不可读或不支持的 system scope 跳过。例外：schema 比当前 Libra 二进制更新的全局配置库会在一次性去重警告后被跳过而不失败（见 `LBR-CONFIG-001`）。`merge.renormalize` 的阶段性读取时机见上文。

### `--dry-run`（Libra 扩展）

`libra merge --dry-run <branch>...` 预演合并结果而**不写任何东西**——不动 HEAD、索引、工作树、reflog、merge 状态与对象库（自动合并的 blob 仅在内存中计算）。因为只读，只要没有进行中的 merge 状态且索引没有未解决条目，脏工作树也可预演。入口检查仍适用于 `--dry-run`，包括未解决的 squash 冲突；除此之外，预演不校验工作树干净度，真实合并仍可能拒绝。结果：fast-forward / 已最新 / 干净三方或 octopus 合并 → 退出 0；会冲突 → 输出 `Would conflict in: <paths>` 并退出 1（结果信号，非真实冲突的 128）。`--json` 下带 `"dry_run": true`（冲突时另有 `"would_conflict": true`、`conflicted_paths` 与 `conflict_kinds`——每个冲突路径一个 `{"path", "kind", "original_path"?}` 对象，`kind` 为 `content` / `modify-delete` / `file-directory` / `rename-rename` / `directory-rename`；`rename-rename` 报告 rename/rename(1to2) 的两个目标，`directory-rename` 报告建议落位或目录分裂。`original_path` 仅目录/文件移位时出现并记录原路径，此时 `path` 是文件将被写到的带 `~` 后缀的名字），真实合并的输出不含这些键（schema 冻结）。

### `--restart`（Libra 扩展，移植 Lore `branch merge restart`）

`libra merge --restart` 一步「推倒重来」：像 `--abort` 一样恢复合并前状态（**丢弃**已做的冲突解决），随后立刻对**记录的目标提交**重跑同一个合并（即使分支已移动也确定重现），重新生成冲突标记与 merge 状态。recovery-critical 的 `--allow-unrelated-histories` 会重放；原 `-m`/`--no-ff` 等展示/策略选项不重放。要求**有冲突的非 squash** 合并：squash 没有可重启的 merge 状态；对已暂存的 `--no-commit` 干净合并会拒绝（用 `--continue` 完成或 `--abort` 丢弃）；无合并进行中时报错（均退出 128）。

## 选项

| 选项 | 说明 |
|--------|-------------|
| `<branch>...` | 一个或多个目标分支、提交或远程跟踪引用；两个及以上进入原子 octopus 路径。 |
| `-m, --message <MSG>` | 覆盖合并提交消息（默认 `Merge <branch> into <head>`）。也可与 `--continue` 同用，覆盖冲突合并开始时记录的消息——这是 Libra 扩展：Git 的 `--continue` 不接受参数，而 Libra 的 merge 从不打开编辑器。 |
| `--ff` | 允许可行的快进，覆盖 `merge.ff=false|only`。 |
| `--ff-only` | 仅当单头合并可快进时才合并；非 already-up-to-date 的 octopus 永不快进，因此会被拒绝。 |
| `--no-ff` | 单头可快进时仍强制生成 merge commit；octopus 本来就总会生成。 |
| `-s ours`, `--strategy=ours` | 以 HEAD 和每个非冗余目标作为 parents 记录合并关系，但完整保留当前 HEAD tree；不同于 `-X ours`。其它 strategy 被拒绝。 |
| `-X <option>`, `--strategy-option=<option>` | 接受 `ours`、`theirs`、`ignore-space-change`、`ignore-all-space`、`ignore-space-at-eol`、`ignore-cr-at-eol`、`renormalize` 或 `no-renormalize`。可重复；favor 与 renormalize toggle 各自 last-one-wins，空白模式选择最强规则；不能与 `-s ours` 组合。 |
| `--allow-unrelated-histories` | 以虚拟空 merge base 允许没有共同祖先的历史；非 squash 冲突的 `--restart` 会保留此许可。 |
| `--log[=<N>]` | 向 merge 消息追加最多 N 条目标侧 subject；bare `--log` 为 20。覆盖 `merge.log`，并可追加到显式 `-m`；与 `--no-log` last-one-wins。 |
| `--no-log` | 禁用 merge 消息 shortlog，覆盖 `merge.log` 和更早的 `--log`。 |
| `--squash` | 生成合并后的索引/工作树，但不创建提交、不移动 HEAD、不记录 merge 状态，即使发生冲突也如此。解决并暂存冲突后，用普通 `libra commit` 创建单亲提交；`--continue`、`--abort`、`--restart` 均报 `no merge in progress`。 |
| `--no-commit` | 执行合并但停在提交之前。干净 octopus 状态保留全部目标供 `--continue`/`--abort`；octopus 冲突原子拒绝且不创建状态。单头冲突仍保留可解决状态。 |
| `--no-verify` | 本次 merge 跳过全部 `.libra/hooks`；与 `--continue` 一起使用时绕过待执行的 commit/消息/post hooks。 |
| `--no-edit` | 接受自动生成的合并消息而不启动编辑器。Libra 从不为 merge 打开编辑器，故此为对齐 Git 而接受的 no-op。 |
| `--stat` | 合并完成后显示 diffstat（合并前 HEAD 与新提交之间的变更）。Git 默认显示；Libra 默认不显示，故用 `--stat` 主动开启。与 `--no-stat`/`-n` 构成 last-wins 切换。仅人类输出。 |
| `-n`, `--no-stat` | 合并结束时不显示 diffstat（Libra 默认）。与 `--stat` 构成 last-wins 切换。 |
| `--no-progress` | 不显示进度条。为对齐 Git 而接受的 no-op：Libra 的 merge 从不渲染进度条。 |
| `--verify-signatures` | 验证每个目标 tip 的 PGP 签名，任一未签名或签名无效都在变更前中止；覆盖 `merge.verifySignatures`。仅能验证本仓库 vault PGP key 所签。 |
| `--no-verify-signatures` | 不验证被合并提交的签名，覆盖 `merge.verifySignatures=true`；与正向标志 last-wins。 |
| `--no-rerere-autoupdate` | 为对齐 Git 而接受。rerere 已集成：`rerere.enabled` 开启时，冲突合并会记录每个冲突的 preimage 并在有匹配记录时回放已保存的解法；回放文件是否自动暂存跟随 `rerere.autoUpdate` 配置。逐次调用的覆盖未实现——暂存始终跟随配置。（Git 的正向 `--rerere-autoupdate` 未公开。） |
| `--no-gpg-sign` | 不对合并提交 GPG 签名。为对齐 Git 而接受的 no-op：Libra 的 merge 从不签名。（Git 的 `-S`/`--gpg-sign` 未实现。） |
| `--continue` | 在冲突已解决并暂存后完成进行中的非 squash 合并，使用 merge 状态记录的 parent 集合。squash 没有可继续的 merge 状态。 |
| `--abort` | 恢复进行中的非 squash 合并开始前的 HEAD、索引和工作树。`--squash` 后不可用。 |
| `--autostash` / `--no-autostash` | 合并前保存本地 tracked 变更，并在结束时分别恢复 staged index 与 unstaged worktree 层；非 squash 冲突期间 held 在 `stash list` 之外，直到 `--continue`/`--abort`。干净的 squash 在命令返回时尝试恢复；冲突 squash 则直接把 autostash 保存进 `stash list`，不执行回贴，保留未解决的索引和工作树。解决并提交 squash 后再运行 `libra stash pop`；保存失败时警告并保留 held sidecar 对原改动的引用。恢复冲突会先保存到普通 stash list 并提示，变更不会丢失。配置项为 `merge.autostash`（布尔；无效值硬错误）；不保存 untracked 文件。`--json` 增加 `autostash: applied\|stashed\|kept`。 |
| `--dry-run` | Libra 扩展：预演合并结果而不写任何东西（见上文）。干净预演退出 0，会冲突退出 1。与 `--continue`/`--abort`/`--restart`/`--squash`/`--no-commit` 互斥。 |
| `--restart` | Libra 扩展：对有冲突的非 squash 合并，像 `--abort` 一样恢复合并前状态（丢弃解决工作）后，立刻对记录的目标提交重跑同一合并（见上文）。不接受分支与合并选项。 |
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
libra merge topic-a topic-b topic-c
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

未使用 `--squash` 的合并（包括 `--no-commit`）发生冲突时：

1. 编辑包含冲突标记的文件。
2. 使用 `libra add <path>` 暂存每个已解决路径。
3. 运行 `libra merge --continue` 创建合并提交。

在继续之前运行 `libra merge --abort` 可将分支、索引和工作树恢复到合并前提交。当存在 merge 状态时，`libra status` 会显示进行中的合并目标，以及 continue/abort 命令。

上述冲突解决流程只适用于单头合并。octopus 内容冲突会被原子拒绝且不创建 merge 状态，因此不能使用 `--continue`、`--abort` 或 `--restart`；请逐个合并这些目标。以 `--no-commit` 停下的干净 octopus 会创建状态，可正常继续或中止。

`--squash` 冲突会留下未解决的索引 stage 和工作树结果，保持 HEAD 不变，但不创建 merge 状态。解决文件、用 `libra add <path>` 暂存每个已解决路径后，运行普通 `libra commit` 创建单亲提交。`libra merge --continue`、`--abort`、`--restart` 均以 `no merge in progress` 拒绝（`LBR-REPO-003`，退出 128），上面的非 squash 恢复步骤不适用。若启用了 `--autostash`，本地改动直接保存进 `stash list`，未解决的索引和工作树保持原样；解决并提交 squash 后，再用 `libra stash pop` 恢复本地改动。

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

`-s ours` 使用 `strategy: "ours"` 和 `files_changed: 0`。默认多目标合并使用 `strategy: "octopus"`；`parents` 依次包含 `HEAD` 与命令行顺序下的每个非冗余目标。已经最新的合并使用 `strategy: "already-up-to-date"`、`commit: null`、`files_changed: 0` 和 `up_to_date: true`。

`--abort` 设置 `aborted: true`；`--continue` 设置 `continued: true`。冲突失败会在 stderr 上返回带有 `LBR-CONFLICT-002` 的错误信封。 `--dry-run` 额外带 `dry_run`、`would_conflict`、`conflicted_paths` 与 `conflict_kinds`（每个冲突路径一个 `{"path", "kind", "original_path"?}` 对象，`kind` 为 `content`、`modify-delete`、`file-directory`、`rename-rename` 或 `directory-rename`；`rename-rename` 报告 rename/rename(1to2) 的两个目标，`directory-rename` 报告建议落位或目录分裂。`original_path` 仅在目录/文件移位时出现，记录文件原来的路径，此时 `path` 与 `conflicted_paths` 里都是带 `~` 后缀的目标名）。

## 参数对比：Libra vs Git vs jj

| 参数 | Libra | Git | jj |
|-----------|-------|-----|----|
| 分支目标 | `<branch>...`（一个或多个） | `<commit>...`（一个或多个） | N/A（使用 `jj new`） |
| 快进 | 支持 | 支持 | N/A |
| 单头三方合并 | 支持 | 支持 | N/A |
| 交叉合并（多个 merge base） | 递归虚拟祖先（折叠顺序：object id 升序；最大深度 20，每层最多 32 个 base） | 递归虚拟祖先（`-s recursive`/`ort`，深度无上限） | N/A |
| Continue / abort | `--continue`, `--abort`（需要非 squash merge 状态） | `--continue`, `--abort`（需要非 squash merge 状态） | N/A |
| Octopus merge | 支持；冲突原子拒绝，parents 经归约并保持顺序 | 支持 | N/A |
| 仅快进 | `--ff-only` | `--ff-only` | N/A |
| 强制合并提交 | `--no-ff` | `--no-ff` | N/A |
| Squash | `--squash`；解决/暂存冲突后普通 `commit`（单亲） | `--squash`；解决/暂存冲突后普通 `commit`（单亲） | N/A |
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
| 空白感知内容合并 | `-X ignore-space-change` / `ignore-all-space` / `ignore-space-at-eol` / `ignore-cr-at-eol` | 相同 | N/A |
| 归一化内容合并 | `merge.renormalize` / `-X renormalize` / `-X no-renormalize` | 相同 | Libra 不运行任意 clean/smudge filter |
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
| 真正三路合并读取到无效 `merge.renormalize` 值 | `LBR-REPO-003` | 128 |
| `--verify-signatures`：tip 未签名、签名无效或 vault 不可用 | `LBR-REPO-003` | 128 |
| 合并冲突 | `LBR-CONFLICT-002` | 128 |
| Octopus 冲突（原子拒绝，不创建 merge 状态） | `LBR-CONFLICT-002` | 128 |
| 无 merge 状态但索引有未解决条目，包括目标已最新或 `--dry-run` | `LBR-CONFLICT-002` | 128 |
| 脏工作树或暂存更改 | `LBR-CONFLICT-002` | 128 |
| 未跟踪文件会被覆盖 | `LBR-CONFLICT-002` | 128 |
| 合并已在进行中 | `LBR-CONFLICT-002` | 128 |
| 对 `--continue` / `--abort` / `--restart` 没有进行中的合并（包括 `--squash` 后） | `LBR-REPO-003` | 128 |
| `--continue` 仍有未解决的冲突 stage | `LBR-CONFLICT-002` | 128 |
| 无法读取 merge 状态或索引 | `LBR-IO-001` | 128 |
| 无法保存状态、索引、树、提交、HEAD 或工作树 | `LBR-IO-002` | 128 |
