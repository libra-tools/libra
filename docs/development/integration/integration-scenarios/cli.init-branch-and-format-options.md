### `cli.init-branch-and-format-options`

目的：覆盖 `-b <branch>`、`--initial-branch <branch>`、`--object-format <format>` 和 `--ref-format <format>`。

最小步骤：

```bash
# Converged short form.
SCENARIO="cli.init-branch-and-format-options"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"

libra init -b develop init-branch-short
cd init-branch-short
libra branch
libra status

cd "$RUN_DIR"
libra init --initial-branch trunk init-branch-long
cd init-branch-long
libra branch

cd "$RUN_DIR"
libra init --object-format sha1 object-sha1
cd object-sha1
libra config get core.objectformat

cd "$RUN_DIR"
libra init --object-format sha256 object-sha256
cd object-sha256
libra config get core.objectformat

cd "$RUN_DIR"
libra init --ref-format strict ref-strict
cd ref-strict
libra config get core.initrefformat

cd "$RUN_DIR"
libra init --ref-format filesystem ref-filesystem
cd ref-filesystem
libra config get core.initrefformat
```

负向步骤：

```bash
cd "$RUN_DIR"
! libra init --object-format sha265 bad-object-format
! libra init --ref-format unknown bad-ref-format
! libra init -b "bad branch" bad-branch-name
```

断言：短/长 initial branch 参数都能通过 `branch` 或等价公开命令观察到初始分支；`core.objectformat` 分别为 `sha1` / `sha256`；`core.initrefformat` 分别为 `strict` / `filesystem`；非法 object/ref format 或非法分支名必须非 0 退出，并给出可理解的参数错误或修复提示。

补充可执行断言（对象格式与 ref 格式关键）：
- `libra --json config get core.objectformat` 在 sha256 仓库中验证值为 "sha256"。
- `libra --json init --object-format sha256 sha256-json` 成功后用 `libra --json cat-file -p HEAD` 验证对象 ID 格式（64 位 hex）。
- 非法 `--object-format sha265` 的错误必须非 0，且包含 "unsupported object format" 或 LBR- 相关标识（捕获 stderr 验证）。
- 所有 init 后立即 `libra fsck --connectivity-only` 通过。

