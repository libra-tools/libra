# 参数覆盖表（按命令组）

### `libra init` 参数覆盖表

| 参数 | 场景 ID | 关键断言 |
|---|---|---|
| `DIRECTORY` | `cli.init-directory-and-quiet` | 目标目录和 `.libra/libra.db` 被创建 |
| `-q` / `--quiet` | `cli.init-directory-and-quiet` | 成功但不输出普通 banner |
| `-b` / `--initial-branch` | `cli.init-branch-and-format-options` | 初始分支可通过公开命令观察 |
| `--object-format` | `cli.init-branch-and-format-options` | `core.objectformat` 为 `sha1` / `sha256`，非法值失败 |
| `--ref-format` | `cli.init-branch-and-format-options` | `core.initrefformat` 为 `strict` / `filesystem`，非法值失败 |
| `--bare` | `cli.init-bare-and-shared` | 存储根为目标目录本身，无普通 `.libra/` 工作区布局 |
| `--shared` | `cli.init-bare-and-shared` | 支持值成功，非法值失败并提示支持值 |
| `--template` | `cli.init-template` | 模板内容复制到 Libra 存储根，缺失路径失败 |
| `--from-git-repository` | `cli.init-from-git-repository` | 本地 Git fixture 的文件/提交/ref 可通过 Libra CLI 观察 |
| `--vault` | `cli.init-vault` | `vault.db` 与 `vault.signing` 状态符合显式 bool |



### `libra status/add/commit/log` 参数覆盖表

| 参数或子命令 | 场景 ID | 关键断言 |
|---|---|---|
| `status` | `cli.commit-status-log` | 默认状态可执行，干净/dirty 状态可观察 |
| `status --short` | `cli.commit-status-log` | untracked 或 staged path 以短格式出现 |
| `status --porcelain` | `cli.commit-status-log` | 输出适合脚本断言的机器可读状态 |
| `status --exit-code` | `cli.commit-status-log` | 干净为 0，dirty 为非 0 |
| `add <pathspec>` | `cli.commit-status-log` | 指定文件被加入 index 并可由 status 观察 |
| `add --dry-run` | `cli.commit-status-log` | 预览输出不改变 index |
| `commit -m` | `cli.commit-status-log` | 提交消息进入 log |
| `commit -F` | `cli.commit-status-log` | 从文件读取提交消息 |
| `commit -a` | `cli.commit-status-log` | 已跟踪文件修改被自动暂存并提交 |
| `commit --allow-empty` | `cli.commit-status-log` | 空提交成功并出现在 log 中 |
| `commit --amend --no-edit` | `cli.commit-status-log` | 最后一个提交被替换且消息复用 |
| `commit --conventional` | `cli.commit-status-log` | 非 conventional 消息失败且不写入提交 |
| `commit --signoff` | `cli.commit-status-log` | 提交消息包含 Signed-off-by trailer |
| `log --oneline` | `cli.commit-status-log` | 输出短 hash 和提交主题 |
| `log -n` | `cli.commit-status-log` | 输出数量受限制 |
| `log --author` / `--grep` | `cli.commit-status-log` | 只返回匹配作者或消息的提交 |
| `log --name-status` / `--stat` | `cli.commit-status-log` | 文件变化摘要可观察 |



### `libra branch/switch/checkout` 参数覆盖表

| 参数或子命令 | 场景 ID | 关键断言 |
|---|---|---|
| `branch <name>` | `cli.branch-switch-checkout` | 从 HEAD 创建本地分支 |
| `branch <name> <commit>` | `cli.branch-switch-checkout` | 从指定 base 创建分支 |
| `branch --list` | `cli.branch-switch-checkout` | 已创建分支可列出 |
| `branch --show-current` | `cli.branch-switch-checkout` | 当前分支名可观察 |
| `branch -m <old> <new>` | `cli.branch-switch-checkout` | 分支重命名后新名可用、旧名不可用 |
| `branch -d` / `branch -D` | `cli.branch-switch-checkout` | 安全删除和强制删除路径均覆盖 |
| `switch <branch>` | `cli.branch-switch-checkout` | 切换到现有分支 |
| `switch -c <branch> <start>` | `cli.branch-switch-checkout` | 创建并切换到新分支 |
| `switch --detach <commit>` | `cli.branch-switch-checkout` | HEAD 进入 detached 状态 |
| `checkout <branch>` | `cli.branch-switch-checkout` | 兼容分支切换路径可用 |
| `checkout -b <branch>` | `cli.branch-switch-checkout` | 兼容创建并切换路径可用 |
| `checkout -- <pathspec>` | `cli.branch-switch-checkout` | 路径恢复行为可观察 |



### `libra diff/restore/reset` 参数覆盖表

| 参数或子命令 | 场景 ID | 关键断言 |
|---|---|---|
| `diff <pathspec>` | `cli.restore-reset-diff` | unstaged 工作区修改可见 |
| `diff --staged` | `cli.restore-reset-diff` | staged 修改可见 |
| `diff --old --new` | `cli.restore-reset-diff` | 两个 revision 间差异可见 |
| `diff --name-only` / `--name-status` | `cli.restore-reset-diff` | 文件名和状态摘要可用于脚本断言 |
| `diff --stat` / `--numstat` | `cli.restore-reset-diff` | 文件级统计输出可见 |
| `diff --raw -z` | `cli.restore-reset-diff`, `compat_diff_review_options` | NUL-safe mode/object/status 记录；rename 分离旧/新路径字段，工作树侧 ID 为零 |
| `diff --compact-summary` | `cli.restore-reset-diff`, `compat_diff_review_options` | 隐含 stat；create/delete 与 executable/symlink mode 注记可观察 |
| `diff --diff-filter=<FILTER>` | `cli.restore-reset-diff`, `compat_diff_review_options` | include/exclude/`*` all-or-none，非法值输出前 fail-closed，sparse-view 后重新判定 |
| `diff -S <STRING>` / `-G <REGEX>` | `cli.restore-reset-diff`, `compat_diff_review_options`, `ai_libra_vcs_safety_test` | 每 file pair literal 次数变化 / 增删 hunk 行正则过滤；textconv 结果一次复用，external driver 前过滤，无效 regex pre-progress fail-closed，AI 默认过滤器审批边界不放宽 |
| `diff --color-words` | `cli.restore-reset-diff`, `command_test::test_diff_word_diff_modes`, `ai_libra_vcs_safety_test` | bare shorthand 进入 word color mode；Auto 下重定向 stdout 仍着色，全局 `--color=never` 抑制 ANSI，plain mode 不受 tty/forced color 污染；regex-valued shorthand 显式拒绝 |
| `diff --full-index --src-prefix --dst-prefix` | `cli.restore-reset-diff`, `compat_diff_review_options` | patch 使用完整对象 ID 与 CLI 指定前缀；`-R` 交换前缀 |
| `diff --output <file>` | `cli.restore-reset-diff` | patch 写入文件，stdout 不输出 hunk |
| `diff --algorithm=histogram` | `cli.restore-reset-diff` | 当前唯一实现算法可用，其他算法负向断言 |
| `restore --staged <path>` | `cli.restore-reset-diff` | index 恢复到 HEAD，工作区保持修改 |
| `restore --worktree <path>` | `cli.restore-reset-diff` | 工作区文件恢复到 index 或 source 内容 |
| `restore --source <rev>` | `cli.restore-reset-diff` | source revision 可恢复文件；不存在时失败且不改写文件 |
| `restore --overlay` | `cli.restore-reset-diff` | 已实现：overlay 模式仅创建/更新 source 中的路径、不移除 source 中缺失的已跟踪路径，runner 正向断言成功 |
| `restore --no-overlay` | `cli.restore-reset-diff` | 已实现：与 `--overlay` 构成 last-wins 切换，默认行为（移除 source 中缺失路径），runner 正向断言成功 |
| `restore --pathspec-from-file` | `cli.restore-reset-diff` | 已实现；负向步骤因 pathspec 文件缺失而失败（非「未实现」），runner 负向断言 |
| `reset <path>` / `reset HEAD -- <path>` | `cli.restore-reset-diff` | 路径级 reset 只取消暂存；revision/path 同名时用 `--` 消歧 |
| `reset --soft` | `cli.restore-reset-diff` | 只移动 HEAD，保留 index/工作区 |
| `reset --mixed` | `cli.restore-reset-diff` | 移动 HEAD 并重置 index |
| `reset --hard` | `cli.restore-reset-diff` | HEAD、index、工作区全部回到目标 revision |
| `reset --pathspec-from-file` | `cli.restore-reset-diff` | 已实现；负向步骤因 pathspec 文件缺失而失败（非「未实现」） |
| `reset --merge` / `--keep` | `cli.restore-reset-diff`, `compat_noninteractive_history_controls` | runner 验证公开正向入口；Cargo compat 精确固定安全本地变更保留、受影响路径覆盖拒绝、HEAD/index/worktree 原子不变 |



### `libra stash/bisect/worktree` 参数覆盖表

| 参数或子命令 | 场景 ID | 关键断言 |
|---|---|---|
| `stash push -m` | `cli.stash-bisect-worktree` | tracked 修改被保存，消息可在列表中观察 |
| `stash push -u` / `--all` / `--keep-index` | `cli.stash-bisect-worktree` | 当前未实现，runner 负向断言稳定错误 |
| `stash list` / `stash show` | `cli.stash-bisect-worktree` | stash 条目和文件级摘要可观察 |
| `stash apply` | `cli.stash-bisect-worktree` | 修改恢复但 stash 条目保留 |
| `stash pop` | `cli.stash-bisect-worktree` | 修改恢复且 stash 条目删除 |
| `stash clear --force` | `cli.stash-bisect-worktree` | 非交互清空 stash 列表 |
| `bisect start <bad> --good <good>` | `cli.stash-bisect-worktree` | 二分边界可初始化 |
| `bisect bad` / `bisect good <rev>` | `cli.stash-bisect-worktree` | 会话状态推进并可由 log/view 观察 |
| `bisect log` / `bisect view` | `cli.stash-bisect-worktree` | 当前会话和候选状态可输出 |
| `bisect reset` | `cli.stash-bisect-worktree` | 结束会话并恢复原 HEAD |
| `bisect start <bad> <good1> <good2>` | `cli.stash-bisect-worktree` | 当前未支持 positional multi-good，runner 负向断言 |
| `worktree add <path>` | `cli.stash-bisect-worktree` | linked worktree 被创建、登记，并从当前 HEAD 填充工作区 |
| `worktree add --no-checkout` | `cli.stash-bisect-worktree` | 当前未实现，runner 负向断言稳定错误 |
| `worktree list` / `--json worktree list` | `cli.stash-bisect-worktree` | 主 worktree 和 linked worktree 均可列出，JSON envelope 可解析 |
| `worktree lock --reason` / `unlock` | `cli.stash-bisect-worktree` | 锁状态和 reason 可观察并可解除 |
| `worktree move <src> <dest>` | `cli.stash-bisect-worktree` | 登记路径和目录路径同步移动 |
| `worktree remove <path>` | `cli.stash-bisect-worktree` | 默认注销登记但保留目录 |
| `worktree remove --delete-dir <path>` | `cli.stash-bisect-worktree` | dirty linked worktree 被拒绝；清理后可删除目录 |
| `worktree prune` | `cli.stash-bisect-worktree` | 清理目录缺失的 stale 登记并输出被清理路径 |



### `libra tag/history-inspection/worktree-tools/ref-log` 参数覆盖表

| 参数或子命令 | 场景 ID | 关键断言 |
|---|---|---|
| `tag <name>` / `tag -m <msg>` | `cli.tag-basic` | 轻量和 inline-message annotated tag 可创建、列出、解析 |
| `tag -F <file>` | `cli.tag-basic` | 当前未实现，runner 负向断言稳定错误 |
| `tag -l` / `tag -l -n` | `cli.tag-basic` | 列表和注释摘要覆盖 |
| `tag -f` / `tag -d <name>` / JSON delete | `cli.tag-basic` | 强制更新、删除、JSON 删除和缺失 tag 错误覆盖 |
| `merge <branch>` | `cli.merge-rebase-cherry-revert-smoke` | fast-forward 与三方无冲突 merge 均可观察 |
| `merge -s ours` | `compat_noninteractive_history_controls` | 双父 merge commit 且 tree 与合并前 HEAD 完全一致；JSON strategy=`ours` |
| `merge -X ours/theirs` | `compat_noninteractive_history_controls` | 只偏向冲突 hunk，目标侧 clean hunk 保留；两组 parent 链固定 |
| `merge --allow-unrelated-histories` | `compat_noninteractive_history_controls` | 默认拒绝；显式允许后虚拟空 base clean merge，冲突可 restart→resolve→continue |
| `merge --log[=<n>]` / `--no-log` | `compat_noninteractive_history_controls` | last-wins；显式 `-m --log=1` shortlog 跨 conflict→continue 原样保留 |
| `merge --find-renames[=<n>]` | `cli.merge-rebase-cherry-revert-smoke` | 当前未实现，runner 负向断言稳定错误 |
| `merge --squash --continue` | `cli.merge-rebase-cherry-revert-smoke` | 与 lifecycle action 组合被拒绝 |
| `merge --continue` / `--abort` | `cli.merge-rebase-cherry-revert-smoke` | 无会话时明确失败；冲突续跑场景另行补充 |
| `rebase <upstream>` | `cli.merge-rebase-cherry-revert-smoke` | topic 提交重放到新 base |
| `rebase --continue` | `cli.merge-rebase-cherry-revert-smoke` | 无会话时明确失败；冲突续跑场景另行补充 |
| `rebase --autostash` / `--no-autostash` | `compat_noninteractive_history_controls` | tracked dirty state 在成功和 conflict→abort 后精确恢复，sidecar 终态清理 |
| repeatable `rebase --exec <cmd>` | `compat_noninteractive_history_controls` | 每提交执行、required sandbox 越界写 fail-closed、失败→continue 重试、exec-created commit 不丢失 |
| `rebase --update-refs` / `--no-update-refs` | `compat_noninteractive_history_controls` | captured-tip 原子移动、linked-worktree checkout 排除、skip/start-empty rewrite 映射 |
| `rebase --fork-point` / `--no-fork-point` | `compat_noninteractive_history_controls` | force-moved upstream reflog 只 replay topic commit，parent 链落到新 upstream |
| `cherry-pick <commit>` / `cherry-pick -x <commit>` | `cli.merge-rebase-cherry-revert-smoke` | 指定提交修改被重放到当前分支；默认消息不追加来源行，`-x` 追加来源提交行 |
| repeatable `cherry-pick -X ours/theirs` | `compat_noninteractive_history_controls` | last-wins，仅选择冲突 hunk，clean hunk 与 parent 链固定 |
| `revert <commit>` / `A..B`, `revert --continue` / `--abort` | `cli.merge-rebase-cherry-revert-smoke` | 单提交反向提交覆盖；范围回滚和空会话控制为负向断言 |
| repeatable `revert -X ours/theirs` | `compat_noninteractive_history_controls` | last-wins，仅选择冲突 hunk，clean hunk 与 parent 链固定 |
| `revert --cleanup=<mode>` | `compat_noninteractive_history_controls` | cleanup 选择跨 conflict→continue 持久化，scissors 后的内容不进入最终提交消息 |
| `grep` / `grep -F/-i/-n/-c/-l/-L/-e/-f/--tree/--cached` | `cli.grep-blame-describe-shortlog` | 工作区、index、pathspec、pattern file 和历史 tree 搜索可观察 |
| `grep -z` / `grep --untracked` / grep 0/1/2 exit codes | `cli.grep-blame-describe-shortlog` | 已实现；runner 正向断言 NUL 路径输出、未跟踪文件搜索，并负向断言无匹配退出 1、命令错误退出 2 |
| `blame` / `blame -L` / `blame <file> <commit>` / `blame --porcelain` | `cli.grep-blame-describe-shortlog` | 行级作者、提交、范围限制和 porcelain 头部可观察 |
| `describe --tags/--always/--abbrev/--exact-match/--dirty[=<mark>]` | `cli.grep-blame-describe-shortlog` | tag 描述、hash fallback、exact match 和 tracked dirty 后缀可观察；HEAD 越过 tag 后的 `--exact-match` 为负向断言 |
| `shortlog` / `shortlog -s` / `shortlog -n` / `shortlog -e` | `cli.grep-blame-describe-shortlog` | 作者汇总、排序、邮箱和 revision 限制可观察；扩展 flags 为负向断言 |
| `rev-parse HEAD` / `--short` / `--show-toplevel` | `cli.object-readback` | 完整哈希、短哈希和工作树根路径可传递给后续 plumbing 命令 |
| `rev-parse --verify` / `--verify --short` / `--default` | `cli.object-readback` | 单对象断言、短哈希断言、默认 revision 回退和 quiet 失败退出 1 可观察 |
| `show --no-patch` / `--stat` / `<rev>:<path>` / `<blob>` | `cli.object-readback` | commit 元数据、统计、历史文件内容、文本 blob 与 binary blob 元数据可观察 |
| `show-ref --head` / `--no-head` / `--heads` / `--branches` / `--no-branches` / `--tags` / `--no-tags` / `--hash[=<n>]` / `--no-hash` / `--abbrev[=<n>]` / `--no-abbrev` / `--dereference` / `--no-dereference` / `--exists` / `--no-exists` / `--verify` / `--no-verify` / `--exclude-existing[=<pattern>]` / pattern | `cli.object-readback`, `cli.show-ref-exclude-existing` | HEAD/分支引用可列出且 reset aliases 可恢复默认范围，annotated tag peel、完整 refname 存在性、精确验证、hash-only/abbrev 输出、stdin filter、pattern 过滤和缺失 ref 退出码可观察 |
| `for-each-ref --points-at <object>` | `cli.object-readback` | branch、lightweight tag 和 annotated tag peeled target 过滤可观察，JSON envelope 可用 |
| `rev-list HEAD` / multi revision / `A..B` / `^A` / `A...B` / `-n` / `--max-count` / `--skip` / `--count` / `--since` / `--after` / `--until` / `--before` / `--first-parent` / `--author` / `--committer` / `--grep` / `-- <path>` / `--left-right` / `--left-only` / `--right-only` / `--cherry-pick` / `--cherry-mark` / `--cherry` / `--no-min-parents` / `--no-max-parents` | `cli.object-readback` | 可达提交、范围/排除、限制、跳过、计数、时间过滤、first-parent、author、committer、message grep、path limitation、side/cherry 过滤和 parent bound reset 输出符合 fixture |
| `rev-list --parents` / `--children` / `--timestamp` | `cli.object-readback` | 父提交、child 提交和 timestamp 输出字段顺序符合 Git；`--parents --children` 互斥 |
| `clean -n/-f/-fd/-fX` | `cli.clean-rm-mv-lfs-basic` | dry-run、文件删除、目录删除、ignored-only 删除覆盖 |
| `rm <path>` | `cli.clean-rm-mv-lfs-basic` | tracked 文件从工作区和 index 移除 |
| `mv <src> <dst>` | `cli.clean-rm-mv-lfs-basic` | tracked 文件移动并更新 index |
| `lfs track/untrack/ls-files` | `cli.clean-rm-mv-lfs-basic` | `.libra_attributes` pattern 和 LFS tracked 文件列表可观察 |
| `reflog show` / `reflog show --stat` / `-p` / filters / `reflog exists` | `cli.reflog-symbolic-ref` | HEAD/ref 更新记录可读，exists 可脚本探测 |
| `reflog expire` | `cli.reflog-symbolic-ref` | 当前未实现，runner 负向断言稳定错误且 reflog 保持 intact |
| `symbolic-ref` / `symbolic-ref --short` / `symbolic-ref HEAD <target>` | `cli.reflog-symbolic-ref` | HEAD 符号引用读写可观察 |
| `--json open` | `cli.open-smoke` | 只输出 URL 和 `launched=false`，不启动外部程序 |



### `libra config` 参数覆盖表

| 参数或子命令 | 场景 ID | 关键断言 |
|---|---|---|
| `config set/get/list/unset` | `cli.config-basic-kv` | local scope 默认写入当前 repo，读写删闭环可观察 |
| `config --local` / `--global` / `--system` | `cli.config-scopes` | scope 隔离，system 在隔离环境中按权限/路径失败或成功 |
| `config set --add` | `cli.config-set-input-and-encryption` | 多值 key 可保留并由 `get --all` 读取 |
| `config set --stdin` | `cli.config-set-input-and-encryption` | stdin value 写入配置，空输入/互斥 flag 失败 |
| `config set --encrypt` / `--plaintext` | `cli.config-set-input-and-encryption` | vault 加密值不明文泄漏，reveal/plaintext 路径符合预期 |
| `config get --all` / `--regexp` / `--default` / `-d` / `--reveal` | `cli.config-get-default-and-patterns` | 多值、正则、默认值和密文 reveal 语义可观察 |
| `config list --name-only` / `--show-origin` / `--vault` | `cli.config-list-variants` | 列表形态、来源、vault 内容脱敏契约可观察 |
| `config list --ssh-keys` / `--gpg-keys` | `cli.config-list-variants`、`cli.config-key-generation` | 生成后的 key 元数据可列出且不泄漏私钥 |
| `config unset` / `--unset` / `--unset-all` | `cli.config-unset-compat-flags` | 单值/多值删除和兼容 hidden flag 翻译可观察 |
| `config import` / `--import` / `path` / `edit` | `cli.config-import-path-edit` | Git config fixture 导入、配置路径输出和编辑器失败路径可观察 |
| `config generate-ssh-key` / `generate-gpg-key` | `cli.config-key-generation` | remote/name/email/usage 参数写入 vault 元数据，非法输入失败 |
| Git 兼容 positional forms (`--get`, `--get-all`, `--get-regexp`, `--list`) | `cli.config-git-compat-mode` | hidden Git forms 翻译为 Libra 子命令，互斥/缺参负向可观察 |



### `libra clone/remote/fetch/pull/push/ls-remote` 参数覆盖表

| 参数或子命令 | 场景 ID | 关键断言 |
|---|---|---|
| `clone <repo> [path]` | `cli.clone-fetch-pull-local` | 本地 Git fixture 克隆后文件、refs、remote config 可观察 |
| `clone --depth` / `fetch --depth` | `cli.fetch-depth-local` | shallow marker 与 checkout 内容可观察，非法 depth 失败 |
| `remote add/remove/rename/-v/show/get-url/set-url/prune` | `cli.clone-fetch-pull-local` | 当前 remote 增删改查和 prune 基础路径可观察 |
| unsupported remote flags (`set-branches` / `set-head` / `update`) | `cli.clone-fetch-pull-local` | 当前未实现，runner 负向断言稳定错误 |
| `ls-remote --heads --tags --refs --get-url --sort --exit-code --symref` | `cli.clone-fetch-pull-local` | refs 过滤、URL 解析、排序、no-match exit 2 可观察；`--symref` 对本地 *Git* 源（`git-upload-pack` 通告 `symref=HEAD`）输出 `ref: refs/heads/main\tHEAD` |
| `ls-remote --symref` 设计边界 | `cli.clone-fetch-pull-local`（正向）+ `ls_remote_tests`（过滤/JSON/空边界） | 仅从 discovery capabilities（`symref=`）解析：Git 远端与本地 Git 仓库（`git-upload-pack`）通告 `symref=HEAD`，故输出 `ref:` 行；本地 **Libra** 仓库不通告该 capability，故不输出 `ref:` 行（Libra 从不基于本地 `HEAD` 合成 symref）。过滤、JSON 与空边界由 `write_ref_lines`/`resolve_output_symrefs` 单元测试覆盖。 |
| `fetch [remote]` / `fetch --all` / `fetch --depth` | `cli.clone-fetch-pull-local`、`cli.fetch-depth-local` | 默认 remote、全部 remote 和 depth fetch 可观察 |
| unsupported fetch flags (`--deepen` / `--unshallow` / `--prune` / `--porcelain` / tags modes) | `cli.clone-fetch-pull-local`、`cli.fetch-depth-local` | 当前未实现，runner 负向断言 |
| `pull` / `pull --ff-only` / `pull --rebase` | `cli.clone-fetch-pull-local` | 本地 fast-forward/rebase 基础路径可观察 |
| pull strategy flags (`--squash` / `--commit` / `--ff` / `--autostash`) | `cli.clone-fetch-pull-local` | 已实现，runner 正向断言 |
| `push <remote> [refspec...]` | `live.github-create-push-clone-fetch` | 真实 GitHub remote ref 更新和 clone/fetch readback 可观察 |
| `push --dry-run` / `--porcelain` | `live.github-create-push-clone-fetch`、`cli.push-local-file-remote-rejected` | 预览/机器输出语义；本地 file remote fail-closed |
| `push -u/--set-upstream` | `live.github-create-push-clone-fetch` | upstream tracking config 写入 |
| `push -f/--force` / `--force-with-lease[=<lease>]` / `--force-if-includes` | `live.github-create-push-clone-fetch`、`cli.push-local-file-remote-rejected` | force/lease 语义或本地 remote 拒绝均可观察 |
| `push --follow-tags` / `--no-follow-tags` / `--tags` / `--mirror` | `live.github-create-push-clone-fetch`、`cli.push-local-file-remote-rejected` | tag 推送、mirror/delete 语义或本地 remote 拒绝可观察 |
| `push --signed[=<when>]` / `-o/--push-option` | `live.github-create-push-clone-fetch` | remote capability/option 处理和无支持路径错误可观察 |
| `push --thin` / `--no-thin` / `--atomic` | `live.github-create-push-clone-fetch`、`cli.push-local-file-remote-rejected` | 兼容 no-op/capability 要求或本地 remote 拒绝可观察 |



### `libra notes/object/plumbing/maintenance` 参数覆盖表

| 参数或子命令 | 场景 ID | 关键断言 |
|---|---|---|
| `notes add/show/list/remove` `--ref` | `cli.notes-smoke` | 已注册：add + show/list（文本与 `--json`）、自定义 `--ref` 命名空间、无 `-f` 重复 add 的 'already has a note' 稳定错误、remove 后清空，并运行 fsck |
| `ls-tree` | `cli.ls-tree-smoke` | 已公开基础 tree inspection：默认输出、递归路径过滤、`--name-only`、JSON envelope、缺失路径负向和 fsck |
| `cat-file -t/-s/-p/-e <object>` | `cli.object-readback` | object 类型、大小、内容和存在性退出码可观察 |
| `cat-file --ai*` | 无（显式排除） | AI object inspection 属 Libra AI 扩展，不纳入 Git 兼容黑盒计划 |
| `hash-object -w` / `--stdin` / `--path` / `--no-filters` / `-t` | `cli.object-readback`、`cli.sha256-object-readback` | blob 写入、stdin 输入、路径上下文/no-filters 兼容入口、类型校验和 sha256 object id 可观察 |
| `show --no-patch` / `<rev>:<path>` / `<blob>` | `cli.object-readback` | commit 元数据、历史文件内容和 blob 内容可观察 |
| `show-ref --head` / `--no-head` / `--heads` / `--branches` / `--no-branches` / `--tags` / `--no-tags` / `--hash[=<n>]` / `--no-hash` / `--abbrev[=<n>]` / `--no-abbrev` / `--dereference` / `--no-dereference` / `--verify` / `--no-verify` / `--exists` / `--no-exists` / `--exclude-existing[=<pattern>]` | `cli.object-readback`, `cli.show-ref-exclude-existing` | HEAD/分支引用、`--branches` alias、hash-only/abbrev 输出、annotated tag peel、reset aliases、精确 ref 验证、存在性检查和 stdin filter 可观察 |
| `for-each-ref --points-at <object>` | `cli.object-readback` | 指向指定对象的 branch、lightweight tag、annotated tag 可观察 |
| `rev-list HEAD` / multi revision / `A..B` / `^A` / `A...B` / `--count` / `-n` / `--skip` / `--since` / `--after` / `--until` / `--before` / `--first-parent` / `--author` / `--committer` / `--grep` / `-- <path>` / `--left-right` / `--left-only` / `--right-only` / `--cherry-pick` / `--cherry-mark` / `--cherry` / `--children` | `cli.object-readback` | 可达提交输出、范围/排除、计数/限制、时间过滤、first-parent、author、committer、message grep、path limitation、side/cherry 过滤、children 元数据和 JSON envelope 可观察 |
| `fsck` / `fsck --connectivity-only` / `fsck <object>` | `cli.object-readback`、`cli.gc-smoke` | 默认、连通性和指定对象检查可观察 |
| `gc` / `prune` | `cli.gc-smoke` | 当前顶层命令未注册，runner 断言 JSON unknown-command 错误 |
| `maintenance run --dry-run --task gc` | `cli.gc-smoke` | 当前可用 maintenance 路径返回 JSON envelope |
| `archive` | `cli.archive-smoke` | 当前未注册，runner 断言 JSON unknown-command 错误 |
| `verify-pack [-v|-s] [--pack <pack>] <idx>...` | `cli.verify-pack-smoke` | idx/pack 对应校验、多 index sibling 推导、verbose 对象行、stat-only 摘要和 `--pack` 多 idx 拒绝路径可观察 |
| schema 建链自动升级（普通命令打开数据库即升级） | `cli.schema-upgrade-observable` | fresh schema 可用、过期 schema 自动升级、非 repo 失败可观察 |
| hidden `index-pack --stdin -o <idx>` / `--keep[=<MSG>]` / `--progress` / `--no-progress` | `cli.verify-pack-smoke` | 仅作为 pack fixture 辅助生成，不作为独立用户命令场景；断言 stdin 同 stem `.pack`、`.keep` 文件和消息换行，并执行 progress/no-progress 兼容入口 |



### 明确排除的命令面

| 命令 | 原因 | 替代保障 |
|---|---|---|
| `agent` / `automation` / `code` / `code-control` / `graph` / `sandbox` / `usage` | AI/交互/运行时扩展，不属于 Git 兼容版本管理黑盒计划 | 专属 TUI、provider、runtime 或命令测试 |
| `cloud` / `publish` / `package` | Cloud/发布/能力包扩展，需要真实云或非 Git 兼容语义 | 专属 cloud/publish/worker 测试和 live gate |
| `hooks` | hidden 兼容入口，由 `agent enable` 安装路径使用 | 专属 cargo 测试 |
| `stats` | Libra-only 只读统计命令，无自有 Git 兼容参数 | `tests/command/stats_test.rs` |
