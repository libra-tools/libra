### `cli.show-ref-exclude-existing`

目的：验证 `show-ref --exclude-existing[=<pattern>]` 的 Git 兼容 stdin filter 行为。

最小步骤：

```bash
SCENARIO="cli.show-ref-exclude-existing"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"

libra init show-ref-exclude-existing-repo
cd show-ref-exclude-existing-repo
libra config user.name "Libra Integration"
libra config user.email "integration@example.invalid"
printf 'base\n' > tracked.txt
libra add tracked.txt
libra commit -m "initial" --no-verify

HEAD_ID="$(libra rev-parse HEAD)"
printf '%s\n' \
  "$HEAD_ID refs/heads/main" \
  "$HEAD_ID refs/heads/new" \
  "refs/tags/newtag" \
  "$HEAD_ID refs/heads/main^{}" \
  | libra show-ref --exclude-existing

printf '%s\n' \
  "$HEAD_ID refs/heads/new" \
  "$HEAD_ID refs/tags/newtag" \
  | libra show-ref --exclude-existing=refs/heads

printf '%s\n' \
  "$HEAD_ID refs/heads/json-new" \
  "$HEAD_ID refs/heads/main" \
  | libra --json show-ref --exclude-existing

! libra show-ref --exclude-existing --verify refs/heads/main
libra fsck
```

关键断言：

- `--exclude-existing` 会剥离输入 refname 的 `^{}` 后缀进行存在性检查，并过滤本地已有 ref。
- 缺失 ref 的输出保留原始 stdin 行；`--exclude-existing=<pattern>` 只处理匹配前缀的 refname。
- JSON 模式返回 `exclude_existing: true` 和 `entries[].line` / `entries[].refname`。
- 与 `--verify` / `--exists` 的组合必须失败并暴露稳定 CLI 参数错误；场景末尾 `fsck` 继续通过。
