### `cli.sha256-object-readback`

目的：验证 `--object-format sha256` 仓库不仅 `core.objectformat` 正确，还能走完整“提交→对象读回”闭环。这覆盖 `src/cli.rs` 的 hash-kind preflight（按仓库 `core.objectformat` 调 `set_hash_kind`）的端到端正确性；`cli.init-branch-and-format-options` 只验证了 config 键，未验证 sha256 对象真正可写可读。

最小步骤：

```bash
SCENARIO="cli.sha256-object-readback"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"
# (prelude libra - short converged, enforced by gate)
libra init --object-format sha256 sha256-repo
cd sha256-repo
libra config get core.objectformat
libra config set user.name "Libra Sha256 Test"
libra config set user.email "sha256@example.invalid"
printf 'sha256 payload\n' > payload.txt
libra add payload.txt
libra commit -m "test: sha256 commit"

HEAD_ID="$(libra rev-parse HEAD)"
test "${#HEAD_ID}" -eq 64          # sha256 对象 id 为 64 位 hex（sha1 为 40 位）
libra cat-file -t "$HEAD_ID"
libra cat-file -p "$HEAD_ID"
libra show --stat HEAD
libra log --oneline -n 1
libra fsck --connectivity-only

BLOB_ID="$(libra hash-object -w payload.txt)"
test "${#BLOB_ID}" -eq 64
libra cat-file -p "$BLOB_ID"
```

断言：`core.objectformat` 为 `sha256`；commit 与 blob 的对象 id 均为 64 位 hex，证明 hash-kind preflight 正确按仓库格式 pin（而非默认 sha1）；`cat-file -t/-p`、`show --stat`、`log --oneline`、`fsck --connectivity-only`、`hash-object -w` 在 sha256 仓库全部成功且写入对象可读回；与默认 sha1 的 `cli.object-readback` 形成对照。

补充可执行断言：
- `libra --json config get core.objectformat` 验证值为 "sha256"。
- `libra --json cat-file -p HEAD` 成功且 commit ID 为 64 字符 hex。
- 写入 blob 后 `libra --json cat-file -t $BLOB_ID` 返回 "blob"。
- 全流程 `libra fsck --connectivity-only` 通过。

