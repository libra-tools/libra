### `cli.verify-pack-smoke`

目的：覆盖 `verify-pack` 对 `.idx` / `.pack` 成对文件的黑盒验证，避免 Maintenance 矩阵把 pack 验证误归入 `fsck` 或 `cat-file` 覆盖。

最小步骤：

```bash
SCENARIO="cli.verify-pack-smoke"
REPO_ROOT="$PWD"   # 记录 libra 仓库根目录（Wave 0 执行目录），供后续复制 fixture
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"
# (prelude libra - short converged)

libra init pack-source
cd pack-source
libra config set user.name "Libra Pack Test"
libra config set user.email "pack@example.invalid"
printf 'pack one\n' > one.txt
printf 'pack two\n' > two.txt
libra add one.txt two.txt
libra commit -m "test: pack source"

# verify-pack 需要 pack+idx 成对输入；用仓库内固定 pack fixture 并通过隐藏
# index-pack 生成 idx，避免读取开发者真实 .git/.libra pack 目录。
mkdir -p "$RUN_ROOT/fixtures/$SCENARIO"
PACK_FILE="$RUN_ROOT/fixtures/$SCENARIO/small-sha1.pack"
PACK_IDX="$RUN_ROOT/fixtures/$SCENARIO/small-sha1.idx"
PACK_KEEP="$RUN_ROOT/fixtures/$SCENARIO/small-sha1.keep"
PACK_FILE_2="$RUN_ROOT/fixtures/$SCENARIO/small-sha1-stdin.pack"
PACK_IDX_2="$RUN_ROOT/fixtures/$SCENARIO/small-sha1-stdin.idx"
cp "$REPO_ROOT/tests/data/packs/small-sha1.pack" "$PACK_FILE"
libra index-pack --progress --keep="integration keep" "$PACK_FILE" --index-version 1   # idx/keep 写到 pack 同目录
cat "$REPO_ROOT/tests/data/packs/small-sha1.pack" | libra index-pack --no-progress --stdin -o "$PACK_IDX_2" --index-version 1
test -f "$PACK_IDX"
test -f "$PACK_FILE_2"
test -f "$PACK_IDX_2"
test "$(cat "$PACK_KEEP")" = "integration keep"
libra verify-pack "$PACK_IDX"
libra verify-pack "$PACK_IDX" "$PACK_IDX_2"
libra verify-pack --pack "$PACK_FILE" "$PACK_IDX"
libra verify-pack -v "$PACK_IDX"
libra verify-pack -s "$PACK_IDX"
libra verify-pack -s "$PACK_IDX" "$PACK_IDX_2"
libra --json verify-pack "$PACK_IDX" >verifypack.json
python3 -c "import json; d=json.load(open('verifypack.json')); assert d['ok'] is True; assert d['data']['verified'] is True; assert d['data']['object_count'] > 0"
libra --json verify-pack "$PACK_IDX" "$PACK_IDX_2" >verifypack-multi.json
python3 -c "import json; d=json.load(open('verifypack-multi.json')); assert d['ok'] is True; assert d['data']['verified'] is True; assert d['data']['count'] == 2; assert len(d['data']['results']) == 2"
```

负向步骤：

```bash
cd "$RUN_DIR/pack-source"
! libra verify-pack "$RUN_ROOT/fixtures/$SCENARIO/missing.idx"
! libra verify-pack --pack "$PACK_FILE" "$PACK_IDX" "$PACK_IDX_2"
cp "$PACK_IDX" "$RUN_ROOT/fixtures/$SCENARIO/corrupt.idx"
printf 'corrupt' >> "$RUN_ROOT/fixtures/$SCENARIO/corrupt.idx"
! libra verify-pack "$RUN_ROOT/fixtures/$SCENARIO/corrupt.idx"
```

断言：`index-pack --keep=...` / `--progress` / `--no-progress` / `--stdin -o ...` 仅作为隐藏内部 fixture 生成器使用；普通 pack 路径创建同名 `.keep` 文件且内容为消息加换行；stdin 路径创建 `-o` 同 stem 的 `.pack` 和目标 `.idx`；`verify-pack` 默认从 idx sibling 推导 `.pack` 路径；多个 idx 按输入顺序分别验证；`--pack` 显式路径可验证单个 pack，但和多个 idx 组合必须失败；`-v` 输出对象 hash/offset；`-s` 输出统计摘要；`--json` 单 idx 输出 `verified=true`、object count、pack/index hash 等结构化字段，多 idx 输出 `count` 和 `results`；缺失或损坏 idx 必须失败且错误包含受影响路径。fixture 来源固定为仓库内 `tests/data/packs/small-sha1.pack` 复制或管道写入到 `$RUN_ROOT/fixtures/$SCENARIO/`，不得读取开发者真实 `.git/objects/pack` 或 `.libra/objects/pack`。

补充可执行断言：
- `libra --json verify-pack "$PACK_IDX"` 必须 `ok:true`；单 idx 时 `data.verified == true` 且 `data.object_count > 0`。
- `libra --json verify-pack "$PACK_IDX" "$PACK_IDX_2"` 必须 `ok:true`，`data.count == 2` 且 `data.results` 含两个结果。
- `libra index-pack --progress --keep=... "$PACK_FILE"` 必须创建 `$PACK_KEEP`，文件内容为消息加换行。
- `libra index-pack --no-progress --stdin -o "$PACK_IDX_2"` 必须创建 `$PACK_FILE_2` 和 `$PACK_IDX_2`。
- `verify-pack "$PACK_IDX" "$PACK_IDX_2"` 必须为两个 idx 分别输出 `<idx>: ok`。
- `verify-pack --pack "$PACK_FILE" "$PACK_IDX"` 输出与 sibling 推导一致的 `<idx>: ok`。
- `verify-pack -v` 输出 `<oid> <type> <size> <size-in-pack> <offset>` 对象行（含 `commit` 和 `blob`）并以 `: ok` 结尾。
- `verify-pack -s` 和 `verify-pack -s "$PACK_IDX" "$PACK_IDX_2"` 输出 `non delta:` 统计摘要且不打印 `: ok` 行。
- `verify-pack --pack "$PACK_FILE" "$PACK_IDX" "$PACK_IDX_2"` 必须非 0，错误包含 `cannot use --pack with multiple index files` 或稳定 CLI 错误码。
- 缺失 idx 场景 `libra verify-pack missing.idx` 必须非 0，错误包含 `could not open pack index` 与路径。
- 损坏 idx 场景 `libra verify-pack corrupt.idx` 必须非 0，stderr 包含 `invalid pack index` 路径或 corrupt 信息。
- 操作后在生成 pack 的仓库执行 `libra fsck` 通过。
- 非 verbose `--json` 不要求 `objects` 数组；`-v` 的对象行由 human 输出断言覆盖。
