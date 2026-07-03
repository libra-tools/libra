### `cli.notes-smoke`

目的：覆盖 `notes` 作为已公开 Git 兼容命令的最小黑盒行为，确保可以在 HEAD 和自定义 ref 上 add/show/list/remove notes，JSON 输出正常，重复添加被拒绝。

最小步骤：

```bash
SCENARIO="cli.notes-smoke"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"

libra init repo
cd repo
libra config user.name "Libra Integration"
libra config user.email "integration@example.invalid"
printf 'base\n' > tracked.txt
libra add tracked.txt
libra commit -m "test: notes fixtures" --no-verify

libra notes add -m "Reviewed-by: Alice"
libra notes show
libra --json notes show >notes-show.json
python3 -c "import json; d=json.load(open('notes-show.json')); assert d['ok'] is True; assert d['command'] == 'notes'; assert d['data']['action'] == 'show'"

libra notes list
libra --json notes list >notes-list.json
python3 -c "import json; d=json.load(open('notes-list.json')); assert d['ok'] is True; assert d['data']['action'] == 'list'; assert len(d['data']['notes']) >= 1"

libra notes --ref refs/notes/review add -m "LGTM"
libra notes --ref refs/notes/review list
libra notes --ref refs/notes/review remove HEAD

libra notes remove HEAD
libra notes list
libra fsck --connectivity-only
```

负向步骤：

```bash
cd "$RUN_DIR/repo"
! libra notes add -m "duplicate without force"
```

断言：`notes add` 在 HEAD 上创建 note；`notes show` 输出 note 文本；`notes list` 至少列出一条 note；自定义 ref 隔离，`--ref refs/notes/review` 的 add/list/remove 不影响默认 `refs/notes/commits`；`--json` 返回 `ok:true` 且命令名为 `notes`；重复 add 必须非 0 退出；操作后 `fsck --connectivity-only` 通过。

补充可执行断言：
- `notes show` 标准输出包含 `Reviewed-by: Alice`。
- `notes --json list` 的 `data.notes` 长度 >= 1。
- `notes --ref refs/notes/review add` 后默认 ref 的 list 数量不变。
- 第二次 `notes add -m` 必须失败。
- `--json` 输出包含 `command: "notes"`。
- 操作后 `libra fsck --connectivity-only` 通过。
