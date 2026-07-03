### `cli.init-vault`

目的：覆盖 `--vault <bool>`，并验证默认 vault 行为与显式关闭行为。

最小步骤：

```bash
# Short form.
SCENARIO="cli.init-vault"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"

mkdir -p home-vault home-no-vault

libra init --vault true vault-repo
cd vault-repo
test -f .libra/vault.db
libra config get vault.signing

cd "$RUN_DIR"
libra init --vault false no-vault-repo
cd no-vault-repo
test ! -f .libra/vault.db
libra config get vault.signing
```

断言：`--vault true` 创建 repo-local `vault.db` 并使 `vault.signing` 可通过 `config get` 观察；`--vault false` 不创建 `vault.db`，`vault.signing` 为关闭值；场景必须隔离 `HOME`，不得读写开发者真实 `~/.libra/vault-keys`。

补充可执行断言（安全关键）：
- `--vault true` 后 `test -f .libra/vault.db` 且 `libra --json config get vault.signing` 成功。
- `--vault false` 后 `test ! -f .libra/vault.db`。
- 使用隔离 HOME 执行后，验证真实 `~/.libra/vault-keys`（或 global vault）未被创建/修改。
- `libra --json config get vault.signing` 在 false 情况下返回关闭值。
- 操作后 `libra fsck` 通过。

