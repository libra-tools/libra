### `cli.config-list-variants`

目的：覆盖 `list` 子命令的 `--name-only`、`--show-origin`、`--vault`、`--ssh-keys`、`--gpg-keys`，以及 Git 兼容 `--list` / `-l` / `--show-origin`。

最小步骤：

```bash
SCENARIO="cli.config-list-variants"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"
# (prelude provides libra() -- converged short form, long wrapper removed)

libra init config-repo
cd config-repo
libra config set user.name "List User"
libra config set user.email "list@example.invalid"

libra config list
libra config -l
libra config --list
libra config list --name-only
libra config list --show-origin
libra config --list --show-origin
libra config list --vault
libra config list --ssh-keys
libra config list --gpg-keys
```

断言：三种 list 入口均成功；`--name-only` 只输出 key 名；`--show-origin` 输出 scope/origin 信息；vault/ssh/gpg 专项列表在无记录时输出明确空状态，在已有记录时只输出公钥或 key 名称，不输出私钥、root token 或 unseal key。

补充可执行断言：
- `libra --json config list --name-only` 必须被拒绝（`ok:false`、`error_code == "LBR-CLI-002"`）：`--name-only` 是人类输出整形 flag，JSON envelope 恒含 key/value（与 cargo 命令测试 `test_config_json_null_is_rejected` 家族一致）。
- `libra --json config list --show-origin` 每个条目包含 origin/scope 信息。
- `libra --json config list --vault`（无 vault 记录时）成功且 data 为空或明确空状态。
- `libra config list --ssh-keys` / `--gpg-keys` 输出不得包含私钥材料。
- 操作后用隔离 global DB 验证无全局污染。

