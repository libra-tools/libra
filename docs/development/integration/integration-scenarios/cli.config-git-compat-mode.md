### `cli.config-git-compat-mode`

目的：集中覆盖 `ConfigArgs` 上的 Git 兼容隐藏 flag 与位置参数翻译路径。

最小步骤：

```bash
# Short converged.
SCENARIO="cli.config-git-compat-mode"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"

libra init config-repo
cd config-repo
libra config set --add remote.origin.fetch "+refs/heads/*:refs/remotes/origin/*"
libra config set --add remote.origin.fetch "+refs/tags/*:refs/tags/*"

libra config user.compat value-from-positional
libra config --get user.compat
libra config --add user.compat second-value
libra config --get-all user.compat
libra config --get-regexp '^user\\.'
libra config --list
libra config -l
libra config --list --show-origin
libra config --unset user.compat
libra config --unset-all remote.origin.fetch
libra config --get -d fallback missing.compat
libra config --get --default fallback-long missing.compat.long

# 类型别名（--bool/--int/--path 等价于 --type=<t>，读取与设置时均规范化值；语义细节由 cargo 测试 test_config_typed_get/test_config_typed_set 覆盖）
libra config set custom.boolflag yes
libra config --bool --get custom.boolflag        # 输出 true
libra config set custom.intval 1k
libra config --int --get custom.intval           # 输出 1024
libra config set custom.pathval /tmp/typed-path
libra config --path --get custom.pathval         # 无 ~ 时原样输出

# --rename-section / --remove-section（仅 flag 形式，无 subcommand 等价物）
libra config set temp.section.alpha one
libra config set temp.section.beta two
libra config --rename-section temp.section moved.section
! libra config --get temp.section.alpha
libra config --get moved.section.alpha
libra config --remove-section moved.section
! libra config --get moved.section.beta
```

负向步骤：

```bash
! libra config --default fallback user.bad-default value
! libra config init value
! libra config --import user.name
! libra config --url https://example.invalid --get user.compat
! libra config --no-show-names --get user.compat
```

断言：位置参数 `key valuepattern` 的默认模式等价于 set；`--get` / `--get-all` / `--get-regexp` / `--list` / `-l` / `--show-origin` / `--add` / `--unset` / `--unset-all` / `-d` / `--default` 均至少有一个直接 invocation 覆盖；`--default` 只能与 get 类模式组合；不含 section 的 key 非 0 退出并对 `init` / `clone` 给出“这是顶层命令”的提示。`--import` 的正向导入路径依赖系统 `git`，由 `cli.config-import-path-edit` 覆盖；本场景只保留 `--import <key>` 的参数拒绝路径，避免把普通 compat 场景误标为 `requires_git`。类型别名 `--bool` / `--int` / `--path` 输出与 `--type=<t>` 等价的规范化值（true / 1024 / 原样路径）；section 操作仅以 flag 形式提供（无 subcommand 等价物）：`--rename-section <old> <new>` 把 old section 的 key 搬到 new（搬移后 `--get` old 下的 key 非 0、new 下的 key 为 0），`--remove-section <section>` 删除该 section（删除后 `--get` 该 section 下的 key 非 0）；被拒绝的 `--url` 与 `--no-show-names` 必须非 0 退出并给出明确不支持提示。

补充可执行断言：
- `libra --json config --get user.compat` 必须 `ok:true`，且 `data.value == "value-from-positional"`。
- `libra --json config --get-all user.compat` 必须返回 `data.entries[]`，且包含 `value-from-positional` 与 `second-value`。
- `libra --json config --list --show-origin` 必须返回 `data.entries[]`，每条包含 key/value 与 origin 或 scope 字段。
- `libra config --get --default fallback-long missing.compat.long` 必须输出 fallback-long 且退出码为 0。
- 负向 `--default` 非 get 模式、`config init value`、`--import user.name` 均必须非 0，stderr 包含可识别错误文本或 LBR- 稳定码。

