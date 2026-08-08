#!/bin/sh
# tests/compat-ledger/t4/SELECTION.sh — 复算 t4 首批候选。
# 约定：所有管道都拆成「生产者写临时文件 + 显式检查退出码 + 消费者单独运行」。
# POSIX sh 没有 pipefail，`producer | sort > f` 的退出码取自 sort，会吞掉生产者失败。
# grep 的退出码 1（零命中）是合法结果，>1 才是执行失败，两者必须分开判定。
# 用法：GRIT_REPO=<grit 仓库路径> sh tests/compat-ledger/t4/SELECTION.sh
# 输出：选中的 12 个文件名，逐行写 stdout（EXPECTED.txt 与此逐行比对）。
#
# ——— 场景展开记录（R74/R75 P0：CT3-01 的 MANIFEST 下界 oracle 逐行 `grep -qxF` 本节）———
# 上游一个 `test_expect_*` 调用点可能在循环里展开成多个场景，所以 `MANIFEST.tsv` 的
# （本文件写 `test_expect_*` 而不写全名：净室 token 筛子按**原始文本**拒绝六个上游 harness
#  标识符，`tests/compat-ledger/` 下除检测器自身与故意违规样本外一律零命中，本文件不在
#  其排除名单内。这只影响注释怎么称呼那个函数，不影响流水线本体。）
# 行数会多于静态调用点数。凡出现这种情况的 stem，**必须**在本节留下一行，格式**恰为**
#     `# expansion <stem> <static> -> <expanded>`
# （无尾随文字——oracle 用 `grep -qxF` 整行精确匹配；理由写在紧邻的下一行注释里）。
# 两条已实测的展开（R13 复核）：
# expansion t4000-diff-format 11 -> 41
#   理由：`for` 循环按 diff 格式逐项展开，一个调用点产生 41 个场景。
# expansion t4017-diff-retval 30 -> 38
#   理由：返回码矩阵在两处调用点内循环展开。
# 其余 stem 的 static/expanded 值由 CT3-01 在跑 MANIFEST 覆盖门时取得——门失败时会打印
# `FAIL: <stem> expands <static> -> <expanded> without a '# expansion' note in SELECTION.sh`，
# 把该行按上面的格式补进本节即可，**不得**反过来去改 `MANIFEST.tsv` 或放宽 oracle。
# ——— 场景展开记录结束 ———
#
# ——— MANIFEST.tsv 的人工复核记录（CT3-01，2026-08-08，复核人 Quanyi Ma）———
# 展开方法：只读 pin 快照（`git archive <PIN> tests`），逐文件取
# `test_expect_*`（success / failure 两种）的**标题实参**，规范化为 slug。上游语料
# 从不执行——这是 clean-room 边界，也是 `MANIFEST.tsv` 不能由跑测试产生的原因。
# 标题实参有三种写法，三种都要认：单引号、双引号（含 `$var`）、以及**不加引号的裸词**
# （`t4010-diff-pathspec.sh` 的 `setup` 就是裸词）。裸词与前置条件（prereq）的区分按上游
# 惯例——prereq 全大写（`SYMLINKS`、`!MINGW`），其余裸词是标题。
# 循环展开只认**包裹调用点**的循环：列 0 起的 `for`，或已在这类循环内部再嵌一层。
# `t4001-diff-rename.sh` 在**测试体内部**用循环造重命名候选（缩进、且此时不在任何包裹循环
# 内），把它当成包裹循环会把该文件从 23 个场景虚增到 95 个——故不计。
# slug 去重：上游允许同名测试重复出现（`t4000` 的 `--no-patch clears all previous ones`
# 在 11 次循环里标题完全相同），但场景身份必须唯一，故同名者按出现顺序追加 `-2`、`-3`…。
# 复核结果：12 个 stem 共 178 行，两处展开如上，无重复场景身份。
# ——— 人工复核记录结束 ———
set -eu
PIN=dfb079967b9cbc99e533c21e65f674bb3f5e8b07
# 命令清单集合摘要（2026-07-29 实测；随 COMPATIBILITY.md 主表变化时必须同批更新本值与预期清单）
EXPECTED_INVENTORY_DIGEST=64b63a7b8370c80ad0cc3ed1d0246460ef83680661cdb2bc5c3c269238c6873b
: "${GRIT_REPO:?set GRIT_REPO to the grit checkout}"
# LIBRA_ROOT 优先取显式环境变量；否则从脚本位置推导，并强制校验命中的确是 Libra 仓库根。
LIBRA_ROOT=${LIBRA_ROOT:-$(CDPATH= cd -- "$(dirname -- "$0")/../../.." && pwd)}
[ -f "$LIBRA_ROOT/COMPATIBILITY.md" ] && [ -d "$LIBRA_ROOT/.libra" ] \
  || { echo "FAIL: LIBRA_ROOT=$LIBRA_ROOT is not a Libra repo root; set LIBRA_ROOT explicitly" >&2; exit 1; }
head=$(git -C "$GRIT_REPO" rev-parse HEAD)
[ "$head" = "$PIN" ] || { echo "FAIL: grit HEAD $head != pinned $PIN" >&2; exit 1; }
# 仅 pin HEAD 不够：tests/ 下的已跟踪修改或未跟踪文件会改变枚举结果而不改变 SHA。
# 必须显式捕获 git 的退出码——`[ -z "$(git ...)" ]` 在 git 失败时同样得到空串而误判为干净。
st=$(git -C "$GRIT_REPO" status --porcelain -- tests) \
  || { echo "FAIL: git status failed in $GRIT_REPO" >&2; exit 1; }
[ -z "$st" ] || { echo "FAIL: grit tests/ is dirty; clean it before recomputing" >&2; exit 1; }
W=$(mktemp -d)
# R41 P1：清理只绑 EXIT；信号各自以标准码退出，避免吞掉 INT/TERM 语义
# R76 P1：删除失败不得被成功退出码掩盖——中间产物留在盘上却报 0，复算证据从此不可信
_sel_cleanup() {
  r=$?; trap - EXIT
  set +e                              # R77 P1（自审）：同 `_gd_cleanup`，见那里的说明
  if ! rm -rf "$W"; then
    echo "FATAL: cannot remove $W — intermediate files left on disk" >&2
    if [ "$r" -eq 0 ]; then r=3; fi
  fi
  exit "$r"
}
trap '_sel_cleanup' EXIT; trap 'exit 130' INT; trap 'exit 143' TERM

# **R73 P1（GC-15 同轴）**：上面的 pin + 干净树断言只是**瞬时**检查——枚举与三段 `grep`
# 之后要跑好几分钟，期间任何人 `git checkout`/编辑 `$GRIT_REPO/tests` 都会让候选集来自
# 另一棵树，而事后无从察觉（SHA 没变、`EXPECTED.txt` 却对不上或恰好对得上）。改为**先把
# pin 的 `tests/` 整棵物化到私有目录，全部扫描只读该副本**；工作树此后不再被读取。
git -C "$GRIT_REPO" archive --format=tar "$PIN" tests > "$W/pin.tar" \
  || { echo "FAIL: cannot materialize tests/ from the pin" >&2; exit 1; }
( cd "$W" && tar xf pin.tar ) || { echo "FAIL: cannot unpack the pin snapshot" >&2; exit 1; }
[ -d "$W/tests" ] || { echo "FAIL: pin snapshot has no tests/ directory" >&2; exit 1; }
rm -f "$W/pin.tar"

# 步骤 0：全集（t4 四位编号）——只在 pin 快照内枚举
cd "$W/tests"
command ls t4[0-9][0-9][0-9]-*.sh > "$W/all.raw" || { echo "FAIL: ls t4*.sh" >&2; exit 1; }
LC_ALL=C sort "$W/all.raw" > "$W/all"
[ "$(wc -l < "$W/all")" -eq 180 ] || { echo "FAIL: stage0 count != 180" >&2; exit 1; }

# 步骤 1：结构筛选（排除仓库布局 / 上游 helper / 外部服务 / submodule / update-ref）
# **R76 P1**：`$(cat "$W/all")` 的失败无法成为 `grep` 的退出码——部分输出会让 `grep` 只扫到
# 候选集的一部分，**空输出更糟**：`grep` 拿不到任何文件名，就会去读脚本的 stdin（阻塞，或者
# 用无关输入算出「结果」）。改为先把清单读进位置参数并显式断言非空，再以 `"$@"` 传给 grep。
# 注意：`set -- $(cat f)` 的退出码是 `set` 的、**永远是 0**，命令替换的失败不会传出来——
# 所以真正的守卫是紧随其后的**基数断言**：读失败（空）或读了一半（少）都会在这里被拦下。
set -- $(cat "$W/all")
[ $# -eq 180 ] || { echo "FAIL: stage0 list expanded to $# entries, expected 180" >&2; exit 1; }
if command grep -LE '\.git/|\$GIT_DIR/|--git-path|(^|[^-[:alnum:]_])test-tool([[:space:]]|$)|lib-httpd\.sh|lib-git-daemon\.sh|lib-git-p4\.sh|lib-git-svn\.sh|lib-cvs\.sh|lib-gitweb\.sh|lib-gpg\.sh|(^|[^-[:alnum:]_])git[[:space:]]+submodule([[:space:]]|$)|(^|[^-[:alnum:]_])git[[:space:]]+update-ref([[:space:]]|$)' \
    "$@" > "$W/struct.raw"; then rc=0; else rc=$?; fi
# 注意：`cmd > f; rc=$?` 在 `set -e` 下会因 grep 的合法退出码 1 直接终止脚本，
# 必须用 if/else 捕获状态；1 = 零命中（合法），>1 = 执行失败。
[ "$rc" -le 1 ] || { echo "FAIL: grep -L failed with $rc" >&2; exit "$rc"; }
LC_ALL=C sort "$W/struct.raw" > "$W/struct"
[ "$(wc -l < "$W/struct")" -eq 145 ] || { echo "FAIL: stage1 count != 145" >&2; exit 1; }

# 步骤 2：命令面筛选。命令清单 = COMPATIBILITY.md「Top-level commands」主表中
# **tier 为 `supported` 或 `partial`** 的数据行（2026-07-29 修订，ADR-CT-06 的要求 1）。
# 主表 108 个数据行 = 72 `partial` + 32 `intentionally-different` + 4 `supported`；
# 只取前两类得 76 行。**必须过滤 tier**：`intentionally-different` 是「Libra 与 Git
# 设计有意不一致」的登记位，其命令的上游测试按 ADR-CT-06 一律不迁移，因此不能进候选清单。
# 该表本身**不含** unsupported 行；`submodule`/`sparse-checkout`/`send-email` 位于独立的
# 「intentionally absent」表 :260-262，因此天然不在清单内，无需另做减法。
# 实测（2026-07-29，grit pin `dfb0799`）：108 清单与 76 清单在 t4 族上产出**完全相同**的
# 步骤 2 结果与相同的首批 12 行——32 个 `intentionally-different` 里只有 `worktree`
# 是真实的 Git 命令，其余是 Libra 专有扩展（`code`/`agent`/`layer`/`hydrate` 等），
# 上游 Git 套件中不会出现。因此本次收紧对既有计数门与 `EXPECTED.txt` 零影响，纯属对
# 后续 wave（尤其是覆盖 `git worktree` 的族）的前置防护。
# 表格边界按**标题**定位，不用硬编码行号（行号会随并发计划改动 COMPATIBILITY.md 而漂移，
# 且「漏一行 + 误纳一行」的等量漂移能骗过纯计数门）。
awk '/^## Top-level commands/{inside=1; next} inside && /^## /{exit} inside {split($0,f,"|"); gsub(/^[ \t]+|[ \t]+$/,"",f[2]); gsub(/^[ \t]+|[ \t]+$/,"",f[3]); if (f[2]!="" && f[2]!="Command" && f[2]!~/^-+$/ && (f[3]=="supported" || f[3]=="partial")) print f[2]}' \
    "$LIBRA_ROOT/COMPATIBILITY.md" > "$W/cmds.raw" || { echo "FAIL: awk COMPATIBILITY.md" >&2; exit 1; }
LC_ALL=C sort -u "$W/cmds.raw" > "$W/cmds"
[ "$(wc -l < "$W/cmds")" -eq 76 ] || { echo "FAIL: command inventory != 76" >&2; exit 1; }
# 计数之外再钉住**集合本身**：等量漂移（漏一行 + 误纳一行）计数不变，摘要必变。
# R34 P1：拆开 sort|shasum|cut 管道——POSIX sh 管道退出码取自末命令，sort/shasum 失败会被吞
LC_ALL=C sort "$W/cmds" > "$W/cmds.sorted" || { echo "FAIL: sort cmds" >&2; exit 1; }
inv_line=$(shasum -a 256 "$W/cmds.sorted") || { echo "FAIL: shasum cmds" >&2; exit 1; }
inv_digest=${inv_line%% *}
[ "$inv_digest" = "$EXPECTED_INVENTORY_DIGEST" ] \
  || { echo "FAIL: command inventory digest $inv_digest != $EXPECTED_INVENTORY_DIGEST" >&2; exit 1; }
: > "$W/final"
while read -r f; do
  bad=0
  if command grep -hoE '(^|[^-[:alnum:]_./"])git( +-[^ ]+)* +[a-z][a-z0-9-]*' "$f" > "$W/subs.raw"; then rc=0; else rc=$?; fi
  [ "$rc" -le 1 ] || { echo "FAIL: grep subcommands in $f failed with $rc" >&2; exit "$rc"; }
  awk '{print $NF}' "$W/subs.raw" > "$W/subs.f" || { echo "FAIL: awk subs" >&2; exit 1; }
  LC_ALL=C sort -u "$W/subs.f" > "$W/subs"
  # **零识别必须失败**（2026-07-29 修订）：`for sub in $(cat 空文件)` 的循环体一次都不执行，
  # `bad` 保持 0，文件反而被无条件放行——实测 t4 的 145 个 stage-1 幸存者里有 5 个命中此路径，
  # 其中 t4070/t4076 是 submodule 测试（Libra 已按 D1 declined），正是必须排除的那类。
  if command grep -c . "$W/subs" > "$W/nsub"; then :; else
    rc=$?; [ "$rc" -eq 1 ] || { echo "FAIL: grep subs failed with $rc" >&2; exit "$rc"; }; echo 0 > "$W/nsub"
  fi
  nsub=$(cat "$W/nsub")
  if [ "$nsub" -eq 0 ]; then
    echo "$f" >> "$W/norecog"
    echo "$f | no recognized subcommand" >&2      # ← 固定短语，CT3-01 的计数门据此断言
    continue
  fi
  while read -r sub; do
    # R34 P1：区分 rc=1（不在清单）与 rc>1（grep 执行失败）——I/O 错误不得被当成普通排除
    if command grep -qx "$sub" "$W/cmds"; then :; else
      rc=$?
      [ "$rc" -eq 1 ] || { echo "FAIL: grep cmds failed with $rc" >&2; exit "$rc"; }
      bad=1; break
    fi
  done < "$W/subs"
  [ "$bad" -eq 0 ] && echo "$f" >> "$W/final"
done < "$W/struct"
LC_ALL=C sort -o "$W/final" "$W/final"
[ "$(wc -l < "$W/final")" -eq 119 ] || { echo "FAIL: stage2 count != 119" >&2; exit 1; }
# 零识别文件单独计数并落 stderr，使「解析器覆盖不到的写法」可见而不是静默放行。
if [ -f "$W/norecog" ]; then nz=$(wc -l < "$W/norecog"); else nz=0; fi
[ "$nz" -eq 5 ] || { echo "FAIL: no-recognized-subcommand count $nz != 5" >&2; exit 1; }

# 步骤 3：确定性截断。选中清单写 stdout（EXPECTED.txt 与之比对）；
# 溢出清单写 stderr（与 stdout 的选中清单分开），不落独立文件。
head -12 "$W/final"                                                           # ← stdout：首批 12
# 溢出段写 stderr（与 stdout 的选中清单分开，便于 EXPECTED.txt 逐行比对）
{
  echo "### overflow (file-level; not [[scenario]] rows)"
  # R34 P1：拆开 tail|sed 管道——tail 失败会被 sed 的 0 退出码吞掉
  tail -n +13 "$W/final" > "$W/overflow.raw" || { echo "FAIL: tail overflow" >&2; exit 1; }
  sed 's#$# | blocked | DEFER-02#' "$W/overflow.raw"
} >&2                                                                          # 溢出 107
