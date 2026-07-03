### `cli.tag-basic`

目的：覆盖当前 `tag` 正向能力，并把未支持的 Git tag 参数作为负向断言保留。

最小步骤：

```bash
SCENARIO="cli.tag-basic"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"

libra init repo
cd repo
libra config user.name "Libra Integration"
libra config user.email "integration@example.invalid"
printf 'base\n' > tracked.txt
libra add tracked.txt
libra commit -m "initial" --no-verify

libra tag v1.0.0
libra tag -l
libra tag -m "release v1.1.0" v1.1.0
libra tag -l -n 1
! libra tag -F release.txt v1.2.0
libra rev-parse v1.0.0
libra describe --tags --always
libra tag -f v1.0.0
libra tag -d v1.1.0
! libra tag v1.3.0 v1.4.0
libra --json tag -d v1.0.0
! libra --json tag -d missing-tag
libra --json tag -l
libra fsck --connectivity-only
```

关键断言：

- lightweight tag、`-m` annotated tag、`-l -n` 摘要、`-f` 强制更新、delete 和 JSON delete 当前可用。
- `-F`、多 tag 创建、缺失 tag 删除必须稳定失败。
- `describe --tags --always` 能使用当前 tag/ref 状态输出可读名称。
