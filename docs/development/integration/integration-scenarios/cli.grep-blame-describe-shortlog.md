### `cli.grep-blame-describe-shortlog`

目的：覆盖 history inspection 剩余命令：`grep`、`blame`、`describe`、`shortlog` 的常用参数和失败路径；其中 `describe` 覆盖 `--tags`、`--always`、`--abbrev`、`--exact-match`、`--long` 和 `--dirty`。

最小步骤：

```bash
SCENARIO="cli.grep-blame-describe-shortlog"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"
# (prelude provides libra() -- converged short form, long wrapper removed)

libra init inspect-repo
cd inspect-repo
libra config set user.name "Libra Inspect Test"
libra config set user.email "inspect@example.invalid"
mkdir -p docs src
printf 'Alpha\nBeta\n' > docs/guide.txt
printf 'fn main() { println!("alpha"); }\n' > src/main.rs
libra add docs/guide.txt src/main.rs
libra commit -m "feat: add inspect files"
libra tag -m "inspect release" v1.0.0
libra describe --exact-match HEAD          # HEAD 恰好位于 tag 上，输出 v1.0.0
printf 'Gamma\n' >> docs/guide.txt
libra add docs/guide.txt
libra commit -m "fix: update guide"

libra grep Alpha docs
libra grep -F 'println!("alpha")' src
libra grep -i gamma docs/guide.txt
libra grep -n -e Alpha -e Gamma docs/guide.txt
libra grep -c Alpha docs/guide.txt
libra grep -l alpha src
libra grep -L Alpha src                    # 列出无匹配的文件
libra grep -z -l Alpha docs                # NUL 结尾的路径列表
libra grep --tree HEAD~1 Alpha docs/guide.txt
printf 'Gamma\n' > patterns.txt
libra grep -f patterns.txt docs/guide.txt
printf 'StagedMarker\n' >> docs/guide.txt
libra add docs/guide.txt
libra grep --cached StagedMarker docs/guide.txt   # 只搜索暂存区内容
libra restore --staged docs/guide.txt
libra restore docs/guide.txt
printf 'Alpha loose\n' > loose.txt
libra grep --untracked 'Alpha loose'       # 连同未跟踪文件一起搜索
rm loose.txt
libra blame docs/guide.txt
libra blame -L 1,2 docs/guide.txt HEAD
libra blame --porcelain docs/guide.txt     # 机器可读头部 + tab 前缀内容行
libra describe --tags HEAD
libra describe --long --tags HEAD          # 精确匹配时输出 v1.0.0-0-gHASH
libra describe --always --abbrev 12 HEAD
printf 'Delta\n' >> docs/guide.txt
libra describe --tags --dirty              # 工作区有改动时输出带 -dirty 后缀
libra restore docs/guide.txt
libra describe --tags --dirty              # 恢复后不再有 -dirty 后缀
libra config set user.name "Second Inspect Author"
libra config set user.email "second@example.invalid"
printf 'extra\n' > extra.txt
libra add extra.txt
libra commit -m "chore: second author"
libra shortlog
libra shortlog -s
libra shortlog -n
libra shortlog -s -e                       # 作者邮箱出现在汇总行
libra shortlog --format "%an %s"           # 占位符重写 subject 行
libra shortlog -s -n --top 1               # 仅保留最高产作者
libra shortlog -s --min-count 2            # 过滤低于阈值的作者
libra shortlog -s -n --reverse             # 反转排名顺序
libra shortlog -s "$(libra rev-parse HEAD~1)"   # 位置参数限制可达历史

# Verify JSON outputs for AI Agent readability
libra --json grep Alpha docs >grep.json
python3 -c "import json; d=json.load(open('grep.json')); assert d['ok'] is True; assert 'matches' in d['data'] or isinstance(d['data'].get('matches'), list)"
libra --json blame docs/guide.txt >blame.json
python3 -c "import json; d=json.load(open('blame.json')); assert d['ok'] is True; assert 'lines' in d['data'] or isinstance(d['data'].get('lines'), list)"
libra --json describe --tags HEAD >describe.json
python3 -c "import json; d=json.load(open('describe.json')); assert d['ok'] is True; assert 'resolved_commit' in d['data'] or 'result' in d['data']"
libra --json shortlog >shortlog.json
python3 -c "import json; d=json.load(open('shortlog.json')); assert d['ok'] is True; assert 'authors' in d['data'] or isinstance(d['data'].get('authors'), list)"
```

负向步骤：

```bash
cd "$RUN_DIR/inspect-repo"
set +e
libra grep no-such-pattern docs/guide.txt
grep_no_match_status=$?
libra grep --tree no-such-revision Alpha docs/guide.txt
grep_bad_tree_status=$?
libra grep -P Alpha docs/guide.txt
grep_perl_status=$?
set -e
test "$grep_no_match_status" -eq 1
test "$grep_bad_tree_status" -eq 2
test "$grep_perl_status" -eq 2
! libra blame -L bad docs/guide.txt
! libra blame missing.txt
! libra describe no-such-revision
! libra describe --long --abbrev=0         # Git 也拒绝该组合
! libra describe --exact-match            # HEAD 已越过 tag，必须失败
```

断言：`grep` 可在工作区、指定 pathspec、pattern file、暂存区（`--cached`）、未跟踪文件（`--untracked`）和历史 tree 中匹配内容，`-F` / `-i` / `-n` / `-c` / `-l` / `-L` 输出可用于脚本断言，`-z -l` 输出 NUL 结尾路径且无尾随换行；`grep` 退出码遵循 Git grep 合同：命中 0、无命中 1 且 stderr 静默、命令错误（例如非法 revision 或 `-P`）2；`blame` 输出每行作者和提交信息，`-L` 限制行范围，COMMIT 位置参数按历史版本归因，`--porcelain` 输出 `author ` / `author-mail <...>` 键值头部与 tab 前缀内容行；`describe --tags` 使用可达 tag，`--always --abbrev` 在需要时输出短 hash，`--long` 在精确匹配时输出 `tag-0-gHASH`，`--exact-match` 仅在 HEAD 恰好位于 tag 时成功，`--dirty` 仅在跟踪内容偏离 HEAD 时追加后缀，`--long --abbrev=0` 必须失败；`shortlog` 默认、summary、排序、`-e` 邮箱、`--format` 占位符、`--top` / `--min-count` / `--reverse` 过滤排序与位置 revision 限制都能按作者汇总；无匹配 grep、非法 revision、非法 blame 范围、缺失文件、越过 tag 的 `--exact-match` 必须失败且不改变仓库。

补充可执行断言：
- `libra --json grep Alpha docs` 必须 `ok:true` 且 `data.matches[]` 可解析。
- `libra --json blame -L 1,1 docs/guide.txt` 验证结构包含 author / commit 信息。
- `libra --json describe --tags` 成功且包含 tag 信息。
- `libra --json shortlog` 返回按作者汇总的结构。
- 负向 `libra grep` 无匹配必须退出 1 且 stderr 静默；grep 命令错误必须退出 2；`libra blame` 非法范围必须非 0，stderr 包含可识别错误（可选 LBR-）。
