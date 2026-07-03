### `cli.gc-smoke`

目的：覆盖 `gc` 与同族维护命令 `prune` 当前未公开的 Git 兼容命令状态，确保它们不会被误当作已发布 CLI；runner 同时验证已发布的 `maintenance run --dry-run --task gc` 入口仍返回成功 JSON envelope，并在场景末尾验证仓库健康。

最小步骤：

```bash
SCENARIO="cli.gc-smoke"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"
# (prelude libra - short converged)

libra init repo
cd repo
libra config set user.name "Libra GC Test"
libra config set user.email "gc@example.invalid"
printf 'tracked\n' > tracked.txt
libra add tracked.txt
libra commit -m "test: gc base" --no-verify

printf 'gc unreachable blob\n' > unreachable.txt
OID="$(libra hash-object -w unreachable.txt)"
libra cat-file -t "$OID" | grep '^blob$'

! libra --json gc --dry-run
! libra --json prune --dry-run
libra --json maintenance run --dry-run --task gc >maintenance-gc.json
python3 -c "import json; d=json.load(open('maintenance-gc.json')); assert d['ok'] is True"
libra fsck --connectivity-only
```

断言：`gc` 和 `prune` 当前未注册为顶层命令；`libra --json gc --dry-run` 与 `libra --json prune --dry-run` 必须非 0 退出，JSON 错误码为 `LBR-CLI-001`；`maintenance run --dry-run --task gc` 必须成功；操作后 `libra fsck --connectivity-only` 通过。

补充可执行断言：
- `libra --json gc --dry-run` 必须失败，错误码为 `LBR-CLI-001`。
- `libra --json prune --dry-run` 必须失败，错误码为 `LBR-CLI-001`。
- `libra --json maintenance run --dry-run --task gc` 必须 `ok:true`。
- 操作后 `libra fsck --connectivity-only` 通过。
