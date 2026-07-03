### `cli.config-set-input-and-encryption`

目的：覆盖 `set` 子命令的 `--add`、`--encrypt`、`--plaintext`、`--stdin` 参数，以及敏感 key 的保护输入行为。

最小步骤：

```bash
# Prelude copied once at top (converged short form).
SCENARIO="cli.config-set-input-and-encryption"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"

libra init config-repo
cd config-repo

libra config set --add remote.origin.fetch "+refs/heads/*:refs/remotes/origin/*"
libra config set --add remote.origin.fetch "+refs/tags/*:refs/tags/*"
libra config get --all remote.origin.fetch

printf 'stdin-value\n' | libra config set --stdin custom.stdin
libra config get custom.stdin

libra config set --encrypt custom.secret "s3cr3t"
libra config get custom.secret
libra config get --reveal custom.secret

libra config set --plaintext custom.plain "plain-value"
libra config get custom.plain
```

负向步骤：

```bash
cd "$RUN_DIR/config-repo"
! libra config set --encrypt --plaintext custom.bad value
! libra config set --stdin custom.bad value
! libra config set --plaintext vault.env.TEST_SECRET value
```

断言：`--add` 允许同 key 多值，`get --all` 能看到全部值；`--stdin` 去掉末尾换行并保存；`--encrypt` 默认 `get` 不泄露明文，`get --reveal` 才输出明文；`--plaintext` 保存普通明文；互斥/非法组合必须非 0 退出且不写入坏状态。

补充可执行断言：
- `libra --json config get --all remote.origin.fetch` 必须返回 `ok:true`，且 `data.entries[]` 长度 ≥2。
- `--encrypt` 后普通 `libra --json config get custom.secret` 必须成功但不返回明文（或返回 masked）；加 `--reveal` 才返回真实值。
- `--stdin` 后验证值不带末尾换行（`libra config get | wc -l` 验证）。
- 非法组合（如 `--encrypt --plaintext`）必须非 0，且不写入任何配置。
- 操作后用隔离 global DB 验证无泄露。

