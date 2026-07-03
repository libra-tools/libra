### `cli.reflog-symbolic-ref`

目的：覆盖 `reflog` 与 `symbolic-ref` 的用户可观察 ref 日志和符号引用行为。

最小步骤：

```bash
# Short converged form.
SCENARIO="cli.reflog-symbolic-ref"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"

libra init ref-log-repo
cd ref-log-repo
libra config set user.name "Libra Reflog Test"
libra config set user.email "reflog@example.invalid"
printf 'one\n' > ref.txt
libra add ref.txt
libra commit -m "test: reflog one"
libra branch feature/ref-log
libra switch feature/ref-log
printf 'two\n' >> ref.txt
libra add ref.txt
libra commit -m "test: reflog two"

libra reflog show
libra reflog show HEAD
libra reflog show --stat -n 1
libra reflog show -p -n 1
libra reflog show --pretty oneline
libra reflog show --grep "reflog two"
libra reflog show --author "Libra Reflog Test"
libra reflog show --until 2000-01-01            # 全部被过滤：空输出、exit 0
libra reflog show refs/heads/no-such-branch     # 缺失 ref：空列表、exit 0（intentional）
libra reflog exists HEAD
libra --json reflog expire --all --dry-run --expire=all
libra --json reflog expire HEAD --dry-run --expire=all --expire-unreachable=all --rewrite --updateref --stale-fix
libra reflog expire --all --dry-run --expire=all -v
libra symbolic-ref HEAD
libra symbolic-ref --short HEAD
libra symbolic-ref HEAD refs/heads/main
libra symbolic-ref --short HEAD
libra symbolic-ref HEAD refs/heads/feature/ref-log
```

负向步骤：

```bash
cd "$RUN_DIR/ref-log-repo"
! libra reflog exists refs/heads/no-such-branch
! libra reflog expire refs/heads/no-such --dry-run --expire=all
! libra --json reflog expire
! libra symbolic-ref refs/heads/bad
! libra symbolic-ref HEAD refs/tags/not-a-branch
! libra symbolic-ref -d HEAD
```

断言：`reflog show` 能观察 commit、branch switch 或 HEAD 更新记录（`<ref_name>` 位置参数与裸 `show` 输出一致）；`--stat` / `-p` / `--pretty oneline` 输出可用于脚本断言（`<file> | 1 +`、`+++ b/<file>`、`<short-oid> HEAD@{N}: <action>: <msg>`）；`--grep` / `--author` 过滤可观察（`--grep` 排除不匹配条目），`--until <过去日期>` 输出为空，`-n` 限制条数；`reflog exists HEAD` 可用于脚本探测；`reflog expire --all --dry-run --expire=all` 必须返回 `reflog.expire` JSON envelope 且不写入；显式 ref 形式 `reflog expire HEAD --dry-run` 可携带 `--expire-unreachable/--rewrite/--updateref/--stale-fix` 并在 envelope 中报告 `ref_name == "HEAD"`；`-v` 输出每条 `expired <ref>@{N} <old>..<new>` 行；所有 dry-run 之后 reflog 必须原样保留；无 ref 且无 `--all` 的 `reflog expire` 必须返回 `LBR-CLI-002`（Libra intentional-difference：Git 静默 no-op），不存在 reflog 的显式 ref 必须失败（LBR-CLI-003）；`symbolic-ref HEAD` 和 `--short` 输出当前分支（`--short` 为不带 `refs/heads/` 前缀的裸分支名）；`symbolic-ref HEAD refs/heads/<branch>` 可切换 HEAD 的符号目标并被后续读取观察；`symbolic-ref -d` 是 Libra intentional-difference（SQLite 存储要求根引用，拒绝删除并提示 switch/checkout）；`reflog exists` 对缺失 ref 必须失败，非 HEAD 名称和非法 symbolic-ref 目标必须失败。注意 `reflog show <missing>` 返回空列表而非失败（intentional），不能作为负向断言，只能断言输出为空或 `count=0`。

补充可执行断言：
- `libra --json reflog show` 验证 `ok:true`，且 entries 中至少包含 "commit:" 或 "checkout:" 条目，并包含本场景创建的提交消息。
- `libra --json reflog expire --all --dry-run --expire=all` 验证 `ok:true`、`command == "reflog.expire"`；显式 ref 形式验证 `data[0].ref_name == "HEAD"`；无参 `expire` 的错误 JSON 验证 `error_code == "LBR-CLI-002"`。
- `libra --json symbolic-ref HEAD` 验证 `ok:true`，且 data 中的 ref 输出为 "refs/heads/..."。
- 非法 symbolic-ref 目标的失败必须包含稳定错误（LBR- 或 "not a branch" 类消息）；`-d` 的拒绝必须包含 "intentionally unsupported"。
- 操作前后 `libra --json show-ref --heads` 验证 `data.entries[]` 一致性（无意外丢失）。
- 委托覆盖（cargo 命令测试，不在 runner 重复）：`reflog show --since`（tests/command/reflog_test.rs `test_reflog_show_json_invalid_date_reports_invalid_arguments`）、`reflog delete`（同文件 `test_reflog_delete_json_*`）、`symbolic-ref -q`（tests/command/symbolic_ref_test.rs `symbolic_ref_quiet_detached_head_exits_silently` 等）、`symbolic-ref -m`（同文件 `symbolic_ref_set_with_reason_records_head_reflog`）。
