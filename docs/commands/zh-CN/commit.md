# `libra commit`

从已暂存更改创建新提交。

**别名：** `ci`

## 概要

```
libra commit [OPTIONS] -m <MESSAGE>
libra commit [OPTIONS] -F <FILE>
libra commit [OPTIONS] -C <COMMIT>
libra commit [OPTIONS] -c <COMMIT>
libra commit [OPTIONS] --date <DATE> -m <MESSAGE>
libra commit --amend [--no-edit]
```

## 说明

`libra commit` 从已暂存更改创建新提交，构建 tree 和 commit 对象，验证消息（包括可选的 conventional commit 格式，以及通过 vault 进行 GPG 签名），并更新 HEAD 和 refs。

该命令读取索引以确定哪些文件已暂存，构造与暂存内容匹配的 tree 对象层级，使用提供的消息和 author/committer 元数据创建 commit 对象，并推进当前分支 ref。启用 vault signing 时，提交会自动进行 GPG 签名。除非用 `--no-verify` 绕过，否则会运行 `.libra/hooks` 的 commit 生命周期。

在计算暂存变更或写入 tree/commit 对象之前，`commit` 会校验 stage-0 index 条目是否指向缺失或类型不匹配的 blob/tree 对象。损坏的 index 条目会 fail-closed，返回 `LBR-REPO-002`，并保持 `HEAD` 不变。

作者身份来自 `--author`，其次是 `GIT_AUTHOR_NAME`/`GIT_AUTHOR_EMAIL`，再回退到配置的 `user.name`/`user.email`；提交者身份来自 `GIT_COMMITTER_NAME`/`GIT_COMMITTER_EMAIL`，再回退到配置。除非设置了 `user.useConfigOnly=true`，Git 环境变量优先于配置。既有 `LIBRA_COMMITTER_NAME`/`LIBRA_COMMITTER_EMAIL` 仍作为更低优先级后备，兼容旧自动化。

## 选项

### `-m, --message <MESSAGE>`

使用给定消息作为提交消息。除非使用 `--no-edit`（搭配 `--amend`）或提供 `-F`，否则必需。

```bash
libra commit -m "Add new feature"
```

### `-F, --file <FILE>`

从给定文件读取提交消息。在未使用 `--no-edit` 时与 `-m` 互斥。

```bash
libra commit -F message.txt
```

### `-t, --template <FILE>`

以 `FILE` 内容作为初始提交消息：打开编辑器时（无其它消息源的默认情形）用作编辑器初始缓冲，`--no-edit` 时直接用作消息。`-t` 未给时回落到 `commit.template` 配置（文件路径，前导 `~/` 展开为 `$HOME`）。当提供消息源（`-m`/`-F`/`-C`/`-c`/`--fixup`/`--squash`）时模板被忽略——该源胜出，模板文件甚至不会被读取。与 Git 一致：若编辑器未改动模板，则中止提交（"you did not edit the message"）；`--no-edit` 不触发该检查。

```bash
libra commit -t .libra/commit-template.txt
```

### `--amend`

通过创建新提交替换当前分支 tip。新提交拥有与被替换提交相同的父提交。不能 amend merge commits（有多个父提交的提交）。
当 index tree 与提交消息都未变化时，`--amend --no-edit` 仍会重写提交并刷新 committer metadata；
不会打印成功但让 `HEAD` 保持不变。

```bash
libra commit --amend
libra commit --amend -m "Updated message"
```

### `--no-edit`

与 `--amend` 一起使用时，复用原提交消息，不提示修改。clean amend 仍会生成替换提交并刷新
committer date。与 `-m` 和 `-F` 冲突。

```bash
libra commit --amend --no-edit
```

### `--conventional`

根据 Conventional Commits 规范（https://www.conventionalcommits.org）验证提交消息。消息必须匹配模式 `<type>[optional scope]: <description>`。验证失败时会报错。

```bash
libra commit -m "feat: add login" --conventional
libra commit -m "fix(auth): handle expired tokens" --conventional
```

### `-a, --all`

提交前自动暂存已修改或已删除的已跟踪文件。等价于在 `libra commit` 前运行 `libra add -u`。不会添加新的未跟踪文件。

```bash
libra commit -a -m "Fix typo"
```

### `-s, --signoff`

使用 committer 身份在提交消息末尾添加 `Signed-off-by` trailer。

```bash
libra commit -s -m "Add feature"
```

### `--allow-empty`

允许创建没有更改的提交（相对父提交为空 diff）。适合触发 CI 或标记里程碑。

```bash
libra commit --allow-empty -m "Trigger CI"
```

### `--disable-pre`

只跳过 `pre-commit`；消息 hooks 与操作后的 hooks 仍会运行。

```bash
libra commit --disable-pre -m "Quick fix"
```

### `--no-verify`

本次调用跳过全部 `.libra/hooks` 生命周期和提交消息校验。

```bash
libra commit --no-verify -m "WIP: work in progress"
```

### 仓库 hooks

顺序为 `pre-commit` → `prepare-commit-msg` → 编辑器/trailers → `commit-msg` →
commit/ref 更新 → `post-commit` → amend 时的 `post-rewrite`。两个消息 hook 可修改
`.libra/COMMIT_EDITMSG`；blocking hook 失败时历史保持不变，post hook 则在成功后
以 advisory 方式运行。所有 hook 都经过强制 workspace-only、禁网 sandbox。参数、
解析顺序、输出行为与逃逸阀见[仓库 hooks](repository-hooks.md)。

### `--dry-run`

只显示将生成的提交摘要，不创建提交。预览不会运行仓库 hooks，也不会打开提交消息编辑器，因此无需提供消息；这样子进程不会在 `-a` 预览期间观察或修改 live index。`-v` 仍会直接打印 staged 预览 diff。

```bash
libra commit --dry-run -a
```

### `--author <AUTHOR>`

覆盖提交作者。必须使用标准 `A U Thor <author@example.com>` 格式。

```bash
libra commit --author "Jane Doe <jane@example.com>" -m "Patch"
```

### `--date <DATE>`

设置新提交的 author date。committer date 仍取当前时间，除非设置 `GIT_COMMITTER_DATE`。支持 Git raw 日期（`<unix> <+HHMM|-HHMM>`）、RFC 3339、`YYYY-MM-DD HH:MM:SS +HHMM`、`YYYY-MM-DD`、`2 days ago` 这类相对日期，以及 Unix timestamp。`--date` 优先于 `GIT_AUTHOR_DATE`。

```bash
libra commit --date "1700000000 +0000" -m "Backdated author timestamp"
```

### 身份与日期环境变量

`GIT_AUTHOR_NAME`、`GIT_AUTHOR_EMAIL`、`GIT_AUTHOR_DATE` 设置 author 身份/日期。`GIT_COMMITTER_NAME`、`GIT_COMMITTER_EMAIL`、`GIT_COMMITTER_DATE` 设置 committer 身份/日期。缺少 Git committer 字段时，Libra 会依次回退到对应 author 字段、email 的 `EMAIL`、再到 `LIBRA_COMMITTER_*`，最后使用配置。`user.useConfigOnly=true` 会禁用环境变量身份回退，但显式 `--author` 仍生效。

### `-C <COMMIT>`, `--reuse-message <COMMIT>`

复用指定提交的提交消息和 author metadata（姓名、邮箱、author date 与时区）。新提交仍使用当前 committer 身份/日期，或使用 `GIT_COMMITTER_*` 覆盖。

```bash
libra commit -C HEAD~1
```

### `-c <COMMIT>`, `--reedit-message <COMMIT>`

复用指定提交的提交消息和 author metadata，然后打开编辑器编辑消息。未配置编辑器时，直接使用复用的消息。

```bash
libra commit -c HEAD~1
```

### `--reset-author`

amend 时把 author 重置为当前 author 身份与日期，而不是保留被 amend 提交的原 author。当前 author 身份/日期按上面的 `GIT_AUTHOR_*` 与 `--date` 规则解析。新建非 amend 提交本来就是当前 author。

```bash
libra commit --amend --reset-author --no-edit
```

### `--status` / `--no-status`

提交消息编辑器模板默认把工作树状态以 `#` 注释行注入。`commit.status=false` 可关闭；`--status` / `--no-status` 显式覆盖该配置，最后一个 flag 胜出。由于是注释行，消息 cleanup 会将其剥离——仅供参考，不进入最终提交消息。未打开编辑器时（例如带 `-m`）无效果。在保留注释行的 cleanup 模式下也会省略（`--cleanup=verbatim`、`--cleanup=whitespace` 与 `--cleanup=scissors`——显式 scissors 保留 marker 之上的 `#` 行），从而绝不泄漏进消息；仅当打开编辑器且生效的 cleanup 会剥离注释（`strip`/`default`）时才注入。`-v` 仅截断附加的 diff，不强制 strip，故上述模式下即便加 `-v` 也不注入 status。

`commit.status` 按严格 local -> global -> system 级联读取，接受 Git 布尔值（包括 `0k`、`2` 等数值形式）。仅在确实能生成编辑器 status 模板时，无效值才会在 `-a`、hook、对象或历史写入前以 `LBR-CLI-002` 失败；local/global 配置库不可读时以 `LBR-IO-001` 失败。`-m`、dry-run/porcelain、JSON 与不剥离注释的 cleanup 因不存在模板而绕过该键；显式 `--status` / `--no-status` 在适用路径中短路并覆盖配置。启用模板 status 时，`status.*` 配置、状态采集或渲染错误保留原稳定错误码，并在 pre-commit hook、编辑器、commit/tree 对象或 ref 写入前中止；`status.showStash=true` 下不可读的 stash ref 或不可读/损坏的 stash log 也会以 `LBR-IO-001` 在这些副作用前中止，而不是静默显示为 0，fresh `status --cached` 同样 fail-closed。因此不可读 store 下的显式 `--status` 会点名首个必需的 `status.*` 键而非 `commit.status`，证明后者已被短路；`--no-status` 绕过整条模板状态路径。`--dry-run -a` 的整个预览使用 task-local 隔离临时 index，live index 从不被替换，临时 auto-stage blob、LFS 备份和 tree 对象也不持久化：非 verbose regular blob 用 64 KiB 缓冲流式计算对象 ID，不保留文件载荷；auto-stage 对 tracked symlink（包括 dangling link 和命中 LFS attribute 的路径）始终读取链接目标字节而不跟随链接，并在真实提交与预览中保持 mode `120000`；verbose 在读取前把 staged diff 变化两侧的 HEAD、已暂存和新 auto-stage 唯一 blob 纳入同一预算（未变化的大 blob 不计费），限制为单 blob 32 MiB、总计费 64 MiB、最多 4096 个对象；完整变化对象数会在任何 storage sizing、loose 解码或 pack index 扫描前先拒绝。auto-stage 会在读取工作区载荷前先预留字节与对象槽，再按对象 ID 去重结算，因此预先暂存和哈希去重都不能绕过限制。scratch 位于所有 linked worktree 共用的公共仓库存储 `.libra/tmp/commit-preview`，并发运行总预留上限 256 MiB；每次启动最多扫描 256 个 run、清理 32 个超过 24 小时且未持锁的旧 run；run 的 reservation 元数据缺失或不可读时会 fail-closed，不会按 0 字节计费。超限保持 live index/对象库不变并提示关闭 verbose；若变化对象不能完成有界本地预检（例如仅在 remote，或 pack 缺少现成 index），也会在加载前 fail-closed；预览绝不重建 pack index。实际预览读取显式调用仅本地的有界加载 API，不依赖 storage runtime 内部的 task-local 状态；loose 与非 delta pack 对象会在解码载荷前拒绝超限声明长度，loose 对象随后流式校验到声明边界并拒绝长度不符和 zlib 流后的尾随字节；超限 delta instruction 声明会在访问 base 链前拒绝，预算内 packed delta 才对完整 base/instruction/result 链计费并校验，有界深度且非法指令 fail-closed，一次批量只枚举 pack 一次并各打开现有 index 一次；后续 bounded pack read 使用专用 uncached delta decoder 并 move 最终 payload，不进入 200 MiB 全局 pack cache，也不产生额外完整 payload 返回克隆。关闭 verbose 可避免无上限加载。dry-run/porcelain 同时跳过 hook、编辑器、rerere 更新与 `post_commit` automation，因为没有真实提交。auto-stage 文件/LFS pointer 读取失败返回 `LBR-IO-001`，LFS 备份、预览缓存或对象写入失败返回 `LBR-IO-002`，均不再 panic。真实 `-a` 为保证落盘 index 不引用缺失对象，会把 LFS 源流式复制到临时快照、从该快照的精确字节生成 pointer，并原子替换同 OID 路径上可能截断/损坏的旧备份；启用 `--sync-data` 时会持久创建 shard 祖先目录、fsync 临时载荷，并同步 staging 与目标目录；Windows 因无可靠目录 fsync 等价物而使用 write-through 原子替换。若之后 status 采集中止，这些对象与已暂存 index 按既有 auto-stage-on-abort 语义保留。

```bash
libra commit                   # 编辑器模板默认包含注释化状态
libra config commit.status false
libra commit --status          # 本次覆盖 commit.status=false
libra commit --no-status       # 本次省略状态段
```

补充的 fail-closed 边界：`refs/stash` 已存在但不是普通文件（包括指向普通文件的符号链接）时，普通与 fresh cached status 都返回 `LBR-IO-001`；verbose preview 的 bounded pack 读取只使用既有 index，不会为无关 pack 重建缺失 index，并携带剩余 64 MiB 聚合预算、沿用 cache 的每对象最少 4 KiB 计费，越界后不再探测后续 pack 对象载荷。

### `--no-gpg-sign`

强制生成未签名提交：跳过本次提交的 Libra vault GPG 签名，对齐 `git commit --no-gpg-sign`。Git 兼容的 `commit.gpgSign=true|false` 默认值优先于 `vault.signing`：`true` 使用仓库 vault key 强制签名，`false` 禁用签名；未配置时继续使用 `vault.signing`。`--no-gpg-sign` 优先级最高并抑制两种配置。Git 的正向 `-S`/`--gpg-sign` 尚未公开。

```bash
libra commit --no-gpg-sign -m "message"
```

## 常用命令

```bash
libra commit -m "Add new feature"
libra commit -m "feat: add login" --conventional
libra commit --amend
libra commit --amend --no-edit
libra commit -a -m "Fix typo"
libra commit -F message.txt
libra commit --date "2026-07-09 10:00:00 +0800" -m "Backdated author date"
libra commit -s -m "Add feature"
libra commit --allow-empty -m "Trigger CI"
libra commit --json -m "Add feature"
```

## 人类可读输出

默认人类模式将提交摘要写到 `stdout`。

普通提交：

```text
[main abc1234] Add new feature
 2 files changed (new: 1, modified: 1, deleted: 0)
```

Root commit：

```text
[main (root-commit) abc1234] Initial commit
 1 file changed (new: 1, modified: 0, deleted: 0)
```

`--quiet` 会抑制所有 `stdout` 输出。

## 结构化输出

`libra commit` 支持全局 `--json` 和 `--machine` 标志。

- `--json` 向 `stdout` 写入一个成功信封
- `--machine` 以紧凑单行 JSON 写入相同 schema
- 两者都会抑制 hook stdout/stderr（通过 pipe 而不是继承）
- 成功时 `stderr` 保持干净

示例：

```json
{
  "ok": true,
  "command": "commit",
  "data": {
    "head": "main",
    "branch": "main",
    "commit": "abc1234def5678901234567890abcdef12345678",
    "short_id": "abc1234",
    "subject": "Add new feature",
    "root_commit": false,
    "amend": false,
    "files_changed": {
      "total": 2,
      "new": 1,
      "modified": 1,
      "deleted": 0
    },
    "signoff": false,
    "conventional": null,
    "signed": true
  }
}
```

Root commit：

```json
{
  "ok": true,
  "command": "commit",
  "data": {
    "head": "main",
    "branch": "main",
    "commit": "abc1234def5678901234567890abcdef12345678",
    "short_id": "abc1234",
    "subject": "Initial commit",
    "root_commit": true,
    "amend": false,
    "files_changed": {
      "total": 1,
      "new": 1,
      "modified": 0,
      "deleted": 0
    },
    "signoff": false,
    "conventional": null,
    "signed": true
  }
}
```

Amend：

```json
{
  "ok": true,
  "command": "commit",
  "data": {
    "head": "main",
    "branch": "main",
    "commit": "def5678abc1234901234567890abcdef12345678",
    "short_id": "def5678",
    "subject": "Amended message",
    "root_commit": false,
    "amend": true,
    "files_changed": {
      "total": 1,
      "new": 0,
      "modified": 1,
      "deleted": 0
    },
    "signoff": false,
    "conventional": null,
    "signed": true
  }
}
```

### Schema 说明

- `head` 是分支名，或为保持向后兼容而使用的 `"detached"`
- HEAD detached 时 `branch` 为 `null`；否则为 `Some(name)`
- 传递 `--conventional` 且验证成功时，`conventional` 为 `true`；未请求时为 `null`
- 启用 vault signing 且提交已 GPG 签名时，`signed` 为 `true`
- `-s` / `--signoff` 追加 `Signed-off-by` trailer 时，`signoff` 为 `true`

## 设计理由

### `--conventional` conventional commits 标志

Git 没有内置提交消息格式验证；团队依赖 commitlint、husky 或 CI 检查等外部工具来强制 Conventional Commits。Libra 在 commit 命令中直接提供一等 `--conventional` 验证。这有两个目的：（1）在提交时立即反馈，而不是在 CI 中延迟反馈；（2）让以编程方式生成提交消息的 AI 代理无需外部工具即可验证输出。该标志是 opt-in 而非强制，以尊重使用不同提交消息约定的团队。

### 默认 vault signing，而不是手动 GPG 设置

在 Git 中，提交签名需要配置 `user.signingkey`、`gpg.program` 和 `commit.gpgSign`，这是多数开发者会跳过的多步流程。Libra 的 vault 在仓库初始化时自动生成并管理 PGP 签名密钥，因此提交默认零配置签名。用户可用 `commit.gpgSign` 设置 Git 兼容的 scope 默认；未设置时继续使用 Libra 的 `vault.signing` 默认值。

### `--disable-pre` 标志

`--disable-pre` 只跳过 pre-commit hook，但仍运行消息与 post-operation hooks。它比 `--no-verify` 更细粒度，后者会跳过全部仓库 hooks。用例是开发者信任提交消息验证，但在快速迭代中想跳过昂贵的 pre-commit 检查。

### 用 `--no-verify` 跳过 hooks

当需要绕过全部仓库策略时（例如紧急修复、WIP commits），`--no-verify` 会跳过每个 `.libra/hooks` 生命周期及提交消息校验。名称遵循 Git 约定；Libra 额外跳过 advisory post hooks 的行为在此明确记录。

## 参数对比：Libra vs Git vs jj

| 参数 / 标志 | Git | jj | Libra |
|---|---|---|---|
| 带消息提交 | `git commit -m "msg"` | `jj commit -m "msg"` | `libra commit -m "msg"` |
| 从文件提交 | `git commit -F file` | N/A | `libra commit -F file` |
| Amend 上次提交 | `git commit --amend` | `jj describe`（编辑工作副本提交） | `libra commit --amend` |
| Amend 且不编辑 | `git commit --amend --no-edit` | `jj describe --no-edit` | `libra commit --amend --no-edit` |
| 自动暂存已跟踪 | `git commit -a` | N/A（自动跟踪） | `libra commit -a` |
| 允许空提交 | `git commit --allow-empty` | `jj commit --allow-empty` | `libra commit --allow-empty` |
| Signoff trailer | `git commit -s` / `--signoff` | N/A | `libra commit -s` / `--signoff` |
| GPG 签名提交 | `git commit -S`（手动 GPG） | N/A（无签名） | 自动（vault-backed） |
| 覆盖 author | `git commit --author="..."` | N/A | `libra commit --author="..."` |
| Author date | `git commit --date=<date>` | N/A | `libra commit --date <date>` |
| Conventional 检查 | 外部工具（commitlint） | N/A | `libra commit --conventional` |
| 只跳过 pre-commit | N/A | N/A | `libra commit --disable-pre` |
| 跳过所有 hooks | `git commit --no-verify` | N/A | `libra commit --no-verify` |
| Fixup commit | `git commit --fixup=<commit>` | N/A | `libra commit --fixup=<commit>` |
| Squash commit | `git commit --squash=<commit>` | `jj squash` | `libra commit --squash=<commit>` |
| 复用消息和 author | `git commit -C/-c <commit>` | N/A | `libra commit -C/-c <commit>` |
| 交互式消息 | `git commit`（打开编辑器） | `jj commit`（打开编辑器） | `libra commit`（无 -m/-F 时打开编辑器）/ `-e` |
| 编辑器中 verbose diff | `git commit -v` | N/A | `libra commit -v` |
| 编辑器模板 status | 默认开启；`commit.status` / `--[no-]status` | N/A | 默认开启；`commit.status` / `--[no-]status` |
| verbose 配置默认 | `commit.verbose`（未给 `-v` 时回退；CLI flag 优先） | N/A | `libra config commit.verbose true` |
| 重置作者日期 | `git commit --reset-author` | N/A | `libra commit --reset-author` |
| Cleanup 模式 | `git commit --cleanup=<mode>` | N/A | `libra commit --cleanup=<mode>` |
| Cleanup 配置默认 | `commit.cleanup`（未给 `--cleanup` 时回退；CLI flag 优先） | N/A | `libra config commit.cleanup <mode>` |
| Trailer | `git commit --trailer="..."` | N/A | `libra commit --trailer="..."` |
| 结构化 JSON 输出 | N/A | N/A | `--json` / `--machine` |
| 错误提示 | 最少 | 最少 | 每种错误类型都有可操作提示 |

## 错误处理

每个 `CommitError` 变体都会映射到显式 `StableErrorCode`。

| 场景 | 错误码 | 退出码 | 提示 |
|----------|-----------|------|------|
| 索引损坏 | `LBR-REPO-002` | 128 | "the index file may be corrupted; try 'libra status' to verify" |
| index 对象缺失或类型不匹配 | `LBR-REPO-002` | 128 | "run 'libra fsck' to inspect missing or mistyped objects" |
| 无法保存索引 | `LBR-IO-002` | 128 | -- |
| 无内容可提交（干净） | `LBR-REPO-003` | 128 | "use 'libra add' to stage changes" |
| 无内容可提交（无已跟踪文件） | `LBR-REPO-003` | 128 | "create/copy files and use 'libra add' to track" |
| 缺少 author 身份 | `LBR-AUTH-001` | 128 | "run 'libra config user.name ...' and 'libra config user.email ...'" |
| 没有可 amend 的提交 | `LBR-REPO-003` | 128 | "create a commit before using --amend" |
| Amend merge commit | `LBR-REPO-003` | 128 | "create a new commit instead of amending a merge commit" |
| 无效 author 格式 | `LBR-CLI-002` | 129 | "expected format: 'Name <email>'" |
| 无效 author/committer 日期 | `LBR-CLI-002` | 129 | 支持的日期格式 |
| 无法读取消息文件 | `LBR-IO-001` | 128 | -- |
| 空提交消息 | `LBR-REPO-003` | 128 | "use -m to provide a commit message" |
| Tree 创建失败 | `LBR-INTERNAL-001` | 128 | Issues URL |
| 对象存储失败 | `LBR-IO-002` | 128 | -- |
| 父提交缺失 | `LBR-REPO-002` | 128 | "the parent commit is missing or corrupted" |
| HEAD 更新失败 | `LBR-IO-002` | 128 | -- |
| Blocking 仓库 hook 失败 | `LBR-REPO-003` | 128 | "use --no-verify to bypass repository hooks" |
| Conventional commit 无效 | `LBR-CLI-002` | 129 | "see https://www.conventionalcommits.org for format rules" |
| Vault signing 失败 | `LBR-AUTH-001` | 128 | "check vault configuration with 'libra config --list'" |
| Auto-stage 源文件读取/hash 失败 | `LBR-IO-001` | 128 | 检查报错中点名的工作树文件 |
| Auto-stage 预览/对象/LFS 写入失败 | `LBR-IO-002` | 128 | 检查报错中点名目标的空间与权限 |
| 暂存更改计算 | `LBR-REPO-002` | 128 | "failed to compute staged changes" |
| `commit.status` 无效 | `LBR-CLI-002` | 129 | 修正配置值 |
| `commit.status` 配置不可读 | `LBR-IO-001` | 128 | 修复 local/global 配置库 |

## 兼容性说明

- Libra 支持交互式编辑器消息编写（`-e/--edit`，以及无 `-m`/`-F` 的裸 `commit` 在有可用编辑器时打开）
- jj 没有带暂存的传统 `commit` 命令；`jj commit` 会完成 working copy commit
- 支持 `--fixup` 和 `--squash`（autosquash 提交重组）
- Vault signing 替代外部 keyring；`commit.gpgSign` 已生效，`user.signingkey` 仍由 vault 管理
- 支持 `--cleanup=<mode>` 消息清理（`strip`/`whitespace`/`verbatim`/`scissors`/`default`），未给时回退到 `commit.cleanup` 配置；`commit.verbose` 配置可使 `-v` 成为默认
