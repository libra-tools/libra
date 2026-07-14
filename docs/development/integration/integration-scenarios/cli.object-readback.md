### `cli.object-readback`

目的：验证当前 CLI 写入的 commit/blob/ref 能通过已注册 plumbing/history-inspection 命令读回。

最小步骤：

```bash
SCENARIO="cli.object-readback"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"

libra init object-repo
cd object-repo
libra config user.name "Libra Object Test"
libra config user.email "object@example.invalid"
mkdir -p docs
printf 'object root\n' > README.md
printf 'object docs\n' > docs/guide.md
libra add README.md docs/guide.md
libra commit -m "test: object readback" --no-verify

HEAD_ID="$(libra rev-parse HEAD)"
test "$(libra rev-parse @)" = "$HEAD_ID"
TREE_ID="$(libra rev-parse 'HEAD^{tree}')"
GUIDE_ID="$(libra rev-parse 'HEAD:docs/guide.md')"
libra rev-parse 'HEAD^{commit}'
libra cat-file -t 'HEAD^{tree}'
libra cat-file -p 'HEAD:docs/guide.md'
libra rev-parse --short HEAD
libra rev-parse --show-toplevel
libra --json rev-parse HEAD
! libra rev-parse no-such-revision
! libra rev-parse --verify 0000000000000000000000000000000000000000

libra show --no-patch HEAD
libra show HEAD:docs/guide.md
libra --json show HEAD
libra show-ref --head
libra show-ref --head --no-head
libra show-ref --heads
libra show-ref --branches
libra show-ref --hash --heads
libra show-ref --abbrev=12 --heads
libra show-ref --hash=12 --heads
libra show-ref --no-hash --heads
libra show-ref --abbrev=12 --no-abbrev --heads
libra --json show-ref --abbrev=12 --heads
libra show-ref --verify refs/heads/main
libra show-ref --verify HEAD
libra show-ref --exists refs/heads/main
libra show-ref --verify --no-verify main
libra show-ref --exists --no-exists refs/heads/main
! libra show-ref --verify main
! libra show-ref --exists refs/heads/missing

libra cat-file -t "$HEAD_ID"
libra cat-file -s "$HEAD_ID"
libra cat-file -p "$HEAD_ID"
libra cat-file -e "$HEAD_ID"
printf 'loose blob\n' > loose.txt
BLOB_ID="$(libra hash-object -w loose.txt)"
libra cat-file -t "$BLOB_ID"
libra cat-file -p "$BLOB_ID"
libra show "$BLOB_ID"
libra --json hash-object loose.txt
libra hash-object --no-filters loose.txt
printf 'loose blob\n' | libra hash-object --stdin
printf 'loose blob\n' | libra --json hash-object --stdin --path loose.txt
! printf 'loose blob\n' | libra hash-object --stdin --path loose.txt --no-filters
! libra hash-object -t bogus loose.txt

printf 'rev-list second\n' > docs/rev-list.md
libra add docs/rev-list.md
libra config user.name "Rev List Committer"
libra config user.email rev-list-committer@example.com
libra commit -m "test: rev-list second" --author "Rev List Author <rev-list@example.com>" --no-verify
test "$(libra rev-parse 'HEAD@{0}')" = "$(libra rev-parse HEAD)"
test "$(libra rev-parse 'HEAD@{1}')" = "$HEAD_ID"
test "$(libra rev-parse '@{1}:docs/guide.md')" = "$GUIDE_ID"
libra rev-list HEAD
libra rev-list HEAD HEAD~1
libra rev-list HEAD~1..HEAD
libra rev-list ^HEAD~1 HEAD
libra rev-list HEAD~1...HEAD
libra rev-list --count HEAD
libra rev-list -n 1 HEAD
libra rev-list --skip 1 --max-count 1 HEAD
libra rev-list --count --since 0 HEAD
libra rev-list --count --after 0 HEAD
libra rev-list --count --until 0 HEAD
libra rev-list --count --before 0 HEAD
libra rev-list --count --min-parents 1 --no-min-parents HEAD
libra rev-list --count --max-parents 0 --no-max-parents HEAD
libra rev-list --count --first-parent HEAD
libra rev-list --author rev-list@example.com HEAD
libra rev-list --count --author missing-author HEAD
libra rev-list --committer rev-list-committer@example.com HEAD
libra rev-list --count --committer missing-committer HEAD
libra rev-list --grep "rev-list second" HEAD
libra rev-list --grep "object readback" --grep "rev-list second" HEAD
libra rev-list --count --grep "REV-LIST SECOND" HEAD
libra rev-list HEAD -- docs/rev-list.md
libra rev-list HEAD -- README.md
libra --json rev-list HEAD -- docs/rev-list.md
libra --json rev-list HEAD
libra branch rev-right HEAD~1
libra switch rev-right
printf 'rev-list second\n' > docs/rev-list.md
libra add docs/rev-list.md
libra commit -m "test: rev-list equivalent right" --no-verify
RIGHT_SAME_ID="$(libra rev-parse HEAD)"
printf 'right only\n' > docs/right-only.md
libra add docs/right-only.md
libra commit -m "test: rev-list right only" --no-verify
RIGHT_UNIQUE_ID="$(libra rev-parse HEAD)"
libra rev-list --left-right main...rev-right
libra rev-list --left-only main...rev-right
libra rev-list --right-only main...rev-right
libra rev-list --cherry-pick main...rev-right
libra rev-list --cherry-mark main...rev-right
libra rev-list --cherry main...rev-right
libra rev-list --left-right --cherry main...rev-right
libra rev-list --count --left-right --cherry-mark main...rev-right
libra rev-list --count --cherry main...rev-right
libra rev-list --count --left-right --cherry main...rev-right
libra --json rev-list --cherry-pick main...rev-right
libra --json rev-list --cherry main...rev-right
libra switch main
libra rev-list --children HEAD
libra rev-list --count --children HEAD
libra --json rev-list --children --skip 1 --max-count 1 HEAD
! libra rev-list --parents --children HEAD
LATEST_HEAD="$(libra rev-parse HEAD)"
libra fsck
libra fsck --connectivity-only
libra fsck "$HEAD_ID"
libra tag -m "release fixture" v1.0
libra tag v1-light
TAG_ID="$(libra rev-parse refs/tags/v1.0)"
test "$(libra rev-parse 'v1.0^{tag}')" = "$TAG_ID"
test "$(libra rev-parse 'v1.0^{}')" = "$LATEST_HEAD"
test "$(libra rev-parse 'refs/tags/v1.0^{commit}')" = "$LATEST_HEAD"
libra branch --list --points-at refs/tags/v1.0
libra show-ref --branches --no-branches
libra show-ref --tags --no-tags
libra show-ref --dereference --tags v1.0
libra show-ref --dereference --no-dereference --tags v1.0
libra for-each-ref --points-at "$LATEST_HEAD" --format='%(refname) %(objecttype)'
libra --json for-each-ref --points-at "$LATEST_HEAD"
! libra cat-file -t deadbeef
```

关键断言：

- `rev-parse` 的 `@`、数字 reflog、typed/recursive peel、完整 tag ref、`REV:path`、`--verify` 存在性校验，以及 `show`、`show-ref`、`for-each-ref`、`branch --points-at`、`cat-file`、`hash-object`（含 `--path` / `--no-filters` 兼容入口）、`rev-list`、`fsck` 当前正向路径可用。
- `rev-list --count` 输出过滤后的提交数量；`rev-list -n` 限制输出行数；`rev-list --skip --max-count` 可跳过当前 HEAD 后定位父提交；`--since` / `--after` 与 `--until` / `--before` 时间过滤可观察；multi revision、`A..B`、`^A`、`A...B`、`--first-parent`、`--author`、`--committer`、`--grep`、`-- <path>` path limitation、`--left-right`、`--right-only`、`--cherry-pick`、`--cherry-mark`、`--cherry`、`--children` 和 parent bound reset aliases 均有正向断言；重复 `--grep` 按 OR 匹配，默认大小写敏感，path limitation 会在 JSON 中回显 `pathspecs[]`，`--cherry-pick` 会在 JSON 中回显 `cherry_pick` 并限制 `commits[]`，`--cherry` 会在 JSON 中回显 `cherry` 并保持 `cherry_mark=false`，`--children` 会在 JSON 中回显 `children` 并通过 `entries[].children[]` 保留 child 元数据，`--parents --children` 会被解析层拒绝，`--count --left-right --cherry-mark` 与 `--count --left-right --cherry` 输出 Git 兼容三字段计数。
- `show-ref --branches` 与 `--heads` 输出一致；`--no-branches` / `--no-tags` reset aliases 恢复默认 branch+tag 范围；`show-ref --abbrev=12` / `--hash=12` 输出 HEAD 的 12 位前缀；`--no-abbrev` 恢复完整哈希，`--no-hash` 按 Git 行为作为 hash-only alias；`show-ref --dereference` 对 annotated tag 输出 `refs/tags/<name>^{}` peeled 行，`--no-dereference` 取消 peeled 行；`branch --points-at refs/tags/v1.0` 把附注标签剥离到 commit；`--no-head`、`--no-verify`、`--no-exists` 可恢复对应默认行为；`show-ref --verify` 只接受完整 refname / `HEAD`；`show-ref --exists` 成功静默，缺失 ref 失败。
- `for-each-ref --points-at` 对 branch、lightweight tag 和 annotated tag peeled target 的过滤可观察；`--json` 返回标准 envelope。
- 缺失 revision/object 和非法 hash-object 类型必须失败。
- `ls-files`、高级 `for-each-ref --contains/--merged`、日期/upstream/push/checkout reflog selector、`rev-list --objects*` 对象枚举遍历输出不属于当前场景正向覆盖。（`rev-list --boundary` 已实现，由单元/集成测试 `test_rev_list_boundary` 覆盖。）
