# Libra PR 长期方案：基于 `gh` 的 GitHub PR 命令

> **文档状态**：设计定稿（实现前约束）。第一版仅 `libra pr create`，后端为本机 GitHub CLI `gh`。  
> **最后修订**：2026-07-09（第二轮十二维复核 + 契约对齐；第三轮补充 `--offline`、错误码 category、dry-run schema 等细节；**第四轮以代码为准逐条核对** `CliErrorReport` / `OutputConfig` / `push` / `ls-remote` / `merge-base` / compat 守卫；**第五轮补齐 `gh` 官方隐式 push/fork prompt、`PushArgs` upstream setter 缺口、`run_push` side-effect 语义**；**第六轮十二维独立分析：修正控制流 ordering（gh 存在性/版本/auth 前置于 push）、钉死 `LBR-PR-007` category、补 unborn HEAD 与 stderr 脱敏口径、信号转发等**；**第七轮补 dry-run 分支（跳过网络 ls-remote 以兑现「仅本地推断」）、`--web` 成功/失败语义、`GH_PROMPT_DISABLED` 不存在修正、`--offline` 双码澄清**——见附录 A.4/A.5/A.6/A.7）。

## 1. 决策摘要

| 决策 | 选择 | 理由 |
| --- | --- | --- |
| PR 是否属于 Git 协议 | 否 | PR/MR 是托管平台 API 概念 |
| 第一版后端 | 本机 `gh` | 复用认证、Enterprise host、SSO/2FA、浏览器流；避免自建 OAuth/token 存储 |
| VCS 主体 | 始终是 Libra | 分支、ahead、dirty、push、tracking 仅走 Libra |
| Push | 仅显式 `--push`；内部调用 Libra push | 禁止 `gh`/`git` 更新 Libra ref |
| `gh --head` | 始终显式传入已解析 head | 阻断 `gh pr create` 在未 push 分支上进入 push/fork prompt 路径 |
| 机器输出 | **全局** `libra --json` / `libra --machine` | 与全仓 `OutputConfig` 一致；禁止子命令级 `--json` |
| 失败契约 | `CliErrorReport` + `LBR-*` | 见 `src/utils/error.rs` / `docs/error-codes.md` |
| 远端 head 判定 | push 结果 → `ls-remote --heads` → tracking（标 `stale_risk`） | 禁止只信过期 `refs/remotes/*` |
| 范围 | 仅 GitHub 同仓库 PR | fork / GitLab / Gitea / native API 延后 |
| Dry-run | **Libra 侧**实现，不映射 `gh pr create --dry-run` | `gh` 的 `--dry-run` 文档写明 *May still push git changes* |
| 非交互标题/正文 | 必须 `--fill` **或** `--title`（`--web` 除外） | 否则 `gh` 会打开交互 prompt/editor，阻塞 CI/非 TTY |
| 预检排序 | `gh` 存在/版本/auth **先于** push | 避免可避免的「已推送未建 PR」；本地检查零网络副作用，应尽早失败 |

按本文约束落地，方案在合理性、可行性、安全性、接口兼容性与可维护性上可接受。

---

## 2. 背景与问题

Libra 的 Git-compatible 命令族（`branch` / `switch` / `commit` / `push` / `open`）已能完成「准备可 review 的分支」，但无法创建 Pull Request：PR 不属于 Git 协议，Libra 尚无托管平台层。

当前临时流程：

```bash
libra switch -c feature/my-change
libra add .
libra commit -s -m "feat(scope): describe change"
libra push -u origin feature/my-change
libra open https://github.com/<owner>/<repo>/compare/main...feature/my-change?expand=1
```

最后一步仍需在浏览器手动点 “Create pull request”。本方案在 Libra 内提供 `libra pr` 门面，以本机 `gh` 作为 GitHub PR 后端。

---

## 3. 目标与非目标

### 3.1 目标

- 提供 `libra pr create`，一条命令创建 GitHub Pull Request。
- Libra 是唯一 VCS 主体：分支状态、commit 检查、dirty 提示、push、tracking 更新均由 Libra 负责。
- 复用 `gh` 的 GitHub 认证、host 配置、Enterprise 与 PR API。
- 人类可读输出稳定；脚本/CI 使用 `libra --json pr create` 或 `libra --machine pr create`。
- 默认不静默 push，只有显式 `--push` 才推送。
- 支持无副作用的 `--dry-run`（仅本地推断 + argv 预览）。
- 为未来 native GitHub API、GitLab、Gitea provider 保留清晰扩展点。

### 3.2 非目标（第一版）

- 不实现 Libra 内置 GitHub token 存储、OAuth device flow 或 REST/GraphQL 客户端。
- 不支持 GitLab / Gitea / Bitbucket PR/MR。
- 不支持 fork PR 完整自动化（`--head <owner>:<branch>` 拒绝）。
- 不让 `gh` 或 `git` 执行 push，不允许它们更新 Libra ref。
- 不依赖真实 GitHub 网络作为默认 CI 前置条件。
- 不透传 `gh` 的 human 输出作为机器接口。
- 不把 Libra `--dry-run` 映射为 `gh pr create --dry-run`。
- 不支持 `--body-file -`（从 stdin 读 body）；第一版只接受普通文件路径。
- 不支持 `gh` 的 `--editor` / `--template` / `--fill-first` / `--fill-verbose` / `--no-maintainer-edit` / `--recover` 等扩展选项；第一版未暴露的 `gh` 选项必须显式不在 CLI 面中，后续阶段逐项加设计、argv 测试与版本门槛。

---

## 4. 设计原则

### 4.1 为何用 `gh` 而不是直接实现 GitHub API

**优点**

- 避免自维护 token 存储、OAuth/device flow、Enterprise host、2FA、SSO。
- `gh` 已覆盖 GitHub.com 与 GitHub Enterprise。
- `gh pr create` 语义成熟（`--fill` / `--draft` / `--reviewer` / `--label` / `--web` 等）。
- Libra 先稳定 UX，未来可替换为 `NativeGitHubProvider` 等。
- 失败时保留经脱敏的 `gh` 上下文，并附加可行动 hint。

**风险与控制**

| 风险 | 控制措施 |
| --- | --- |
| 外部运行时依赖 | 启动前检查 `gh` 可执行与版本；缺失时给出安装 hint |
| 本机 `gh` 版本漂移 | 固定最低支持版本；argv 组装用 fake `gh` 测兼容 |
| human 输出不稳定 | 全局 JSON/machine 只输出 Libra envelope，不透传 `gh` 文本 |
| `gh` 隐式 push / fork / 交互 | 显式传 `--head`/`--repo`/`--base`；禁用不兼容组合；push 只走 Libra；无 title/fill 时拒绝（防 prompt） |
| Enterprise host 不一致 | 从 remote URL 推断 host；`gh auth status --hostname <host>` 与 `--repo` 同源 |
| `gh pr create --dry-run` 仍可能 push | **禁止**调用该 flag；dry-run 完全在 Libra 实现 |
| PATH 上错误的 `gh` | 解析一次可执行路径；错误信息报告实际路径；可选后续配置 `pr.ghPath`（非 v1 必做） |

### 4.2 职责边界

| 角色 | 负责 | 不负责 |
| --- | --- | --- |
| **Libra** | repo 上下文、base/head 推断、ahead/dirty、远端 head 判定、push、安全校验、错误归一化、稳定 JSON envelope | GitHub OAuth、token 生命周期 |
| **`gh`** | GitHub 认证、host 配置、PR 创建 API、`--web` 浏览器、GHE 兼容 | 更新 Libra SQLite refs / objects / tracking |

**硬约束**：push 必须用 Libra push 路径；`gh` 只负责「创建/打开 PR」这一步。

---

## 5. 命令设计

新增命令族（第一阶段只实现 `create`）：

```bash
libra pr create [OPTIONS]
libra pr status [OPTIONS]             # 后续阶段
libra pr view [<number>] [OPTIONS]    # 后续阶段
libra pr checkout <number> [OPTIONS]  # 后续阶段
```

在 `src/cli.rs` 注册为 Libra extension（`COMPATIBILITY.md`：`intentionally-different`），并加入 `ROOT_AFTER_HELP` 的 `Remote And Cloud` 分组（与 `push` / `fetch` / `open` 同组，满足 `root_after_help_lists_every_visible_command`）。

### 5.1 `libra pr create` 参数

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
  [--require-clean]
```

**机器可读输出**（全局 flag，子命令不定义 `--json`）：

```bash
libra --json pr create --fill              # pretty JSON → stdout
libra --json=compact pr create --dry-run
libra --machine pr create --push --fill    # ndjson + no-pager + quiet（CI 推荐）
# 因 clap global=true，下列写法通常也合法，文档与脚本优先推荐 flag 在前：
libra pr create --fill --json
```

### 5.2 互斥与前置规则

| 规则 | 结果 | 建议 error_code |
| --- | --- | --- |
| 非 `--web` 且既无 `--fill` 也无 `--title` | **拒绝**（防 `gh` 交互 prompt/editor） | `LBR-PR-005` / `LBR-CLI-002` |
| `--body` 与 `--body-file` 同时 | 拒绝 | `LBR-PR-005` |
| `--fill` 与 `--title` / `--body` / `--body-file` 同时 | **第一版拒绝**（`gh` 虽规定 title/body 覆盖 fill，但版本差异风险；稳定后可放宽） | `LBR-PR-005` |
| `--web` 与全局 `--json` / `--machine` | 拒绝 | `LBR-PR-005` |
| `--web` 与 `--dry-run` | 拒绝 | `LBR-PR-005` |
| `--web` 且非 TTY / 无 GUI 探测失败 | 拒绝，可行动 hint | `LBR-PR-005` 或 `LBR-IO-*` |
| `--body-file` 为 `-` 或指向非普通文件 | 拒绝 | `LBR-PR-005` / `LBR-IO-001` |
| `--head` 含 `:`（fork 语法） | 拒绝 | `LBR-PR-005` |
| `--push` 且 `--head` 不是当前分支 | 拒绝 | `LBR-PR-005` |
| detached HEAD | 拒绝 | `LBR-PR-007` / `LBR-REPO-003` |
| unborn HEAD（空仓库，HEAD 指向尚无 commit 的分支） | 拒绝（无 head tip，ahead 无法判定） | `LBR-PR-007` / `LBR-REPO-003` |
| `--base` 与 `--head` 同名 | 拒绝（head 无独有 commit，等同 ahead 为空；ahead 检查本会拦截，此处显式前置给可读错误） | `LBR-PR-007` |
| 相对 base 无 ahead commit | 拒绝 | `LBR-PR-007` |
| 工作区 dirty | 默认允许 + warning；`--require-clean` 拒绝（dirty 指 index/worktree 与 HEAD 不一致，含 untracked 文件） | 拒绝时 `LBR-PR-007` |
| 远端 head 缺失或 OID 不一致且无 `--push` | 拒绝 | `LBR-PR-004` |
| remote 非 GitHub（第一版判定失败） | 拒绝 | `LBR-PR-003` |
| `--offline`（全局）与 `pr create` | 拒绝：`pr create` 必须调用 GitHub API | `LBR-PR-005`（用法级预检，优先，exit 129）。`--offline` 在互斥校验阶段即被拦截（§6 步骤 1），不会进入网络阶段，故 `LBR-NET-001` 对此场景不可达；保留双码引用仅为防御性文档 |

### 5.3 默认行为推断

- **`--head`**：缺省 = 当前分支名（须为 symbolic ref，非 detached）。
- **目标 remote**：当前分支 upstream remote → 否则 `origin` → 否则错误（要求用户配置 remote）。
- **`--base` 优先级**（任一成功即停）：
  1. 显式 `--base <branch>`
  2. 当前分支 upstream 的 merge 分支名，即读取 `branch.<name>.merge`（若与 head 同名或该配置指向非 remote-tracking 则跳过）。**注意**：Libra 的 `branch.<name>.merge` 存的是 head 自身的上游（`libra branch` set-upstream 写入 `refs/heads/<remote_branch>`，见 `command::branch::set_upstream_with_conn`），通常与 head 同名 → 该步**多数情况被跳过**、退化到步骤 3；仅当上游被显式配成一个不同的 base 分支名时才命中。这是 best-effort 提示，不是主路径。
  3. 本地 `refs/remotes/<remote>/HEAD` 解析出的默认分支（Libra 里这是由 `fetch`/`clone`/`libra remote set-head` 维护的符号 ref；**这是首选的默认分支来源**）
  4. `libra ls-remote --symref <remote> HEAD` 解析默认分支（**一次**网络查询；失败则继续）。**能力边界**：`libra ls-remote --symref` 只回显远端在协议里 advertise 的 `symref=` 能力（`command::ls_remote::parse_symrefs`），**对 Libra-native 远端与本地仓库会返回空**，仅对通过 `git-upload-pack` 服务、真正 advertise symref 的 Git 远端有效——因此步骤 3 才是默认分支的主来源，步骤 4 只是对真 Git 远端的补充。
  5. 探测本地是否存在 `main` / `master` 的 remote-tracking 或本地分支
  6. 仍失败 → 错误，要求显式 `--base`
- **未 push**：默认拒绝并提示 `--push` 或 `libra push -u <remote> <branch>`；不静默 push。
- **dirty**：默认允许（PR 关心已 push 的 commit）；human 提示；JSON 中 `data.dirty: true`。
- **成功输出**：PR URL；`--web` 只走浏览器创建流，且不可与 JSON/machine 同次调用。`--web` 仍须通过远端 head 一致性预检（§6.3）与 ahead 检查（§6.2）——`gh pr create --web` 打开预填表单，前提是分支已推送到远端，故预检不豁免；唯一豁免的是 fill/title 必填约束（浏览器表单由用户手动填写）。`--web` 成功时 human 输出提示「Opening browser...」+ compare URL（非 PR URL，因 PR 在浏览器中手动完成），exit 0；失败（浏览器无法打开、非 TTY 等）→ `LBR-PR-005` 或 `LBR-IO-*`。
- **空 title/body**：`--title ""`（空串）视为未提供 title，回退到 fill/title 必填校验；`--body ""` 视为无 body（`has_body: false`）。
- **`--offline`（全局）**：`pr create` 必须通过网络调用 GitHub API（或 Enterprise equivalent），在 `--offline` 下直接拒绝，避免用户误以为纯本地推断可继续。

---

## 6. 数据流与控制流

`libra pr create` **严格按序**（早失败，无默认 fetch/push）。**原则**：所有零网络副作用、可预防失败的本地检查（`gh` 存在性、版本门禁、auth 状态）必须前置于任何 push 副作用，避免把用户推入可避免的「已推送未建 PR」状态：

```text
parse + 互斥校验
  → require Libra repo
  → reject detached / unborn HEAD
  → 推断 head / remote / base
  → 解析 remote URL → host / owner / repo → 必须为 GitHub（com 或 Enterprise）
  → dirty 检查（require-clean?）
  → ahead 检查（相对 base）
  → 解析 gh 可执行文件 + 版本门禁              ← 本地、零网络副作用，前置于 push
  → dry-run：远端 head 仅查本地 tracking（标 stale_risk）→ 输出推断结果与脱敏 gh_args，退出成功（无网络、无 push、无 create、无 browser）
  → 非 dry-run：gh auth status --hostname <host>（禁止 --show-token）  ← 前置于 push
  → 非 dry-run：远端 head 预检（ls-remote --heads → tracking+stale_risk fallback）
  → 可选：Libra push（仅 --push；成功后以 PushOutput 更新远端 head 判定）
  → 非 dry-run：Command 执行 gh pr create（超时 + stderr 上限）
  → 解析 PR URL → human 或全局 JSON/machine
```

> **排序理由**：`--push` 路径会写远端（不可回滚的副作用）。若把 `gh` 存在性/版本/auth 检查放在 push 之后，用户在 `gh` 缺失或未认证时会得到「分支已推送但 PR 未建」——而这是**可预防**的。auth 检查虽触网（GitHub），但先于 push 可以在写远端前暴露认证问题。
>
> **dry-run 分支理由**：§3.1 与 §10 均要求 dry-run「仅本地推断」。若 dry-run 经过「远端 head 预检」步骤，会触发 `ls-remote --heads` 网络调用，与「仅本地推断」矛盾。因此 dry-run 在 gh 版本检查后**立即分支**：远端 head 仅查本地 tracking ref（标 `stale_risk: true`），不调用 `ls-remote`、不 auth、不 push、不 create。`would_push` 基于 tracking ref 判定（可能过期，由 `stale_risk` 标记）。

### 6.1 编号步骤（实现清单）

1. 解析 CLI；**在任何外部命令前**完成互斥与「fill/title/web」完整性校验。
2. 确认当前目录为 Libra repo（`require_repo` 同类逻辑）。
3. 拒绝 detached HEAD 或 unborn HEAD（空仓库，HEAD 指向尚无 commit 的分支）。
4. 推断 head、remote、base（§5.3）。
5. 解析 remote URL；提取 `host` / `owner` / `repo`；确认第一版支持的 GitHub remote。
6. dirty：默认记 warning；`--require-clean` → 错误。
7. **Ahead 检查**（见 §6.2）。同时显式拒绝 `--base` 与 `--head` 同名（head 无独有 commit）。
8. 解析 `gh` 可执行文件；检查最低版本（临时 `>=2.40.0`，实现前用目标平台实测后写入用户文档最终值）。**本地、零网络副作用，前置于任何 push。**
9. **`--dry-run` 分支**：远端 head 仅查本地 tracking ref（标 `stale_risk: true`），不调用 `ls-remote`；组装 `gh pr create` argv（脱敏）；输出推断结果与脱敏 `gh_args`，成功退出（**无网络、无 push、无 create、无 browser**）。`would_push` 基于 tracking ref 判定（可能过期，由 `stale_risk` 标记）。JSON 标 `auth_checked: false`。
10. 非 `--dry-run`：`gh auth status --hostname <host>`（不传 `--show-token`，不透传 token 到日志）。**前置于 push**——auth 失败时不推送分支，避免「已推送未建 PR」。
11. 非 `--dry-run`：**远端 head 状态预检**（见 §6.3）：`ls-remote --heads` 取远端 tip，失败才退到 tracking+`stale_risk`；与本地 `<head>` 分支 tip 比较。若 `--push` 且 `ls-remote` 失败（网络错误或分支不存在），**不阻断** push（push 会处理），但记 warning。
12. 若远端缺失或不一致：无 `--push` → 错误；有 `--push` → 调用 Libra push（§6.4）。push 成功后以 `PushOutput.updates[].new_oid` / `up_to_date` 作为 `remote_head_oid == local_head_oid` 的依据；`run_push` 成功路径已调用 `update_remote_tracking_refs`，不要在 `pr` 层重复手写 remote-tracking ref。若同时需要设置 upstream，必须走 §6.4 的安全 setter 方案。
13. 组装 `gh pr create` argv：显式 `-R`/`--repo`、`--base`、`--head`，Enterprise 时 host 与 auth 一致；**永不**加入 `gh` 的 `--dry-run`。即使用户未传 `--head`，也要把 Libra 已解析的当前分支名传给 `gh --head`，避免 `gh` 在未 push 分支上提示 push 或 fork。
14. 非 dry-run：`std::process::Command` 执行，stdin 置空/null，捕获 stdout/stderr，超时默认 **120s**（可配置 env，见 §11），stderr 捕获上限建议 **64 KiB**。转发 SIGINT/SIGTERM 到 `gh` 子进程（见 §7.4）。
15. 解析 PR URL；失败 → `CliError`（`LBR-PR-006` 等），stderr 脱敏后可放 `details`（截断）。
16. human 或 `emit_json_data("pr create", …)`；失败走 `CliError` 渲染（JSON 失败在 **stderr**，成功在 **stdout**）。

### 6.2 Ahead 检查（正确性）

目标：head 相对 base 至少有一个可提出的 commit。

| 条件 | 行为 |
| --- | --- |
| base 在本地不可解析（无本地分支且无 `refs/remotes/<remote>/<base>`） | 错误：要求 `libra fetch`（用户显式）或换 `--base`；**第一版不隐式 fetch** |
| 可解析 base OID 与 head OID | 用 Libra 对象图做 ancestor 判断——直接调用 `internal::merge_base::is_ancestor`（public、同步、in-process），勿 shell 出 `git`，也**勿**复用 `push.rs` 内私有的 `is_ancestor`（仅供 `--force-if-includes`） |
| `base..head` 为空——即 head 是 base 的祖先或与之相等（`is_ancestor(head, base)` 为真），head 无独有 commit | 拒绝「no commits to propose」 |
| base 与 head 分叉（双方都有独有 commit，`base..head` 非空） | **允许**创建 PR（常见 feature 落后 main 仍开 PR）；判据只需 `base..head` 非空，即 head 侧有独有 commit |

### 6.3 远端分支状态判断

**禁止**仅依赖 `refs/remotes/<remote>/<head>`。

1. 若本命令内刚执行的 Libra push 成功：以 push 返回的远程更新结果作为 `remote_head_oid == local_head_oid` 的依据，并同步更新本地 `refs/remotes/<remote>/<head>`。
2. 否则：`libra ls-remote --heads <remote> <head>`（或等价 `refs/heads/<head>`）取远端 tip，与**本地 `<head>` 分支的 tip** 比较（当 `--head` 省略或等于当前分支时，该 tip 即当前 HEAD；显式指定其他分支时须解析该分支的本地 tip，切勿硬用 HEAD）。
3. 仅当 1/2 不可用时，可读本地 remote-tracking ref，但必须标 `stale_risk: true`；缺失或不一致时提示 `libra push -u <remote> <head>` 或 `--push`。
4. 第一版**不**调用 `git fetch`，**不**让 `gh` 隐式修正远端 ref。
5. **`--push` 路径的 ls-remote 失败处理**：若 `ls-remote --heads` 因网络错误失败，不阻断 push（push 自身会协商远端并创建/更新分支）；但若失败原因是「远端分支不存在」（与网络错误区分），那正是 push 要解决的，继续 push。两种情况都记 warning。无 `--push` 时 ls-remote 失败才退到 tracking+`stale_risk`，并按不一致语义处理。

**不一致语义**（本地 tip ≠ 远端 tip）：

- 默认：拒绝，提示 push 或 `--push`。
- 第一版**不做** force-push；若远端超前本地，`--push` 走普通 Libra push，失败则表面 tip 保护错误。

### 6.4 Push 路径（仅 `--push`）

| 本地状态 | 调用 |
| --- | --- |
| 当前分支尚无 upstream | `libra push -u <remote> <head>`（`push -u` 的 clap `requires("repository")`，必须带 remote 名） |
| 已有 upstream 且 remote 匹配 | `libra push`（或显式 `libra push <remote> <head>`，与实现统一即可） |
| 已有 upstream 但 remote 与推断目标不一致 | **拒绝**（第一版）并 hint：用 `libra branch --set-upstream-to=<remote>/<head>` 改 upstream，或显式指定匹配的 remote |

实现上应调用 **in-process** push API，而不是再 spawn 一个 `libra` 子进程，以便：

- 测试可注入 fake push；
- 继承同一 `OutputConfig` / 错误契约；
- 避免嵌套 CLI 解析差异。

**当前 API 缺口（实现前必须处理）**：`PushArgs::for_refspecs(repository, refspecs)` 目前把私有字段 `set_upstream` 固定为 `false`，而 `src/command/pr.rs` 作为 sibling module 不能直接写该私有字段。因此第一版若要支持「无 upstream + `--push` 自动 `-u`」，必须在 `src/command/push.rs` 增加一个窄的安全构造器（例如 `PushArgs::for_refspecs_set_upstream(repository, refspecs)`，内部强制 remote 名 + 恰好一个 branch refspec，并复用 `validate_push_args` / `validate_set_upstream_plan`），或在 push 成功后调用 `branch::set_upstream_safe_with_output(local_branch, &format!("{remote}/{head}"), silent_output)` 并明确「远端已推送但 upstream 写入失败」的失败语义、提示与重试方式。禁止在 `pr` 层手写 `PushArgs` 私有字段或 spawn `libra push -u` 子进程。

具体入口（已核对存在）：优先 `command::push::run_push(args, &output).await -> Result<PushOutput, PushError>`（**非渲染执行入口**，但不是纯函数：会协商远端、上传 pack/LFS、更新 remote-tracking refs、必要时写 upstream；返回含 `updates`/`upstream_set` 等的结构化结果，最利于把远端 tip 直接喂给 §6.3 判定）；或 `command::push::execute_safe(args, &output).await -> CliResult<()>`（渲染 + 结构化错误）。用 `PushArgs::for_refspecs(repository, refspecs)` / `for_refspecs_with_lease(..)` 以编程方式构造参数；若需要 `-u`，先补上上段所述安全构造器或显式 upstream setter。**注意**：编程构造绕过 clap，`-u`/`requires("repository")` 的 clap 约束不再自动生效，须自行保证传入了 remote 名与恰好一个分支 refspec（`push` 内部的运行时 `validate_push_args` 仍会校验 `--set-upstream` 只带一个分支）。

### 6.5 PR URL 解析

`gh pr create` 成功时 stdout 通常为 PR URL。仅接受：

```text
https://<host>/<owner>/<repo>/pull/<number>
```

- host 必须与解析 remote 得到的 host 一致（防异常输出被当成成功）。
- 退出码 0 但无法解析 URL：
  - human：打印 `gh` 成功信息 + 无法解析 URL 的 warning/错误说明；
  - JSON/machine：**不得**输出残缺成功 schema → `CliError`（`LBR-PR-006`）。
- 「PR already exists」：尽量从 `gh` 输出/二次只读查询解析已有 URL；失败则专用错误 + hint `gh pr view` / 未来 `libra pr view`。
- 超时、SIGINT 或 `gh` 被信号杀死（OOM 等）：**不**假设 PR 未创建；hint 用户用 GitHub UI 或 `gh pr list --head <branch>` 核对。非 0 退出（含信号死亡）一律走 `LBR-PR-006`，stderr 脱敏后入 `details`（截断）。
- 退出码 0 但 stdout 为空：同「无法解析 URL」处理（`LBR-PR-006`），不得输出残缺成功 schema。

### 6.6 参数映射（Libra → `gh`）

| Libra | `gh` | 备注 |
| --- | --- | --- |
| `--base main` | `--base main` | 纯分支名，无 remote 前缀 |
| `--head feature/x` | `--head feature/x` | 同仓库；无 `owner:` |
| `--title` | `--title` | 与 `--fill` 第一版互斥 |
| `--body` | `--body` | 与 `--fill`、`--body-file` 互斥 |
| `--body-file PR.md` | `--body-file PR.md` | 规范化路径；拒绝 `-` |
| `--draft` | `--draft` | |
| `--fill` | `--fill` | |
| `--web` | `--web` | 与 dry-run / JSON 互斥 |
| （内部） | `--repo [HOST/]OWNER/REPO` | 始终显式；GHE 用 `HOST/OWNER/REPO` |
| （内部） | `--head <branch>` | 始终显式传入 Libra 已解析 head，即使用户未写 `--head`；用于禁用 `gh` 的 push/fork prompt 路径 |
| **禁止** | `--dry-run` | Libra dry-run 自实现 |
| **禁止** | 隐式 push 相关交互 | 通过已 push 前置 + Libra `--push` 避免 |

后续可透传（仍需 argv 测试）：`--reviewer` / `--assignee` / `--label` / `--milestone` / `--project`。

---

## 7. 安全边界

### 7.1 进程与 argv

使用 `std::process::Command` **逐参数**传 argv，禁止 shell 拼接：

```rust
Command::new(&gh_path)
    .arg("pr")
    .arg("create")
    .arg("--repo")
    .arg(&repo_slug) // host/owner/repo or owner/repo
    .arg("--base")
    .arg(&base)
    .arg("--head")
    .arg(&head)
    .stdin(Stdio::null())
    // stdout/stderr piped; kill on timeout
    ;
```

禁止：

```rust
// 禁止：sh -c "gh pr create --title ..."
// 禁止：Command::new("gh").arg(format!("pr create --title {title}"))
```

### 7.2 输入与路径

- `--body-file`：规范化路径；存在、是普通文件、可读；大小上限默认 **512 KiB**（超限拒绝）；拒绝 `-` 与非文件。**符号链接**：跟随一层（`is_file` 为真即可），但若解析后指向仓库外敏感路径，错误信息不得回显目标绝对路径（防路径泄露）；v1 不拒绝 symlink 本身（用户显式指定路径即视为授权读取其内容作为 PR body）。
- 允许仓库外路径，但错误信息避免泄露无关敏感目录树。
- branch / title / body / label 等只作 argv 值；拒绝空 branch、含 `NUL`/换行、无法被 Libra ref 规则接受的 branch 名。`--title ""` / `--body ""`（空串）按 §5.3 处理。
- remote URL：拒绝 `file://`、纯本地路径、无 host、被误判为 GitHub 的任意 SSH host；  
  - GitHub.com：host 必须是 `github.com`（大小写按规范化）；  
  - Enterprise：host **仅**来自 remote URL，不得从 title/body/环境猜测。  
  - **`GH_HOST` 环境变量**：`gh` 会读取 `GH_HOST` 作为默认 host。若 `GH_HOST` 与 remote 推断的 host 不一致，`gh auth status --hostname <remote-host>` 仍以 remote-host 为准（Libra 始终显式传 `--hostname`），但 `gh pr create` 的 `--repo` 必须含 GHE 的 host 前缀以避免 `gh` 回退到 `GH_HOST`。Libra 不读 `GH_HOST`，只在错误信息中提示用户检查 `GH_HOST` 是否干扰了 `gh` 行为。

### 7.3 凭据与日志

- 不调用 `gh auth status --show-token`。
- 不打印 `GH_TOKEN` / `GITHUB_TOKEN` / authorization header / cookie / credential helper 输出。
- 捕获的 `gh` stderr 经敏感信息过滤后再进入 human 错误或 JSON `details`（截断）。**具体过滤口径**（落地时实现并加单测）：正则替换已知 GitHub token 格式（`ghp_`、`gho_`、`ghu_`、`ghs_`、`ghr_`、`github_pat_` 前缀 + `[A-Za-z0-9_]{36,}`）、`Authorization:` header 行（含 `Bearer`/`token` 值）、`set-cookie` 行；对非已知模式保守截断而非猜测。过滤后仍超 stderr 上限则整体替换为 `"<redacted: possibly contains secrets>"`。
- dry-run 的 `gh_args`：`--body` 值替换为 `"<redacted>"`；`--body-file` 只展示安全 basename 或已规范化展示路径。
- JSON `data` 中**不得**出现明文 `body`：只暴露 `has_body: true|false`（可选 `body_len`），与 dry-run `gh_args` 的脱敏保持一致（见 §9.2）。`title` 视为非敏感，可原样输出。
- 不把完整 PR body 写入 tracing 默认级别日志。

### 7.4 超时与资源

| 项 | 默认 | 说明 |
| --- | --- | --- |
| `gh` 进程超时 | 120s | 超时后先 SIGTERM、5s 内未退出再 SIGKILL（Unix）；非 Unix 平台退化为 `std::process::Child::kill()`（等价强制终止，无优雅信号阶段）；错误可行动 |
| SIGINT/SIGTERM 转发 | —— | 用户按 Ctrl-C 时 Libra 应转发 SIGINT 到 `gh` 子进程（Unix），让 `gh` 先清理再退出；非 Unix 用 `Child::kill()`。超时与信号死亡都不假设 PR 未创建（§6.5） |
| stderr 捕获 | 64 KiB | 超长截断并标记 truncated |
| body-file 大小 | 512 KiB | 防意外大文件进入 argv/日志 |
| 可选 env（落地时命名并写 README） | `LIBRA_PR_GH_TIMEOUT_SECS` 等 | 非法值 → 硬错误，不静默回落 |

### 7.5 测试必须覆盖的注入场景

fake `gh` 证明未走 shell：title/body 含空格、引号、`;`、`$()`、换行、Unicode；PATH 前置恶意 `gh` 脚本只被当作 argv 接收器。

### 7.6 `gh` 可执行路径信任边界

Libra 通过 PATH 解析 `gh`，解析一次并报告实际路径。**威胁模型**：PATH 上被替换的 `gh` 既是 argv 接收器，也持有 `gh auth` 的 token 存储访问权——一个木马 `gh` 可窃取 GitHub token。这等同于「PATH 上的 `git` 被替换」的既有风险（用户信任自己的 PATH）。Libra **不**做完整性校验（签名/哈希门禁 v1 非目标），只在错误信息中回显解析到的绝对路径，让用户自行判断。可选 `pr.ghPath` 配置（阶段 4+）允许固定路径以缩小信任面。

---

## 8. 功能正确性与接口兼容性

### 8.1 正确性不变量

1. 创建 PR 时，本地 `<head>` 分支的 tip 必须与其远端 tip 一致（刚被成功 Libra push 确认同步，或经 §6.3 判定一致）。当 `--head` 省略或等于当前分支时，该 tip 即当前 HEAD；`--push` 路径本就要求 head=当前分支（§5.2），故此时二者等价。
2. base/head 比较在同一 remote/repository；第一版不跨 fork。
3. 无 ahead（head 相对 base 无独有 commit）不得创建。
4. `--push` 不得推送非当前分支。
5. `--dry-run` 无远端副作用（无 push、无 `gh pr create`、无 browser）。
6. 全局 JSON/machine **成功**（stdout）：`ok: true`、`command`、`data` 含 §9 schema 必选字段。
7. 全局 JSON/machine **失败**（stderr）：`CliErrorReport`（`ok: false`、`error_code: "LBR-*"`、`category`、`exit_code`、`severity`、`message`、`hints`），不依赖解析 human 文本。
8. 非 `--web` 路径在无 `--fill` 且无 `--title` 时不得调用 `gh`（防交互阻塞）。
9. `--offline` 下不得调用 `gh` 或发起任何远程请求。
10. 非 dry-run/web 的 `gh pr create` 必须始终带显式 `--head`；fake `gh` 测试要断言没有任何路径遗漏，防止 `gh` 回到提示 push 或 fork 的交互流程。
11. **预检排序不变量**：`gh` 存在性 + 版本门禁 + auth 状态检查必须前置于任何 Libra push 副作用（`--push` 路径）。即：`--push` 路径下若 `gh` 缺失/版本过低/未认证，分支**不得**被推送。fake `gh` / mock push 测试须断言此顺序。
12. **unborn HEAD 不得创建 PR**：空仓库（HEAD 指向尚无 commit 的分支）无 head tip，ahead 无法判定，直接拒绝。

### 8.2 CLI 兼容性

- `libra pr` 是 Libra extension，**不是** Git-compatible 命令。
- 未来新选项不得改变已有选项语义。
- 第一版拒绝不确定组合，优先稳定契约。
- 文档、help、`COMPATIBILITY.md`、compat 测试同 PR 更新。

### 8.3 `gh` 版本

- **临时最低版本**：`gh >= 2.40.0`（实现阶段用 fake 矩阵 + 一台真机验证后可上调；不得依赖低于该版本不存在的 flag）。实现 PR 必须在 CI 与至少一台目标平台真机上验证后，把最终值写入 `docs/commands/pr.md` 与 `COMPATIBILITY.md`。
- 过低时：

```text
error: GitHub CLI version is unsupported
hint: upgrade gh to version 2.40.0 or newer (https://cli.github.com/)
```

- 若后续使用新 flag，必须提高最低版本并更新本文与用户文档。

### 8.4 与 `libra open` 的关系

- `open` 只负责把 remote 变成浏览器 URL；`pr create` 负责创建 PR。
- URL 解析可复用 `open` 中 SCP/SSH/HTTPS 变换思路，但 PR 路径需要 **owner/repo/host** 结构化结果与 GitHub 判定，建议抽到 `internal/github` 而不是依赖 `open` 的 web URL 字符串。

---

## 9. JSON 输出与错误处理

### 9.1 通道约定（全仓契约）

| 模式 | 成功 | 失败 |
| --- | --- | --- |
| human | stdout 文案 | stderr：`fatal:`/`error:` + hints；非 TTY 可附 `Error-Code:` + JSON 行（见 `docs/error-codes.md`） |
| `--json` / `--machine` | stdout：`emit_json_data("pr create", …)`（内部写 `{ ok, command, data }` 信封；`write_json_command_envelope` 是它的**私有** helper，勿直接调用） | stderr：`CliErrorReport` JSON |

脚本应：检查 exit code → 成功 parse stdout envelope；失败 parse stderr 最后一行 JSON 的 `error_code`。

### 9.2 成功 schema（稳定字段）

`libra --json pr create --fill` 示例：

```json
{
  "ok": true,
  "command": "pr create",
  "data": {
    "provider": "github",
    "backend": "gh",
    "remote": "origin",
    "repository": "owner/repo",
    "host": "github.com",
    "base": "main",
    "head": "feature/my-change",
    "url": "https://github.com/owner/repo/pull/123",
    "number": 123,
    "pushed": true,
    "dirty": false,
    "draft": false,
    "dry_run": false,
    "stale_risk": false
  }
}
```

**必选**（非 dry-run 成功）：`provider`、`backend`、`remote`、`repository`、`host`、`base`、`head`、`url`、`number`、`pushed`、`dirty`、`draft`、`dry_run`（=`false`）、`stale_risk`。`dry_run` 与 `stale_risk` **始终出现**，便于消费者按稳定 schema 解析，无需按 dry-run/非 dry-run 分支判断字段是否存在。  
**dry-run 成功**：无 `url`/`number`；含 `dry_run: true`、`would_push`、`auth_checked`、`gh_args`（脱敏）。**正文（body）绝不以明文进入 JSON**：只输出 `has_body: true|false`（可选 `body_len`），与 §7.3 对 `--body`/`gh_args` 的脱敏一致；`--body-file` 只展示规范化展示路径或安全 basename。`title`/`draft`/`fill`/`web` 等非敏感已解析选项可原样输出（若用户提供）。

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
    "host": "github.com",
    "base": "main",
    "head": "feature/my-change",
    "fill": true,
    "draft": false,
    "web": false,
    "has_body": false,
    "would_push": false,
    "auth_checked": false,
    "dirty": false,
    "stale_risk": false,
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

### 9.3 失败 schema

对齐 `CliErrorReport`（**不是**嵌套 `error.code` 点分串）：

```json
{
  "ok": false,
  "error_code": "LBR-PR-002",
  "category": "auth",
  "exit_code": 128,
  "severity": "fatal",
  "message": "GitHub CLI is not authenticated for github.com",
  "hints": [
    "run `gh auth login --hostname github.com`"
  ]
}
```

`category` 必须是 `CliErrorCategory::as_str()` 之一：`cli` / `repo` / `conflict` / `network` / `auth` / `io` / `internal` / `warning`。

关于其余字段（以 `src/utils/error.rs` 的 `CliErrorReport` 为准）：

- `severity` 取值由错误 *kind* 决定，只有 `"fatal"` 与 `"error"` 两种：致命错误（`CliErrorKind::Fatal`，如上面的 auth 失败）为 `"fatal"`，用法/软失败（含 `cli` category 的互斥参数错误 `LBR-PR-005`，exit 129）为 `"error"`。消费者**不得**假设 `severity` 恒为 `"fatal"`；分支逻辑应依据 `error_code` / `category`，而非 `severity`。
- 序列化时还可能出现两个可选字段：`usage`（用法串，`skip_serializing_if` 为空时省略）与 `details`（`BTreeMap`，用于放脱敏后截断的 `gh` stderr 等，空时省略）。稳定契约只依赖 `ok`/`error_code`/`category`/`exit_code`/`severity`/`message`/`hints`；`usage`/`details` 属可选增量。

**退出码**（默认 Git-standard，见 `docs/error-codes.md`）：

| category | 默认 exit |
| --- | --- |
| `cli`（互斥参数、用法） | **129** |
| 其他 fatal | **128** |

### 9.4 建议稳定错误码

落地时必须走 `StableErrorCode` 扩展流程（`src/utils/error.rs` 注释清单 + `docs/error-codes.md` + `compat_error_codes_doc_sync`）。可新建 `LBR-PR-*` 或明确复用现有码；下表为**推荐新建**映射：

| 码 | category | 默认 exit | 场景 | 可复用 |
| --- | --- | --- | --- | --- |
| `LBR-PR-001` | `internal` | 128 | `gh` 未安装或版本过低 | 无直接等价 |
| `LBR-PR-002` | `auth` | 128 | `gh auth status --hostname` 失败 | 近 `LBR-AUTH-001` |
| `LBR-PR-003` | `repo` | 128 | 非 GitHub remote / 无法解析 owner/repo/host | 近 `LBR-REPO-003` |
| `LBR-PR-004` | `repo` | 128 | head 未 push / OID 不一致且无 `--push` | 无 |
| `LBR-PR-005` | `cli` | **129** | 互斥选项、缺 fill/title、body-file 非法 | 近 `LBR-CLI-002` |
| `LBR-PR-006` | `network` | 128 | `gh pr create` 非 0、超时、URL 解析失败 | 近 `LBR-NET-001` |
| `LBR-PR-007` | `repo` | 128 | 无 ahead、`--require-clean` dirty、detached/unborn HEAD、`--base` 与 `--head` 同名 | 近 `LBR-REPO-003` |

> `gh` 未安装属于运行环境不满足内部依赖，不是用户 CLI 参数错误，因此 `LBR-PR-001` 推荐 `internal` 而非 `cli`。实现时写死该 category 并测 Display pin。
>
> **`LBR-PR-007` category 说明**：一个 `StableErrorCode` 变体必须映射到**恰好一个** category。无 ahead / detached/unborn HEAD / `--base`==`--head` 都是仓库状态问题（head 无可提出 commit、HEAD 不可解析），归 `repo`（exit 128）。`--require-clean` 的 dirty 拒绝虽带「操作被阻止」意味，但根因仍是工作区状态，同归 `repo` 以保持单码单 category；若后续需要区分「操作被阻止」语义，应另建 `LBR-PR-008`（`conflict`）而非让 `LBR-PR-007` 承载两个 category。

### 9.5 Human 错误示例

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

```text
error: option conflict: --web cannot be used with --json or --machine
hint: remove one of the options and retry
```

```text
error: title is required unless --fill or --web is set
hint: pass --fill, or --title "...", or use --web
```

```text
error: failed to create GitHub pull request
hint: gh reported an error; verify repository permissions and branch protection settings
```

```text
error: branch was pushed but pull request was not created
hint: the remote branch is up to date; fix the GitHub error and re-run without --push, or open the compare URL
```

（push 成功 + PR 失败：**不回滚 push**。注：`gh` 缺失/版本过低/auth 失败现已前置于 push（§6），故本场景**仅**发生在 `gh pr create` 自身失败时——而非可预防的前置检查失败。）

---

## 10. 性能与效率

非热路径；目标是少网络、少进程：

- 互斥、repo 状态、dirty、ahead 在调用 `gh` 前完成。
- **fail-fast 排序**：`gh` 存在性 + 版本检查（本地、零网络）先于任何网络操作（auth、ls-remote、push、`gh pr create`），避免无 `gh` 时白白触网。auth 检查先于 push，避免无谓推送。
- `--dry-run`：本地推断 + `gh --version`；不 auth、不 create、不 push。
- 不默认 fetch / 不默认 browser / 不默认 push。
- 每次执行最多：一次 `gh --version`、一次 `gh auth status`、一次 `gh pr create`；base 探测最多一次 `ls-remote --symref`；head 探测最多一次 `ls-remote --heads`（push 刚成功则可省）。auth 与 ls-remote 是两次独立网络往返，v1 不合并。
- JSON 不读完整 diff，只做 commit 图/OID 比较。

---

## 11. 可靠性与容错

| 场景 | 行为 |
| --- | --- |
| stdin | 外部 `gh` 使用 `Stdio::null()`，避免意外阻塞 |
| auth 失败 | 不继续 `gh pr create`；**且因 auth 前置于 push，分支不会被推送** |
| push 成功 / PR 失败 | 不回滚 push；明确「已推送未建 PR」（仅 `gh pr create` 自身失败，非可预防的前置检查） |
| PR already exists | 尽量返回已有 URL；否则专用错误 |
| 超时 / 中断 / 信号死亡 | 不假设 PR 未创建；hint 核查（§6.5/§7.4） |
| 非 TTY + 需要交互 | 拒绝（缺 fill/title，或 `--web` 无 GUI） |
| `GH_TOKEN` 环境认证 | 允许（`gh` 行为）；Libra 不记录 token 值 |
| `gh` 意外 prompt（未被显式 flag 阻断） | stdin 为 null → `gh` 读到 EOF 且 `isatty(stdin)` 为假，`gh` 自退不 prompt；无需额外 env（`gh` 无 `GH_PROMPT_DISABLED` 等非交互 env） |
| unborn HEAD | 拒绝（无 head tip） |

可配置超时 env 名称在实现 PR 中写入 `docs/commands/pr.md` 与 README；非法值硬错误。

---

## 12. 兼容性与互操作性

| Remote 形态 | 第一版 |
| --- | --- |
| `git@github.com:owner/repo.git` | 支持 |
| `https://github.com/owner/repo.git` | 支持 |
| `ssh://git@github.com/owner/repo.git` | 支持 |
| `ssh://git@github.com:22/owner/repo.git`（显式端口） | 支持（解析 host 时忽略端口，host=`github.com`） |
| `git@github.example.com:owner/repo.git` 等 GHE | 支持（`gh auth status --hostname` 成功为前提） |
| GitLab / Gitea / 未知 host | 拒绝 |
| fork `--head owner:branch` | 拒绝 |

- CI：`libra --machine pr create`，分支逻辑依赖 `error_code`，不依赖 human message。
- `COMPATIBILITY.md` 行：`pr | intentionally-different | Libra GitHub extension via gh; not a Git command`。

---

## 13. 可扩展性与可维护性

### 13.1 建议分层

```text
src/command/pr.rs              CLI 参数、互斥、输出、错误展示、PR_EXAMPLES
src/internal/github/mod.rs     remote 识别、owner/repo/host 解析（`open` 后续可复用该模块，但 v1 保持 `open` 行为不变，避免回归）
src/internal/github/gh.rs      gh 路径/版本、auth status、pr create、超时与脱敏
src/internal/pr/mod.rs         前置校验、base/head 推断、push 编排、调用 provider
```

**落地时复用的既有 in-process API（第四轮已逐一核对存在）**：

- push：`command::push::run_push(args, &output).await -> PushOutput`（非渲染执行入口，**有远端/ref 写入副作用**，首选）或 `execute_safe`；参数用 `PushArgs::for_refspecs(repository, refspecs)` 构造（编程构造绕过 clap，须自带 remote + 恰好一个 refspec；`-u` 需先补安全 setter，见 §6.4）。
- ahead / 祖先判定：`internal::merge_base::{is_ancestor, merge_bases}`（public、同步）；**不要**用 `push.rs` 内私有的 `is_ancestor`。目前**没有**现成 ahead 计数器——本命令只需布尔判据故无需自行走图。
- remote / upstream 解析：`ConfigKv::get_remote(&branch)` 读 `branch.<name>.remote`，`branch.<name>.merge` 与符号 ref `refs/remotes/<remote>/HEAD` 用于 base 推断（§5.3）。
- 输出：`utils::output::emit_json_data`（信封含 `command` 键）；错误：`utils::error::CliError` + `StableErrorCode`。
- GitHub 结构化解析放新建的 `src/internal/github/`（当前树中**不存在** `internal::github`，无冲突；`open` 只产出 URL 字符串、无 owner/repo/host 结构，不可直接复用）。

### 13.2 Provider 边界

第一版可不引入 trait，但 **必须** 可注入 `gh` runner（测试 fake）。若引入：

```rust
trait PullRequestProvider {
    async fn create(&self, request: CreatePullRequestRequest) -> Result<CreatePullRequestResponse>;
}
```

**编排字段不要塞进 provider 请求**。推荐拆分：

```rust
/// 编排层（Libra 命令）
struct CreatePullRequestPlan {
    remote: String,
    push: bool,
    dry_run: bool,
    require_clean: bool,
    web: bool,
    // ... 推断结果
}

/// Provider 请求（仅 PR API 语义）
struct CreatePullRequestRequest {
    host: String,
    repository: String, // OWNER/REPO or HOST/OWNER/REPO per gh
    base: String,
    head: String,
    title: Option<String>,
    body: Option<String>,
    body_file: Option<PathBuf>,
    draft: bool,
    fill: bool,
    web: bool,
}
```

当前只实现 `GhGitHubProvider`；未来 `NativeGitHubProvider` / `GitLabProvider` / `GiteaProvider`。

---

## 14. 合规性与标准符合性

- 不伪装 Git 标准命令；`COMPATIBILITY.md` 标 extension。
- 不改变 commit / 签名 / DCO / ref 规则。
- 不新增 token 持久化面；凭据由 `gh`（及用户环境中的 `GH_TOKEN` 等）管理。
- 日志与错误不得泄露 token、cookie、authorization、credential helper 或用户正文敏感段。
- Live 测试必须显式 env gate：`LIBRA_TEST_GITHUB_TOKEN` + `LIBRA_TEST_GITHUB_NAMESPACE` + `--features test-network`；**禁止**发明不存在的 `test-live-github` feature。
- 新增 `StableErrorCode` 时同步 `docs/error-codes.md` 与 compat 守卫。

---

## 15. 推荐 UX

```bash
libra pr create --fill --push
```

1. 检查当前分支与 ahead。  
2. Libra push 当前分支（必要时 `-u <remote> <branch>`）。  
3. `gh pr create --fill`（显式 `--repo`）。  
4. 输出 PR URL。

```text
Pushed feature/my-change to origin.
Created pull request:
https://github.com/owner/repo/pull/123
```

不自动 push：

```bash
libra pr create --fill
```

未 push 时：

```text
error: current branch 'feature/my-change' has not been pushed to origin
hint: run `libra push -u origin feature/my-change` or retry with `libra pr create --push --fill`
```

---

## 16. 测试策略

### 16.1 单元测试

- GitHub URL 解析：HTTPS / SCP / SSH / GHE / SSH 显式端口 / 非 GitHub 拒绝 / `file://` 拒绝。
- base / head / remote 推断（含默认分支 fallback 顺序）。
- ahead：无独有 commit 拒绝；分叉但 head 有独有 commit 允许；`--base`==`--head` 拒绝。
- unborn HEAD（空仓库）拒绝。
- argv 组装（无 shell）；特殊字符 title/body。
- argv 组装始终含显式 `--head`（用户未传时也使用 Libra 推断的当前分支），且不包含未实现的 `gh` 扩展选项。
- 互斥表全覆盖（含 fill/title、web/json、body-file `-`）。
- dry-run：不 create、不 push、不 browser；`gh_args` 脱敏（`--body` → `<redacted>`，token 模式被过滤）。
- URL 解析与 host 一致性；成功但无 URL / 空 stdout → JSON 错误。
- 成功 envelope 与 `CliErrorReport` Display/JSON pin。
- stderr 脱敏：构造含 `ghp_*`/`Authorization: Bearer ...` 的伪 stderr，断言过滤后不含明文。

### 16.2 集成测试（L1，fake `gh`）

- fake `gh` 置于临时 `PATH`，记录 argv。
- 成功 URL → human 与 `libra --json pr create`。
- auth 失败 → `LBR-PR-002` + hints；**断言分支未被推送**（mock push 未被调用）。
- 非 0 退出 → 不吞错误；stderr 含伪 token → 过滤。
- PR already exists 路径。
- `--push` → 调用 in-process Libra push（mock），**不** spawn 真 push 到外网。
- 无 upstream + `--push` → 覆盖安全 `set_upstream` 路径，断言传入 remote 名、单一 branch refspec，并在成功后得到 `upstream_set` 或等价 config 写入。
- stale tracking ref → 走 ls-remote 或 `stale_risk`，不误判已同步。
- dirty 默认 warning / `--require-clean` 拒绝。
- 非 TTY `--web` 错误。
- **预检排序断言**：`gh` 缺失/版本过低 + `--push` → 断言 push **未发生**（mock push 未被调用），错误为 `LBR-PR-001`。auth 失败 + `--push` → 同理断言 push 未发生。
- `--push` + ls-remote 网络失败 → 不阻断 push，记 warning 后继续 push。

### 16.3 可选 L2 live

```bash
# 需 LIBRA_TEST_GITHUB_TOKEN + LIBRA_TEST_GITHUB_NAMESPACE
cargo test --features test-network --test pr_github_live_test
```

登记 `tests/INDEX.md`（Wave 3 / network）；默认 CI 不跑真网。Live 测试不得在日志/输出/`details` 中回显 `LIBRA_TEST_GITHUB_TOKEN` 明文（遵循 §7.3 脱敏口径）。

### 16.4 验收门槛

- `cargo +nightly fmt --all --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all`
- `tests/INDEX.md` 新 target 行（约定，无强制守卫）
- CLI help（`PR_EXAMPLES` + `after_help` → `compat_help_examples_banner`；并把 `pr` 加入该守卫 `tests/compat/help_examples_banner.rs` 里**硬编码**的 `VISIBLE_COMMANDS` 数组，否则静默跳过）
- **两份**命令文档（缺一必挂 CI）：
  - `docs/commands/pr.md`（`compat_command_docs_examples_section` 要求含 `## Examples` / `## Common Commands` 标题）
  - `docs/development/commands/pr.md` + 在 `docs/development/commands/README.md` 增行（`compat_matrix_alignment` 的 `command_development_readme_matches_public_cli_surface` 检查）
  - 建议附 `docs/commands/zh-CN/pr.md`（无守卫但保持 EN↔zh 平价惯例）
- `docs/error-codes.md`（新 `LBR-PR-*` 与 `compat_error_codes_doc_sync` 同步）、`COMPATIBILITY.md` 新增 `| pr | intentionally-different | … |` 行（含首尾竖线，满足 `compatibility_matrix_matches_cli_commands`）
- `src/cli.rs` `ROOT_AFTER_HELP` 的 `Remote And Cloud` 组加入 `pr`（`root_after_help_lists_every_visible_command` 单测强制）

---

## 17. 文档与落地清单（实现 PR）

| 项 | 要求 |
| --- | --- |
| `src/command/pr.rs` | `PR_EXAMPLES` + `#[command(after_help = …)]` |
| `src/cli.rs` | 注册 `Commands::Pr`；`ROOT_AFTER_HELP` 的 `Remote And Cloud` 分组行；`--offline` 互斥处理 |
| `src/command/pr.rs` | 调用 `gh` 时 stdin 置 `Stdio::null()`（§11）；预检排序：gh 版本/auth 前置于 push（§6）；dry-run 在 gh 版本检查后立即分支（§6.1 步骤 9） |
| `tests/compat/help_examples_banner.rs` | 把 `pr` 加入硬编码 `VISIBLE_COMMANDS` 数组 |
| `docs/commands/pr.md` | Examples / Common Commands 小节（`compat_command_docs_examples_section`）；`--offline` 限制说明 |
| `docs/development/commands/pr.md` + `docs/development/commands/README.md` | 命令页 + README 增行（`command_development_readme_matches_public_cli_surface`） |
| `docs/commands/zh-CN/pr.md` | zh-CN 平价页（无守卫，建议同 PR 补齐） |
| `COMPATIBILITY.md` | `\| pr \| intentionally-different \| … \|`（含首尾竖线） |
| `docs/error-codes.md` | `LBR-PR-*` 或复用映射表（`compat_error_codes_doc_sync`） |
| `tests/INDEX.md` | integration / live 行（约定） |
| README 命令列表 | 若有命令索引则更新 |

---

## 18. 分阶段落地

| 阶段 | 能力 | 出口标准 |
| --- | --- | --- |
| **1** | `libra pr create --dry-run` | 推断 + 互斥 + gh 版本检查 + 脱敏 argv；无 push/create |
| **2** | 最小创建：`--base`/`--title`/`--body`/`--fill`；分支须已 push | fake `gh` + JSON 失败路径 |
| **3** | `--push` → in-process Libra push | push 成功 PR 失败文案；不回滚 |
| **4** | `--draft` / reviewer / label / `--web`；`pr status|view|checkout` | 元数据 argv 测试；非 TTY web 拒绝 |
| **5** | fork PR、native API、GitLab/Gitea | 单独设计 push remote / head owner / URL schema |

---

## 19. 结论

长期最优解是 Libra 提供稳定门面：

```bash
libra pr create --push --fill
```

内部规则：

1. Libra = VCS + push；`gh` = GitHub PR API。  
2. 不经 shell 调用 `gh`；超时、脱敏、argv 注入防护齐全。  
3. 默认不静默 push；显式 `--push`。  
4. 机器输出仅全局 `--json`/`--machine` + `CliErrorReport`。  
5. 第一版：GitHub 同仓库；拒绝 fork 与非 GitHub remote。  
6. 远端状态：push 结果或 `ls-remote --heads`，不单信 tracking。  
7. Dry-run 纯 Libra，不映射 `gh --dry-run`。  
8. 非 web 路径必须 `--fill` 或 `--title`，避免交互阻塞。  
9. `--offline` 下直接拒绝，不尝试调用 `gh`。
10. 错误可行动且不泄露凭据。
11. `gh` 存在/版本/auth 检查前置于 push，避免可避免的「已推送未建 PR」。

---

## 附录 A：十二维评审与前后冲突扫描

### A.1 评审矩阵（第二轮 + 第三轮补丁 + 第四/第五轮代码与 `gh` 契约核对 + 第六轮十二维独立分析 + 第七轮补遗，2026-07-09）

| 维度 | 结论 | 主要依据 / 残留风险 | 文档处置 |
| --- | --- | --- | --- |
| 合理性 | **通过** | PR 非 Git 协议；`gh` 降认证复杂度 | 维持边界 |
| 可行性 | **有条件通过** | 分阶段 + fake `gh`；`gh>=2.40` 为临时地板，落地前再实测；`PushArgs` upstream setter 需先补窄 API | §6.4 §8.3 |
| 完整性 | **有条件通过→已补** | 曾缺 fill/title 强制、gh dry-run 禁令、stdout/stderr 通道、exit 129；第六轮补 unborn HEAD / base==head / 空 title-body / SSH 端口 / 信号死亡 / GH_HOST；第七轮补 `--web` 成功/失败语义 | §5.2 §6 §9 §12 |
| 安全性 | **有条件通过→已补** | argv 无 shell；补超时/stderr 上限/body-file 限制/禁 show-token/禁 body-file `-`；显式 `gh --head`；第六轮钉死 stderr 脱敏口径与 token 模式、PATH 信任边界、symlink | §6.1 §6.6 §7 |
| 功能正确性与接口兼容性 | **已补强** | 全局 JSON；`CliErrorReport`；禁止子命令 `--json`；第六轮钉死 `LBR-PR-007` 单 category | §5 §9 |
| 数据流与控制流 | **已补强→第六轮重排→第七轮 dry-run 分支** | 具名 `ls-remote`；ahead 规则；in-process push；第六轮 gh 存在/版本/auth 前置于 push；**第七轮 dry-run 在 gh 版本检查后立即分支（跳过网络 ls-remote）** | §6 |
| 性能与效率 | **通过** | 非热路径；限制进程/网络次数；第六轮 fail-fast 排序；第七轮 dry-run 零网络 | §10 |
| 可靠性与容错 | **有条件通过→已补** | push/PR 部分失败、超时不假设、已存在 PR；第六轮补 SIGINT 转发 / auth-before-push 语义；第七轮修正 `GH_PROMPT_DISABLED` 不存在 | §11 |
| 兼容性与互操作性 | **有条件通过** | GHE host 与 auth 对齐；fork 延后；第六轮补 SSH 显式端口 / `GH_HOST` | §12 |
| 可扩展性与可维护性 | **通过** | 分层 + 可注入 runner；plan/request 拆分 | §13 |
| 合规性与标准符合性 | **有条件通过→已补** | error-codes 流程；test-network gate；COMPATIBILITY；第六轮补 live test token 脱敏 | §14 §17 |
| 前后一致性 | **已修订** | 见 A.2（含第六轮 P35–P41 + 第七轮 P42–P44） | 全文对齐 |

### A.2 冲突与缺口清单

| ID | 严重度 | 问题 | 修订 |
| --- | --- | --- | --- |
| P1 | **高** | 子命令 `--json` 与全仓全局 `OutputConfig` 双轨 | 仅全局 `--json`/`--machine`；成功 `emit_json_data` |
| P2 | **高** | 错误用点分 `error.code` 与 `CliErrorReport`/`LBR-*` 不符 | §9.3–9.4 |
| P3 | **高** | 无 `--fill`/`--title` 时 `gh` 会交互阻塞 CI | §5.2 / 不变量 8 |
| P4 | **高** | 若映射 `gh pr create --dry-run` 可能仍 push | 明确禁止；Libra 自实现 dry-run |
| P5 | 中 | 远端 head 只读 tracking 易过期 | push → ls-remote → stale_risk |
| P6 | 中 | `test-live-github` feature 不存在 | L2：`test-network` + GitHub env |
| P7 | 中 | `--web` 与全局 JSON 互斥未写清 | 互斥表 |
| P8 | 中 | `push -u` 必须带 repository；已有 upstream 时应普通 push | §6.4 |
| P9 | 中 | 成功/失败 JSON 通道（stdout vs stderr）未写 | §9.1 |
| P10 | 中 | 用法错误 exit 应为 129 | §9.3 |
| P11 | 中 | ahead 在 base 本地不存在时行为不明；分叉是否允许 | §6.2 |
| P12 | 低 | `PR_EXAMPLES` / 三 compat 守卫 | §17 |
| P13 | 低 | CreatePullRequestRequest 混入 push/dry_run 破坏分层 | §13.2 |
| P14 | 低 | 评审矩阵曾写 `LBR-NETWORK`（正确为 `LBR-NET-*`） | 已改正 |
| P15 | 低 | P1 原文写「`libra open --json` 均为全局」易误解 | 澄清：`--json` 为 clap `global = true`，推荐 `libra --json open` / `libra --json pr create` |
| P16 | 中 | 未处理全局 `--offline` 与 `pr create` 的冲突 | §5.2 互斥表 / §5.3 / 不变量 9 |
| P17 | 中 | `LBR-PR-001` category 在 `cli`/`internal` 间摇摆 | §9.4 明确为 `internal` |
| P18 | 中 | dry-run JSON schema 未展示已解析选项字段 | §9.2 示例补充 `fill`/`draft`/`web` 等 |
| P19 | 低 | `--require-clean` 的 dirty 定义未明确 | §5.2 补充含 untracked |
| P20 | 低 | 远端 push 成功后如何更新本地状态未明确 | §6.1 第 9 步 / §6.3 第 1 步补充 |
| P21 | 低 | `pr` 在 `ROOT_AFTER_HELP` 中的分组未指定 | §5 明确为 `Remote And Cloud` |
| P22 | **中** | §6.3/§8.1 把「head 分支 tip」等同于「当前 HEAD」；`--head`≠当前分支（非 push）时判定错源 | §6.3 步骤 2 / §8.1 不变量 1 改为「`<head>` 分支本地 tip」 |
| P23 | **中·安全** | §9.2 dry-run 曾要求输出已解析 `body`，与 §7.3 对 body 的脱敏冲突 → 明文正文入 JSON | §9.2/§7.3：JSON 只出 `has_body`（可选 `body_len`），永不出明文 body |
| P24 | 中 | §5.3 步骤 4 `ls-remote --symref` 对 Libra-native/本地远端返回空，原文当作可靠默认分支来源 | §5.3：标注 symref 仅对 advertise 该能力的真 Git 远端有效；步骤 3 `refs/remotes/<remote>/HEAD` 才是主来源 |
| P25 | 中 | §16.4/§17 只列 `docs/commands/pr.md`，漏 `matrix_alignment` 另需的 `docs/development/commands/pr.md` + README 行、及 `help_examples_banner.rs` 硬编码 `VISIBLE_COMMANDS` | §16.4/§17 补全双份文档 + README + VISIBLE_COMMANDS |
| P26 | 低 | §6.2 ahead 中间行「head 不是 base 的严格后代」措辞含糊、与「分叉允许」表面冲突 | §6.2 重述为「`base..head` 为空（`is_ancestor(head, base)`）才拒绝」 |
| P27 | 低 | §9.2 必选字段漏 `dry_run`/`stale_risk`（示例里却有） | §9.2：二者始终出现，纳入必选 |
| P28 | 低 | `severity` 并非恒为 `"fatal"`（cli/用法错误为 `"error"`）；`write_json_command_envelope` 私有 | §9.1/§9.3 更正；分支逻辑依 `error_code`/`category` |
| P29 | 低 | §7.4 超时用 SIGTERM/SIGKILL（Unix-only），未提非 Unix 语义 | §7.4：非 Unix 退化为 `Child::kill()` |
| P30 | **中** | §6 数据流先写「远端 head 状态（push 结果）」再写可选 push，容易让实现先信 tracking/远端状态再 push，顺序含混 | §6：改为远端 head 预检 → 可选 push → `PushOutput` 确认 |
| P31 | **中** | §6.4 暗示 `PushArgs::for_refspecs` 可表达 `-u`，但当前 `set_upstream` 为私有字段且构造器固定 false，`src/command/pr.rs` 无法直接设置 | §6.4：新增实现前必须补安全构造器或显式 upstream setter 的约束 |
| P32 | **中** | §6.4 把 `run_push` 称为「纯函数」，与源码副作用（远端协商、上传、remote-tracking/upstream 写入）冲突，可能误导 dry-run/测试隔离 | §6.4：改为「非渲染执行入口」，明确 async 与 side effects |
| P33 | **中·安全/UX** | `gh pr create` 官方行为：未 fully pushed 时会 prompt push/fork；原文虽拒绝隐式 push，但未把「始终传 `--head`」作为不变量与测试断言 | §1 §4.1 §6.1 §6.6 §8.1 §16：显式 `--head` 成为硬约束 |
| P34 | 低 | 第一版未列 `gh --editor`/`--template`/`--fill-first`/`--fill-verbose` 等未暴露选项，后续实现可能误以为可自由透传 | §3.2 §16.1：列为非目标，要求逐项设计、版本门槛与 argv 测试 |
| P35 | **高·控制流** | §6 数据流把「解析 gh + 版本门禁」与「gh auth status」放在「可选 Libra push」**之后**；`--push` 路径下若 `gh` 缺失/版本过低/未认证，分支已被推送，用户陷入可避免的「已推送未建 PR」。本地检查零网络副作用，应前置于任何写远端副作用 | §6/§6.1：gh 存在/版本/auth 前置于 push；新增决策行 + 不变量 11 + 测试断言 |
| P36 | **高·一致性** | §9.4 `LBR-PR-007` category 写「`conflict` **或** `repo`」——一个 `StableErrorCode` 变体必须映射到恰好一个 category，否则实现时 category/exit 不确定 | §9.4：钉死为 `repo`；dirty 拒绝同归 `repo`，需区分时另建 `LBR-PR-008` |
| P37 | 中·完整性 | 未处理 unborn HEAD（空仓库，HEAD 指向尚无 commit 的分支）：无 head tip，ahead 无法判定 | §5.2/§6.1 第 3 步/§8.1 不变量 12：拒绝 |
| P38 | 中·安全 | §7.3「stderr 经敏感信息过滤」未给出口径（哪些 pattern），实现易遗漏 token 格式 | §7.3：钉死 `ghp_*`/`gho_*`/`ghu_*`/`ghs_*`/`ghr_*`/`github_pat_*` + `Authorization:`/`set-cookie` 正则 + 单测 |
| P39 | 中·完整性 | `--base`==`--head` 未显式处理（ahead 检查会兜底但错误不可读）；空 `--title ""`/`--body ""` 行为未定义 | §5.2/§6.1 第 7 步：base==head 显式拒绝；§5.3：空串语义 |
| P40 | 中·可靠性 | 用户 Ctrl-C 时是否转发 SIGINT 到 `gh` 子进程未写；`gh` 意外 prompt（未被 flag 阻断）时兜底未写 | §7.4/§11：SIGINT 转发 + `Stdio::null()` 非交互兜底（`gh` 无 `GH_PROMPT_DISABLED` env，`isatty(stdin)` 为假即自退） |
| P41 | 低·兼容 | SSH 显式端口 `ssh://git@github.com:22/...` 与 `GH_HOST` 环境变量交互未提 | §12：补端口变体；§7.2：补 `GH_HOST` 说明 |
| P42 | **高·一致性** | §3.1/§10 要求 dry-run「仅本地推断」，但 §6 控制流中 dry-run 经过「远端 head 预检」步骤（触发 `ls-remote --heads` 网络调用），与「仅本地推断」矛盾 | §6/§6.1：dry-run 在 gh 版本检查后立即分支，远端 head 仅查本地 tracking（标 `stale_risk`），不调用 `ls-remote` |
| P43 | 中·完整性 | `--web` 成功/失败输出语义未定义（human 输出什么？exit code？） | §5.3：`--web` 成功输出「Opening browser...」+ compare URL，exit 0；失败 → `LBR-PR-005`/`LBR-IO-*` |
| P44 | 中·正确性 | §11 引用 `GH_PROMPT_DISABLED=1` 作为 `gh` 非交互兜底，但 `gh` 无此 env（`gh` 通过 `isatty(stdin)` 判断交互性，`Stdio::null()` 已足够） | §11：改为 `Stdio::null()` 非交互兜底，删除 `GH_PROMPT_DISABLED` 引用；§17 同步删除 |

**冲突扫描结论**：P1–P4 为落地前必须遵守的契约/正确性约束；P16–P18 为第三轮补全的正确性/完整性约束；P22–P23 为第四轮以代码核对发现的**正确性/安全性冲突**（必须遵守）；P24–P29 为第四轮的完整性/兼容性补强；P30–P34 为第五轮针对 push API、`run_push` 副作用与 `gh` 官方交互行为的落地补强；**P35 为第六轮发现的控制流正确性问题（gh 预检前置于 push，必须遵守）；P36 为前后一致性冲突（单码单 category，必须钉死）；P37–P41 为第六轮完整性/安全/可靠性/兼容性补强；P42 为第七轮发现的 dry-run 与「仅本地推断」矛盾（必须遵守）；P43–P44 为第七轮完整性/正确性补强**。无与「第一版仅 GitHub 同仓库 PR + gh 后端」目标相悖的条款。

### A.3 第一轮已吸收、本轮保留的结论

- Libra=VCS、gh=PR API 分层合理。  
- 默认不静默 push。  
- 机器接口不透传 `gh` human 输出。  
- fake `gh` 为默认 CI 策略。

### A.4 第四轮：以代码为准的逐条核对（2026-07-09）

对文档援引的所有内部契约与 API 逐条比对源码，结论：**绝大多数具体断言与现网代码一致**（含函数名 `push::execute_safe`、`-u` 的 `requires("repository")`、`ROOT_AFTER_HELP` 组名 `Remote And Cloud`、六个可复用 `LBR-*` 码的 category/exit、全局 `--json`/`--machine`/`--offline` 语义等，均已核实存在）。核对同时暴露了上表 P22–P29，均已在正文修订。要点汇总：

| 核对项 | 结果 | 处置 |
| --- | --- | --- |
| `CliErrorReport` 字段（`ok`/`error_code`/`category`/`exit_code`/`severity`/`message`/`hints`） | 与序列化名一致；另有可选 `usage`/`details` | §9.3 补 `usage`/`details` 说明 |
| `CliErrorCategory::as_str()` 8 值、cli→129/fatal→128 映射 | 一致 | 维持 |
| `severity` 取值 | 仅 `"fatal"`/`"error"`（kind 决定，非 category） | §9.3 更正 |
| `LBR-CLI-002`/`AUTH-001`/`REPO-003`/`NET-001`/`CONFLICT-002`/`IO-001` | 均存在且 category/exit 与文档一致；**`LBR-PR-*` 尚不存在** | 维持复用映射；落地走扩展流程 |
| 全局 `--json`（`-J`，`require_equals`，`pretty/compact/ndjson`）、`--machine`（另含 color=never/progress=none）、`--offline`、`--max-connections` | 存在；`--json=compact` 需 `=` | §9.1 已用 `emit_json_data` 表述 |
| `emit_json_data`（信封含 `command`）；`write_json_command_envelope` | 前者 public、后者**私有** | §9.1 更正 |
| `push::run_push→PushOutput` / `execute_safe` / `PushArgs::for_refspecs` | 均存在（编程构造绕过 clap）；但 `for_refspecs` 不能设置私有 `set_upstream` | §6.4/§13.1 具名并补 API 缺口 |
| `internal::merge_base::{is_ancestor, merge_bases}` | public、同步、in-process；无 ahead 计数器；勿用 `push.rs` 私有 `is_ancestor` | §6.2/§13.1 具名 |
| `ls-remote --heads` / `--symref` | 均存在；**`--symref` 仅回显协议 advertise 的能力，对 Libra-native/本地远端为空** | §5.3 标注能力边界 |
| `branch.<name>.{remote,merge}` / `refs/remotes/<remote>/HEAD` | 与 Libra 存储方式一致（`merge` 存 head 自身上游） | §5.3 步骤 2 降级为 best-effort |
| `open` 无结构化 owner/repo/host；`src/internal/github` 不存在 | 一致（greenfield，无冲突） | §13.1 确认新建 |
| compat 守卫 `root_after_help_lists_every_visible_command`/`compat_help_examples_banner`（硬编码 `VISIBLE_COMMANDS`）/`compat_command_docs_examples_section`（查 `docs/commands/`）/`compat_matrix_alignment`（另查 `docs/development/commands/` + README）/`compat_error_codes_doc_sync` | 均存在且如述 | §16.4/§17 补全双份文档 + VISIBLE_COMMANDS |

### A.5 第五轮：`gh` 官方契约 + push API 可实现性核对（2026-07-09）

本轮补充两类实现前风险：外部 `gh` 的真实交互行为，以及内部 push API 的可调用边界。

| 核对项 | 结果 | 处置 |
| --- | --- | --- |
| `gh pr create` 未 fully pushed 时行为 | 官方手册说明会提示 push 分支，并可能提供 fork base repo；`--head` 可显式跳过 fork/push 行为 | §1/§4.1/§6.1/§6.6/§8.1：始终传显式 `--head`，并加 fake `gh` 断言 |
| `gh pr create --dry-run` | 官方手册仍写明可能 push git changes | 维持 Libra 自实现 dry-run，禁止 `gh --dry-run` |
| `gh --body-file -` | 官方手册支持从 stdin 读 body | 第一版继续拒绝 `-`，防 stdin 阻塞与日志/body 泄露；§3.2/§5.2/§16 保持测试 |
| `gh --title/--body` 与 `--fill` | 官方手册允许覆盖 `--fill` | 第一版为稳定性继续拒绝组合，后续若放宽需新增版本矩阵与 argv 测试 |
| 其他 `gh` PR flags | 官方还有 `--editor`/`--template`/`--fill-first`/`--fill-verbose`/`--no-maintainer-edit`/`--recover` 等 | §3.2 列为 v1 非目标，避免无设计透传 |
| `PushArgs::for_refspecs` | 当前构造器固定 `set_upstream=false`，且字段私有 | §6.4 要求先补 `for_refspecs_set_upstream` 等窄 API，或 push 后走安全 upstream setter |
| `run_push` side effects | 源码注释称 Pure execution，但函数实际执行远端协商、上传、tracking/upstream 写入 | §6.4 改为「非渲染执行入口」，避免把它用于 dry-run 或纯计划阶段 |

### A.6 第六轮：十二维独立分析（2026-07-09）

本轮对全文做合理性 / 可行性 / 完整性 / 安全性 / 功能正确性与接口兼容性 / 数据流与控制流 / 性能与效率 / 可靠性与容错 / 兼容性与互操作性 / 可扩展性与可维护性 / 合规性与标准符合性 / 前后一致性十二维独立复核，并逐条以源码复核（`CliErrorReport`/`StableErrorCode`/`PushArgs`/`run_push`/`emit_json_data`/`ROOT_AFTER_HELP`/compat 守卫均已核实存在且如述）。结论：**绝大多数条款与代码一致**，新增 7 项（P35–P41）：

| 维度 | 核对项 | 结果 | 处置 |
| --- | --- | --- | --- |
| 数据流/控制流/可靠性 | §6 把 gh 存在/版本/auth 排在 push 之后 | **控制流缺陷**：`--push` + `gh` 缺失/未认证 → 分支已推送但 PR 未建，且本可预防 | §6/§6.1 重排：gh 存在/版本/auth 前置于 push；决策行 + 不变量 11 + 测试断言 |
| 前后一致性 | §9.4 `LBR-PR-007` 写「`conflict` 或 `repo`」 | **一致性冲突**：`StableErrorCode` 单码须单 category | §9.4 钉死 `repo`；dirty 同归；需区分另建码 |
| 完整性 | unborn HEAD / base==head / 空 title-body | **缺口**：未定义行为 | §5.2/§6.1/§8.1 补拒绝与语义 |
| 安全 | §7.3 stderr 脱敏无具体口径 | **安全缺口**：实现易漏 token 格式 | §7.3 钉死 pattern + 单测 |
| 可靠性 | SIGINT 转发 / `gh` 意外 prompt 兜底 | **缺口**：未写 | §7.4/§11 补 |
| 兼容 | SSH 显式端口 / `GH_HOST` | **缺口**：未提 | §12/§7.2 补 |
| 安全 | PATH 上木马 `gh` 的信任边界 | **缺口**：未声明威胁模型 | §7.6 新增 |

无与第一版目标相悖的条款。`emit_json_data` public / `write_json_command_envelope` private / `for_refspecs` 为 `pub(crate)`（同 crate 可调但 `set_upstream` 字段私有不可设）/ `run_push` 返回 `PushOutput` 且有远端写入副作用 / `StableErrorCode::exit_code()` 按 **category** 映射（`cli`→129、其余→128，`AddNothingStaged` 例外）/ `severity()` 按 **kind** 映射（`Fatal`→`"fatal"`、其余→`"error"`）——均与正文一致。

### A.7 第七轮：补遗（2026-07-09）

本轮针对第六轮遗留的 dry-run 网络调用矛盾、`GH_PROMPT_DISABLED` 不存在、`--web` 语义缺失、`--offline` 双码歧义做补遗：

| 核对项 | 结果 | 处置 |
| --- | --- | --- |
| dry-run 控制流与「仅本地推断」一致性 | §3.1/§10 要求 dry-run「仅本地推断」，但 §6 控制流中 dry-run 经过「远端 head 预检」→ 触发 `ls-remote --heads` 网络调用，矛盾 | §6/§6.1：dry-run 在 gh 版本检查后立即分支，远端 head 仅查本地 tracking（标 `stale_risk`），不调用 `ls-remote` |
| `GH_PROMPT_DISABLED` 存在性 | `gh` 无此 env；`gh` 通过 `isatty(stdin)` 判断交互性，`Stdio::null()` 已足够 | §11/§17/附录 B.14：删除 `GH_PROMPT_DISABLED` 引用，改为 `Stdio::null()` 非交互兜底 |
| `--web` 成功/失败语义 | 未定义 human 输出与 exit code | §5.3：成功输出「Opening browser...」+ compare URL，exit 0；失败 → `LBR-PR-005`/`LBR-IO-*` |
| `--offline` 双码映射 | `LBR-PR-005`（129）与 `LBR-NET-001`（128）双码，但 `--offline` 在互斥阶段即被拦截，`LBR-NET-001` 不可达 | §5.2：明确 `LBR-PR-005` 为主路径，`LBR-NET-001` 仅防御性保留 |

---

## 附录 B：实现前待钉死项（不阻塞设计，阻塞 merge 实现 PR）

1. **`gh` 最低版本**：临时 `2.40.0`，用目标平台实测后写入用户文档最终值。  
2. **`LBR-PR-*` vs 复用现有码**：二选一；禁止半套新码半套推断。  
3. **`LBR-PR-001` 的 category**：已定 `internal`；实现时写死并测 Display pin。  
4. **超时 env 正式名**：实现时写入 `docs/commands/pr.md`（候选 `LIBRA_PR_GH_TIMEOUT_SECS`）。  
5. **是否暴露 `pr.ghPath` 配置**：v1 可用 PATH 解析；配置项可阶段 4+。  
6. **已存在 PR 时是否自动 `gh pr view --json url`**：建议阶段 2 做只读补救，失败则错误。  
7. **默认分支来源优先级**：主用 `refs/remotes/<remote>/HEAD`；`ls-remote --symref` 仅作真 Git 远端的补充（对 Libra-native 远端返回空），实现时确认 fallback 顺序与「无默认分支 → 要求显式 `--base`」的错误路径。  
8. **双份命令文档 + 守卫登记**：`docs/commands/pr.md`、`docs/development/commands/pr.md` + README 行、`help_examples_banner.rs` 的 `VISIBLE_COMMANDS`、`COMPATIBILITY.md` 行须在同一实现 PR 内一起落地，否则 CI 挂（见 §16.4/§17）。
9. **`--push -u` 的内部 API**：实现 `libra pr create --push` 前，先在 `push.rs` 补 `set_upstream` 安全构造器，或明确采用 push 成功后的 `branch::set_upstream_safe_with_output`；必须有无 upstream 集成测试。
10. **`gh --head` 不变量**：fake `gh` argv 测试必须覆盖用户未传 `--head` 的路径，断言仍显式传入解析后的 head，防止 `gh` prompt push/fork。
11. **预检排序不变量**：实现时必须保证 `gh` 存在性/版本/auth 在 push 之前执行；集成测试用 mock push 断言 `gh` 缺失/auth 失败时 push **未被调用**（P35）。
12. **`LBR-PR-007` category**：已钉死为 `repo`（P36）；若后续需 `--require-clean` dirty 的 `conflict` 语义，另建 `LBR-PR-008` 而非改 007 的 category。
13. **stderr 脱敏实现**：落地时实现 §7.3 钉死的 token 正则过滤并加单测（构造含 `ghp_*`/`Authorization: Bearer` 的伪 stderr 断言过滤）。
14. **`Stdio::null()` 非交互兜底**：调用 `gh` 时 stdin 置 `Stdio::null()`，`gh` 通过 `isatty(stdin)` 判断交互性，null stdin 即自退不 prompt（§11）。`gh` 无 `GH_PROMPT_DISABLED` 等非交互 env。
