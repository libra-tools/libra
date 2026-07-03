### `live.github-create-push-clone-fetch`

目的：验证 `libra` 能和通过 `gh` 创建的 GitHub 临时仓库完成真实远端闭环。

前置条件：

1. `gh auth status --active --hostname github.com` 退出码为 0。
2. 当前账号有创建私有仓库和删除测试仓库权限；若没有删除权限，不启动场景。
3. 本机具备 Libra 访问所选远端 URL 的认证能力。默认使用 `sshUrl`，因此需要 GitHub 已配置可用 SSH key；HTTPS 只在 Libra 明确配置了可记录、可隐藏的认证来源时使用。

最小步骤：

```bash
# (prelude at top of run; Short converged short form per §3.3.1 + §0 checklist.
# gh calls use host env for auth/config; libra() wrapper (with SSH_AUTH_SOCK) is from the
# single prelude block. gh create + trap/guard precede repo work. See run-live Rust impl.)

SCENARIO="live.github-create-push-clone-fetch"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"

# gh bare (host auth); libra via shared wrapper from prelude (see §3.3.1)
gh auth status --active --hostname github.com
OWNER="$(gh api user --jq '.login')"
REPO="$OWNER/libra-integ-$(date -u +%Y%m%dT%H%M%SZ)-$$"
gh repo create "$REPO" --private --disable-issues --disable-wiki \
  --description "Temporary Libra integration test $REPO"
trap 'gh repo delete "$REPO" --yes' EXIT

REMOTE_URL="$(gh repo view "$REPO" --json sshUrl --jq '.sshUrl')"
gh repo view "$REPO" --json nameWithOwner,isPrivate,isEmpty,url,sshUrl

cd "$RUN_DIR"
libra init source
cd source
libra config set user.name "Libra GitHub Integration"
libra config set user.email "libra-integration@example.invalid"
printf 'github remote\n' > README.md
libra add README.md
libra commit -m "test: github integration"
libra remote add origin "$REMOTE_URL"
libra push --dry-run origin main
libra push -u origin main

REMOTE_MAIN_SHA="$(gh api "repos/$REPO/git/ref/heads/main" --jq '.object.sha')"
test "$REMOTE_MAIN_SHA" = "$(libra rev-parse HEAD)"

libra branch feature/live main
libra switch feature/live
printf 'feature branch\n' > feature.txt
libra add feature.txt
libra commit -m "test: github feature branch"
libra push origin feature/live:feature/pushed
libra tag v-live-smoke
libra push --tags origin
gh api "repos/$REPO/git/ref/tags/v-live-smoke" --jq '.object.sha' >/dev/null
libra push origin :feature/pushed
libra push --mirror --dry-run origin
libra push --mirror origin

libra switch main
printf 'forced rewrite\n' >> README.md
libra add README.md
libra commit --amend --no-edit
FORCED_MAIN="$(libra rev-parse HEAD)"
set +e
libra push origin main >non-ff.out 2>non-ff.err
NON_FF_STATUS=$?
set -e
test "$NON_FF_STATUS" -ne 0
libra push --force origin main
test "$(gh api "repos/$REPO/git/ref/heads/main" --jq '.object.sha')" = "$FORCED_MAIN"

cd "$RUN_DIR"
libra clone "$REMOTE_URL" cloned
cd cloned
libra log --oneline
grep 'forced rewrite' README.md

cd "$RUN_DIR/source"
printf 'second commit\n' >> README.md
libra add README.md
libra commit -m "test: github second commit"
libra push origin main

cd "$RUN_DIR/cloned"
libra fetch origin
libra pull origin main
grep 'second commit' README.md
```

断言：

1. `gh repo create` 创建的是当前账号名下的临时私有仓库，`gh repo view` 可查询到 `nameWithOwner`、`isPrivate`、`sshUrl`。
2. `libra remote add`、`push --dry-run origin main`、`push -u origin main`、refspec push、tag push、delete refspec、`push --mirror --dry-run`、`push --mirror`、`push --force`、`clone`、`fetch`、`pull` 均退出码为 0。
3. `gh api repos/<owner>/<repo>/git/ref/heads/main` 能看到被推送的 `main` ref，且 normal push 在非快进 rewrite 后必须失败、`push --force` 后远端 main 才更新到 `FORCED_MAIN`。
4. clone 后 `log --oneline` 能看到首次/force 后提交；pull 后工作区能看到第二次提交内容。
5. 日志不得包含 GitHub token、PAT、SSH 私钥、`gh auth token` 输出或带明文凭据的 URL。
6. 场景结束后 `gh repo delete "$REPO" --yes` 成功；失败时报告 `cleanup_required` 并列出仓库名。

补充可执行断言（Wave 3 最高价值）：
- 关键步骤后执行 `libra --json log -n 1` 并验证 `ok:true`。
- `gh api` 返回的 sha 与本地 `libra rev-parse HEAD` 一致（initial push 与 force push 后都 capture 比对）。
- 整个运行使用完整隔离 `libra()`（含 TMPDIR + SAFE_PATH + LIBRA_TEST）。
- 强制要求 `trap 'gh repo delete ... --yes' EXIT` 且 cleanup 状态明确记录。
- 推荐验证 `libra --json show-ref --heads` 在 clone 后可解析。

通过标准：真实 GitHub 仓库创建、push、远端 ref 查询、clone、fetch/pull 和删除全部成功。若失败是认证、权限、GitHub 服务或本机网络问题，报告必须区分环境失败与 Libra 行为失败。

补充可执行断言（Wave 3 最高价值场景）：
- 每个 `libra` 操作（init、push、clone、fetch、pull）均使用完整隔离 `libra()` wrapper 或 Rust runner 中等价的 `env_clear()` 白名单环境（含 TMPDIR + SAFE_PATH）。
- 关键步骤后执行 `libra --json log -n 1` 并验证 `ok:true` + 提交存在。
- `gh api` 查询与 `libra show-ref` 结果必须一致（至少覆盖 main、tag、删除后的 feature ref、force push 后 main）。
- 强制要求 Rust cleanup guard（或人工执行时的 trap）调用 `gh repo delete --yes`，失败时明确记录 `cleanup_required`。
- 整个 Wave 3 运行日志必须通过 §3.6 脱敏自检（无 token/PAT/私钥）。
- 推荐在 runner 中捕获 `gh api` 返回的 sha 与本地 `libra rev-parse` 比对。

---
