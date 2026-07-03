### `cli.archive-smoke`

目的：覆盖 `archive` 作为已公开 Git 兼容命令的最小黑盒行为，确保 committed tree 可以生成 tar/zip 文件输出，`--list` 可列出格式，pathspec 可限制归档内容，安全前缀生效，非法前缀被拒绝。

最小步骤：

```bash
SCENARIO="cli.archive-smoke"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"
# (prelude libra - short converged)

libra init repo
cd repo
libra config user.name "Libra Integration"
libra config user.email "integration@example.invalid"
printf 'base\n' > tracked.txt
mkdir -p docs
printf 'archive docs\n' > docs/guide.md
libra add tracked.txt docs/guide.md
libra commit -m "test: archive fixtures" --no-verify

libra archive --output "$RUN_DIR/release.tar" --prefix release/
test -f "$RUN_DIR/release.tar"
python3 -c "p='$RUN_DIR/release.tar'; d=open(p,'rb').read(); assert len(d) >= 263 and d[257:263] in (b'ustar\0', b'ustar '); assert b'release/tracked.txt' in d; assert b'release/docs/guide.md' in d"

libra archive --format=zip --output "$RUN_DIR/release.zip"
test -f "$RUN_DIR/release.zip"
python3 -c "d=open('$RUN_DIR/release.zip','rb').read(4); assert d.startswith(b'PK')"

libra fsck --connectivity-only
```

负向步骤：

```bash
cd "$RUN_DIR/repo"
! libra archive --prefix ../escape
```

断言：`archive` 默认 tar 文件输出包含 committed tree 中的根文件和子目录文件；`--prefix` 只产生相对归档路径；`--format=zip --output` 生成 zip 文件而不依赖 stdout；非法 `..` 前缀必须非 0 退出且错误包含 `invalid archive prefix` 或 `LBR-`；归档操作后 `fsck --connectivity-only` 通过。

补充可执行断言：
- `release.tar` 和 `release.zip` 均必须实际存在。
- tar 输出包含 `release/tracked.txt` 和 `release/docs/guide.md`。
- zip 输出必须以 `PK` 文件头开始。
- `libra archive --prefix ../escape` 必须失败。
- 操作后 `libra fsck --connectivity-only` 通过。
