# Libra PR 长期解决方案：基于 `gh` 的 GitHub PR 命令

## 背景与问题

Libra 当前的 Git-compatible 命令族（`branch` / `switch` / `commit` / `push` / `open`）已经能完成
"准备一个可被 review 的分支" 的全部工作，但 **无法直接创建 Pull Request**。原因是 PR 不是 Git
协议的一部分，而是 GitHub / GitLab / Gitea 等托管平台的 API 概念，Libra 目前没有托管平台层。

当前可行的临时流程是：

```bash
libra switch -c feature/my-change
libra add .
libra commit -s -m "feat(scope): describe change"
libra push -u origin feature/my-change
libra open https://github.com/<owner>/<repo>/compare/main...feature/my-change?expand=1
```

最后一步仍需在浏览器里手动点 "Create pull request"，不是一条命令的体验。

本方案给出一个 **长期可维护** 的实现路径：在 Libra 内新增 `libra pr` 命令族，以本地 `gh`
（GitHub CLI）作为 GitHub PR 后端执行器。

## 设计原则

### 为什么用 `gh` 作为后端，而不是直接实现 GitHub API

优点：

- 避免 Libra 自己维护 GitHub token 存储、OAuth / device flow、Enterprise host、2FA、SSO 等认证复杂度。
- `gh` 已经覆盖 GitHub.com 和 GitHub Enterprise。
- `gh pr create` 语义成熟，支持 `--fill` / `--draft` / `--reviewer` / `--label` / `--assignee` /
  `--project` / `--web` 等。
- Libra 先提供稳定的 PR UX，未来如需可替换为原生 GitHub API provider。
- 失败时保留 `gh` 原始错误信息，同时在 Libra 层补充上下文和修复建议。

缺点：

- 多一个外部运行时依赖。
- 行为受用户本机 `gh` 版本影响。
- JSON / 机器可读输出需要约束 `gh` 调用方式，不能完全依赖 human output。

长期看，这比在 Libra 内直接重写 GitHub 客户端更低风险、更容易维护。

### 职责边界

- **Libra 负责**：VCS 上下文、分支状态、diff、push、安全校验、统一 UX 与稳定 JSON schema。
- **`gh` 负责**：GitHub 认证、host 配置、PR 创建、浏览器打开、Enterprise 兼容。

关键约束：**push 必须用 `libra push`，不要让 `gh` 或 `git` 替 Libra 更新 ref**。Libra 是 VCS 主体，
`gh` 只负责 GitHub PR API 这一步。

## 命令设计

新增命令族（第一阶段只实现 `create`）：

```bash
libra pr create [OPTIONS]
libra pr status [OPTIONS]        # 后续阶段
libra pr view [<number>] [OPTIONS]   # 后续阶段
libra pr checkout <number> [OPTIONS] # 后续阶段
```

### `libra pr create` 参数

```bash
libra pr create \
  [--base <branch>] \
  [--head <branch>] \
  [--title <title>] \
  [--body <body>] \
  [--body-file <path>] \
  [--draft] \
  [--fill] \
  [--web] \
  [--push] \
  [--dry-run] \
  [--json]
```

### 默认行为推断

- `--head`：缺省取当前分支。
- 目标 remote：当前分支 upstream 的 remote，找不到则用 `origin`。
- `--base`：优先级为 显式 `--base` → remote 默认分支 → `main` / `master` 探测。
- 当前分支未 push 时 **默认报错** 并提示 `--push`，不静默 push。
- 工作区 dirty 时默认允许创建 PR（PR 关注的是已 push 的 commit，不是工作区），但给出提示；
  可选 `--require-clean` 拒绝。
- 当前分支相对 base 没有 ahead commit 时直接拒绝。
- 成功后输出 PR URL；`--web` 走 `gh pr create --web`。
- `--json` 输出 Libra 自己的 envelope，不直接透传 `gh` human output。

## 内部流程

`libra pr create` 按以下顺序执行：

1. 检查当前目录是 Libra repo。
2. 检查 `gh` 是否存在（`gh --version`）。
3. 检查当前分支不是 detached HEAD。
4. 解析 remote URL，确认是 GitHub remote（拒绝非 GitHub remote）。
5. 检查 `gh auth status --hostname <host>`。
6. 计算当前分支相对 base 是否有 ahead commit。
7. 检查远端是否已有 head 分支（`refs/remotes/<remote>/<head>` 是否存在且与本地一致）。
8. 若无远端分支：
   - 无 `--push`：报错并提示 `libra push -u origin <branch>` 或 `libra pr create --push`。
   - 有 `--push`：执行 `libra push -u origin <branch>`，而不是调用 `gh` push。
9. 组装 `gh pr create` 参数。
10. 执行 `gh pr create`（不通过 shell，见下节）。
11. 解析 PR URL。
12. 输出人类可读结果或 JSON。

## 参数映射

Libra 参数到 `gh` 参数：

| Libra 选项 | `gh` 参数 |
|------------|-----------|
| `--base main` | `--base main` |
| `--head feature/x` | `--head feature/x` |
| `--title "..."` | `--title "..."` |
| `--body "..."` | `--body "..."` |
| `--body-file PR.md` | `--body-file PR.md` |
| `--draft` | `--draft` |
| `--fill` | `--fill` |
| `--web` | `--web` |

后续可扩展的 GitHub 元数据（直接透传到 `gh`）：

```bash
--reviewer <login>
--assignee <login>
--label <name>
--milestone <name>
--project <name>
```

## 安全边界

因为要执行外部命令，**必须避免 shell 拼接**。使用 `std::process::Command` 逐个传 argv：

```rust
Command::new("gh")
    .arg("pr")
    .arg("create")
    .arg("--base")
    .arg(base)
    .arg("--head")
    .arg(head)
```

不要构造字符串再交给 shell：

```rust
// 禁止：sh -c "gh pr create --title ..."
```

其他注意点：

- `--body-file` 必须走路径规范化，错误要带上下文。
- remote URL 解析不能允许 `file://`、`ssh://evil` 等被误判成 GitHub。
- `--json` 模式下不要打开浏览器。
- 不要打印 token、`gh auth status` 的敏感信息。
- `--dry-run` 不得调用真正的 `gh pr create`，只打印将执行的动作。

## 错误处理

把常见失败转成 Libra 风格的可行动错误：

```text
error: GitHub CLI is not installed
hint: install it from https://cli.github.com/ or use `brew install gh`
```

```text
error: GitHub CLI is not authenticated for github.com
hint: run `gh auth login --hostname github.com`
```

```text
error: current branch 'feature/x' has not been pushed to origin
hint: run `libra push -u origin feature/x` or retry with `libra pr create --push`
```

```text
error: remote 'origin' is not a GitHub remote
hint: `libra pr create` currently supports only GitHub remotes through gh
```

```text
error: no commits to propose
hint: commit changes first, or choose a different base with `--base`
```

## JSON 输出

Libra 自己定义稳定 schema，不透传 `gh` 的输出：

```json
{
  "ok": true,
  "command": "pr create",
  "data": {
    "provider": "github",
    "backend": "gh",
    "remote": "origin",
    "repository": "owner/repo",
    "base": "main",
    "head": "feature/my-change",
    "url": "https://github.com/owner/repo/pull/123",
    "pushed": true,
    "draft": false
  }
}
```

`--dry-run --json`：

```json
{
  "ok": true,
  "command": "pr create",
  "data": {
    "dry_run": true,
    "provider": "github",
    "backend": "gh",
    "remote": "origin",
    "repository": "owner/repo",
    "base": "main",
    "head": "feature/my-change",
    "would_push": false,
    "gh_args": [
      "pr", "create",
      "--repo", "owner/repo",
      "--base", "main",
      "--head", "feature/my-change",
      "--fill"
    ]
  }
}
```

`gh_args` 是否暴露可讨论；若暴露，要保证不含 secret。

## 长期架构

不要把 `gh` 调用直接塞进 `src/command/pr.rs` 的大函数里，建议分层：

```text
src/command/pr.rs            CLI 参数、输出格式、错误展示
src/internal/github/mod.rs   GitHub remote 识别、owner/repo/host 解析
src/internal/github/gh.rs    gh 可用性检查、auth status、pr create 调用
src/internal/pr.rs           PR 创建前置校验、base/head 推断、调用 provider
```

可抽象一个轻量 trait，但第一版不必过度设计：

```rust
trait PullRequestProvider {
    async fn create(&self, request: CreatePullRequestRequest) -> Result<CreatePullRequestResponse>;
}
```

当前只实现 `GhGitHubProvider`。未来如需原生 API 或其他平台，可新增
`NativeGitHubProvider` / `GitLabProvider` / `GiteaProvider`。第一版可以先不引入 trait，
除非测试需要 mock。

## 推荐默认 UX

最理想的一条命令体验：

```bash
libra pr create --fill --push
```

行为：

1. 检查当前分支。
2. 自动推送当前分支到 `origin` 并设置 upstream。
3. 调用 `gh pr create --fill`。
4. 输出 PR URL。

典型输出：

```text
Pushed feature/my-change to origin.
Created pull request:
https://github.com/owner/repo/pull/123
```

若不想自动 push：

```bash
libra pr create --fill
```

当前分支未 push 时：

```text
error: current branch 'feature/my-change' has not been pushed to origin
hint: run `libra push -u origin feature/my-change` or retry with `libra pr create --push --fill`
```

## 测试策略

### 单元测试

- GitHub remote URL 解析：
  - `git@github.com:owner/repo.git`
  - `https://github.com/owner/repo.git`
  - `ssh://git@github.com/owner/repo.git`
  - GitHub Enterprise host
  - 非 GitHub remote 拒绝
- base / head 推断。
- `gh` argv 组装（不使用 shell）。
- `--body` / `--body-file` / `--fill` 冲突规则。
- `--dry-run` 不执行外部创建。

### 集成测试

- fake `gh` binary 放到临时 `PATH`，记录 argv。
- fake `gh` 返回成功 URL，验证 Libra JSON / human 输出。
- fake `gh` 返回 auth 失败，验证错误和 hint。
- fake `gh` 返回非 0，验证 Libra 不吞错误。
- `--push` 时验证调用的是 Libra push 逻辑，或在测试中 mock push 层。

不建议默认 CI 依赖真实 GitHub 网络。真实 `gh` + GitHub 的测试放 feature-gated / live test：

```bash
cargo test --features test-live-github --test pr_github_live_test
```

## 文档更新

落地时需同步：

- 新增 `docs/commands/pr.md` 与 `docs/commands/zh-CN/pr.md`。
- `COMPATIBILITY.md`：标记 `pr` 为 Libra GitHub extension，非 Git 命令。
- `docs/error-codes.md`：新增外部工具缺失、认证失败、remote 非 GitHub、未 push 等错误码。
- README command list 更新。
- `tests/INDEX.md`（若新增 integration target）。

## 分阶段落地

### 第 1 阶段：只读准备能力

```bash
libra pr create --dry-run
```

完成 remote 解析、base / head 推断、gh 检查、argv 生成，不真正创建 PR。

### 第 2 阶段：最小可创建

```bash
libra pr create --base main --title "..." --body "..."
libra pr create --fill
```

要求分支已 push，不自动 push。

### 第 3 阶段：自动 push

```bash
libra pr create --push --fill
```

用 `libra push -u origin <branch>` 推送，成功后调用 `gh`。

### 第 4 阶段：完整 GitHub UX

```bash
libra pr create --draft --reviewer alice --label bug --web
libra pr status
libra pr view
libra pr checkout
```

## 结论

长期最优解不是让用户手动组合 `libra push` + `gh pr create`，而是在 Libra 里提供稳定的
`libra pr create` 门面：

```bash
libra pr create --push --fill
```

内部规则：

- Libra 负责仓库状态和 push。
- `gh` 负责 GitHub PR 创建。
- 不通过 shell 调用 `gh`。
- 默认不静默 push，必须显式 `--push`。
- 输出 Libra 自己的稳定 JSON schema。
- 第一版只支持 GitHub，非 GitHub remote 明确拒绝。

这能在很低实现成本下获得长期可维护的 GitHub PR 能力，同时不把 Libra 绑定到一套自维护的
GitHub API / auth 实现。
