### `cli.config-get-default-and-patterns`

目的：覆盖 `get` 子命令的 `--all`、`--reveal`、`--regexp`、`-d/--default`，以及 Git 兼容隐藏 flag `--get`、`--get-all`、`--get-regexp`。

最小步骤：

```bash
SCENARIO="cli.config-get-default-and-patterns"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"

# (prelude provides libra)
libra init config-repo
cd config-repo

libra config set user.name "Pattern User"
libra config set user.email "pattern@example.invalid"
libra config set core.editor vim
libra config set --add remote.origin.fetch "+refs/heads/*:refs/remotes/origin/*"
libra config set --add remote.origin.fetch "+refs/tags/*:refs/tags/*"

libra config get user.name
libra config --get user.name
libra config get --default fallback missing.key
libra config get -d fallback-short missing.short
libra config get --regexp '^user\\.'
libra config --get-regexp '^user\\.'
libra config --get-all remote.origin.fetch
```

断言：普通 get 与 `--get` 输出一致；缺失 key 带 default 时退出码为 0 并输出 fallback；regexp 只输出匹配 key；`--get-all` 覆盖多值 key。隐藏 flag 是兼容 invocation 覆盖，不要求出现在 `config --help`。

补充可执行断言（Agent 非常常用）：
- `libra --json config get --default fallback missing.key` 必须 `ok:true`，且 `data.value == "fallback"`、`data.default_applied == true`。
- `libra --json config --get-regexp '^user\.'` 返回 `data.entries[]`，所有 entry 的 `key` 以 `user.` 开头。
- 普通 `libra --json config get user.name` 与 `libra --json config --get user.name` 结果等价。
- 非法 `--default` 与非 get 组合必须失败。
- 验证 `--json` 输出结构稳定（"ok", "data" 字段存在）。

