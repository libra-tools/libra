# 仓库 hooks

Libra 可从 `.libra/hooks` 运行仓库级生命周期 hook。它与外部 AI agent
集成所调用的隐藏命令 `libra hooks` 完全不同。Libra 有意不读取
`.git/hooks` 或 `core.hooksPath`。

## 发现顺序与信任边界

每个生命周期先查找无扩展名文件，再在 Unix 查找 `<name>.sh`、在 Windows
查找 `<name>.ps1`；最多执行一个。若高优先级候选存在但不安全，Libra 会
fail-closed，不会回落到另一个脚本。hooks 目录和 hook 文件都不能是符号链接；
文件必须是普通文件，Unix 上还必须可执行。`libra init` 安装的原样空操作
`pre-commit` 模板只作为示例，不会作为策略执行。超过 16 MiB 的 hook 文件会在
复制或执行前被拒绝。

hook 属于任意代码执行面。Libra 会把选中的文件复制到私有只读执行位置，
使用结构化参数，并通过强制仓库 sandbox 启动。hook 可写当前 worktree 中的
普通文件，但 `.git`、`.libra`、`.codex` 和 `.agents` 受保护。仅
`prepare-commit-msg` 与 `commit-msg` 可写当前 worktree 的
`.libra/COMMIT_EDITMSG`。sandbox 禁止写 worktree 外部和访问网络；运行时间
上限为 15 分钟，捕获输出上限为 1 MiB。若无法提供强制 sandbox backend，
自定义 hook 会 fail-closed，绝不降级为无 sandbox 执行。

hook 进程不会继承任意调用方环境变量。Libra 会先清空环境，再只传递当前已有的
`PATH`、home/profile、locale、terminal/timezone、Windows 进程定位变量，以及
下列 hook 变量和命令私有临时目录变量。API token、云凭据、agent socket 等
调用方专属变量不会进入仓库可控代码。

当前尚未实现 Windows restricted-token backend。Libra 会跳过原样内置的空操作
PowerShell 模板，因此普通已初始化仓库仍可使用；自定义 Windows 仓库 hook
会 fail-closed，直到该 backend 可用。只有评估过策略影响后才应使用下文逃逸阀。

每个 hook 都会收到 `LIBRA_HOOK_NAME`、`LIBRA_DIR`（worktree 私有元数据
目录）、`LIBRA_COMMON_DIR`（共享仓库元数据）、`LIBRA_HOOK_SOURCE`（原始
选中文件）和 `LIBRA_WORK_TREE`。

## 生命周期

| Hook | 时机与参数 | 失败行为 |
|------|------------|----------|
| `pre-commit` | 提交消息处理前；无参数。 | 阻止提交。 |
| `prepare-commit-msg` | `commit` 的编辑器前，或自动 merge commit 的 `pre-merge-commit` 后；`<message-file> [source [commit]]`。source 为 `message`、`template`、`merge` 或 `commit`；可修改消息文件。 | 阻止提交。 |
| `commit-msg` | 编辑/trailers 后，或 merge 消息准备后；`<message-file>`；可修改消息文件。 | 阻止提交。 |
| `post-commit` | `commit` 或自动 merge commit 成功后；无参数。amend 时先于 `post-rewrite`。 | advisory：警告但不回滚。 |
| `post-checkout` | checkout/switch 真正改变状态后；`<old-oid> <new-oid> <branch-flag>`，分支/detached 为 `1`，路径恢复为 `0`。already-on/show-current 不运行。 | advisory。 |
| `pre-rebase` | 新 rebase（含 `pull --rebase`）修改本地历史之前；`<upstream> [branch]`。 | 阻止 rebase。 |
| `pre-merge-commit` | 自动 merge commit 之前（含 `merge --continue`）；无参数。fast-forward、squash 或尚未继续的 `--no-commit` 不运行。 | 阻止 merge commit。 |
| `post-merge` | merge 完成后；`<squash-flag>`，squash 为 `1`，普通 merge/fast-forward 为 `0`。already-up-to-date 与冲突结果不运行。 | advisory。 |
| `post-rewrite` | amend 或 rebase 完成后；参数为 `amend` 或 `rebase`，stdin 为多行 `<old-oid> <new-oid>`。 | advisory。 |

blocking hook 启动失败、超时或非零退出时，会在相应 ref/历史变更前中止。
post hook 观察的是已完成变更，因此失败只产生警告，绝不宣称已回滚。人类模式
会重放 hook stdout/stderr；quiet、`--json` 与 `--machine` 会抑制它们，以保持
结构化信封。使用 `--exit-code-on-warning` 时，advisory 失败返回 9，但已记录的
变更仍然完成。

## 逃逸阀

- `libra commit --no-verify` 跳过该提交的全部仓库 hook；`--disable-pre`
  只跳过 `pre-commit`。
- `libra merge --no-verify` 跳过本次 merge 的全部 hooks，包括消息 hooks、
  `post-commit` 和 `--continue` 时待执行的 `pre-merge-commit`。
- 对没有专用 flag 的命令可设 `LIBRA_NO_HOOKS=1`，包括 checkout、switch、
  rebase 和 pull；也接受 `true`、`yes`、`on`。

这些控制会绕过仓库策略。应优先修复 hook 或 sandbox 配置。

## 示例

```sh
cat >.libra/hooks/pre-commit <<'EOF'
#!/bin/sh
cargo test --quiet
EOF
chmod +x .libra/hooks/pre-commit

# 绕过前先评估仓库策略影响。
LIBRA_NO_HOOKS=1 libra rebase main
```
