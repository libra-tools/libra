### `cli.push-local-file-remote-rejected`

目的：验证 `push` 对本地 file remote 的故意差异：本地路径 remote 可用于 `clone`/`fetch`/`pull` fixture，但 `push` 当前只支持网络 remote，必须拒绝本地 file remote。真实 push/refspec/tag/force/mirror 成功路径放到 Wave 3 GitHub 场景。

最小步骤：

```bash
SCENARIO="cli.push-local-file-remote-rejected"
REMOTE_DIR="$RUN_ROOT/fixtures/$SCENARIO/remote.git"
WORK_DIR="$RUN_ROOT/repos/$SCENARIO/work"
mkdir -p "$(dirname "$REMOTE_DIR")" "$(dirname "$WORK_DIR")"
# (prelude provides libra() -- converged short form, long wrapper removed)


libra init --bare "$REMOTE_DIR"
libra init "$WORK_DIR"
cd "$WORK_DIR"
libra config set user.name "Libra Push Rejection Test"
libra config set user.email "push-reject@example.invalid"
printf 'push\n' > push.txt
libra add push.txt
libra commit -m "test: push rejection base"
libra remote add origin "$REMOTE_DIR"
libra remote set-url --add --push origin "$REMOTE_DIR"
libra remote get-url --all origin
libra remote get-url --push origin

expect_local_push_rejected() {
  name="$1"
  shift
  set +e
  libra --json=compact push "$@" >"$name.out" 2>"$name.err"
  status=$?
  set -e
  test "$status" -ne 0
  python3 - "$name.err" <<'PY'
import json, sys
raw = open(sys.argv[1]).read().strip()
payload = json.loads(raw)
assert payload["ok"] is False
assert payload["error_code"] == "LBR-CLI-003"
assert "local file" in payload["message"] or "local file repositories" in payload["message"]
PY
}

expect_local_push_rejected push-main origin main
expect_local_push_rejected push-dry-run --dry-run origin main
expect_local_push_rejected push-force --force origin main
expect_local_push_rejected push-atomic --atomic origin main
expect_local_push_rejected push-tags --tags origin
expect_local_push_rejected push-mirror --mirror --dry-run origin
# lease/signing/push-option/thin/follow-tags flag 组合在 transport 选择前解析校验，
# 锁定各 flag 的 CLI parse 路径（lease/signed/push-option 行为语义由 src/command/push.rs 单元测试覆盖）
expect_local_push_rejected push-lease --force-with-lease --force-if-includes origin main
expect_local_push_rejected push-signed --signed --follow-tags -o ci.skip --thin origin main
expect_local_push_rejected push-no-thin --no-thin --no-follow-tags origin main

# --porcelain 与 JSON envelope 互斥，走 human 错误面断言
set +e
libra push --porcelain origin main >porcelain.out 2>porcelain.err
status=$?
set -e
test "$status" -ne 0
grep -q 'local file' porcelain.err
libra fsck --connectivity-only
```

断言：本地 file remote 已存在且可作为 remote URL 存储，`remote get-url --push origin` 能读回 `set-url --add --push` 写入的 pushurl；`push origin main`、`push --dry-run origin main`、`push --force origin main`、`push --atomic origin main`、`push --tags origin`、`push --mirror --dry-run origin`，以及携带 `--force-with-lease --force-if-includes`、`--signed --follow-tags -o ci.skip --thin`、`--no-thin --no-follow-tags` 的组合都必须非 0 退出；`--json=compact` 的 stderr 错误 envelope 必须包含 `ok:false`、`error_code == "LBR-CLI-003"` 和本地 file remote 不支持的可操作提示；`push --porcelain origin main`（与 JSON 互斥，走 human 错误面）同样非 0 且 stderr 含本地 file remote 提示；失败不得写入 remote refs 或修改本地 HEAD。

补充可执行断言：
- 每个本地 file remote push 失败后执行 `libra fsck --connectivity-only`，确认本地源仓库仍健康。
- `libra --json remote get-url --all origin` 仍能返回本地路径，证明失败点是 push 传输策略而非 remote 配置丢失。
- 若未来实现支持本地 file remote push，必须把本场景改成正向闭环，并同步更新 COMPATIBILITY.md / declined note。
- lease/signed/push-option/follow-tags 的行为语义（lease 校验、push-cert nonce、option 字节校验、tag 计划）由 `src/command/push.rs` 单元测试与 `tests/command/push_test.rs` 覆盖；本场景只锁定这些 flag 的黑盒 CLI parse + 拒绝路径。
