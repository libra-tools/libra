#!/bin/sh
# plan-20260729 CT3-06 — the clean-room overlap gate, and the ONLY
# implementation of it. CT3-04 and CT3-02 call this; neither reimplements it.
# Its normative content is the block the plan fixes as a plan-level rule; the
# body below is that block verbatim.
#
# Usage: TARGET=<file> GRIT_REPO=<pin checkout> sh CLEANROOM.sh
set -eu
# **ER-11（自审 P1）**：本脚本是 CT3-06 的**已提交交付物**，随推送公开——绝不能内联任何
# 审阅者的私有绝对路径。与 `SELECTION.sh` 同口径，改由调用方 export，缺失即失败。
: "${GRIT_REPO:?set GRIT_REPO to the grit checkout (never hard-code an absolute path)}"
GD=$(mktemp -d)
_tmp_c() {
  r=$?; [ -n "${1:-}" ] && r=$1
  trap - EXIT INT TERM
  set +e
  if ! rm -rf "$GD"; then
    echo "FATAL: cannot remove $GD — intermediate files left on disk" >&2
    if [ "$r" -eq 0 ]; then r=3; fi
  fi
  exit "$r"
}
trap '_tmp_c' EXIT; trap '_tmp_c 130' INT; trap '_tmp_c 143' TERM
PIN=dfb079967b9cbc99e533c21e65f674bb3f5e8b07
[ "$(git -C "$GRIT_REPO" rev-parse HEAD)" = "$PIN" ] || { echo "FAIL: grit HEAD != pin"; exit 1; }
st=$(git -C "$GRIT_REPO" status --porcelain -- tests) \
  || { echo "FAIL: git status failed in $GRIT_REPO"; exit 1; }
[ -z "$st" ] || { echo "FAIL: grit tests/ is dirty; overlap check needs a clean pinned tree"; exit 1; }
# **语料按内容绑定（自审 P2）**：`rev-parse HEAD == PIN` + `status --porcelain` 为空**不足以**
# 证明工作区文件就是 pin 里的内容——`skip-worktree` / `assume-unchanged` 的条目既不出现在
# status，也不影响 HEAD。故：① 拒绝任何 `S`/`h` 标记；② 对 `EXPECTED.txt` 的 12 个源逐个
# 比对工作区文件与 pin 中 blob 的 sha256。
git -C "$GRIT_REPO" ls-files -v -- tests > "$GD/grit_lsfiles.txt" \
  || { echo "FAIL: git ls-files -v failed in the grit checkout"; exit 1; }
if command grep -nE '^[Sh]' "$GD/grit_lsfiles.txt"; then
  echo "FAIL: grit tests/ has skip-worktree / assume-unchanged entries — corpus is not trustworthy"; exit 1
else
  rc=$?; [ "$rc" -eq 1 ] || { echo "FAIL: grep failed with $rc"; exit "$rc"; }
fi
# R79 P1：stem 清单只读冻结的 EXPECTED_SNAP（调用方 GC-15 快照），禁止工作树字面路径
: "${EXPECTED_SNAP:?export EXPECTED_SNAP to a frozen copy of tests/compat-ledger/t4/EXPECTED.txt (GC-15)}"
[ -s "$EXPECTED_SNAP" ] || { echo "FAIL: EXPECTED_SNAP missing or empty: $EXPECTED_SNAP"; exit 1; }
export EXPECTED_SNAP
while read -r stem; do
  [ -n "$stem" ] || continue
  f="tests/$stem"
  wt=$(shasum -a 256 "$GRIT_REPO/$f") || { echo "FAIL: cannot hash $f in the worktree"; exit 1; }
  git -C "$GRIT_REPO" show "$PIN:$f" > "$GD/pin_blob" \
    || { echo "FAIL: $f is not present at the pin"; exit 1; }
  pb=$(shasum -a 256 "$GD/pin_blob") || { echo "FAIL: cannot hash the pinned blob of $f"; exit 1; }
  [ "${wt%% *}" = "${pb%% *}" ] \
    || { echo "FAIL: $f differs from the pinned blob — corpus substituted"; exit 1; }
done < "$EXPECTED_SNAP"
# **R72 P1（TOCTOU）**：摘要核对之后**不得再打开工作树文件**——源文件可在检查后被临时替换、
# 扫描结束前还原。逐个把 pin blob 物化到私有快照目录，后续重叠扫描**只读这些副本**。
mkdir -p "$GD/grit" || { echo "FAIL: cannot create the grit snapshot dir"; exit 1; }
while read -r stem; do
  [ -n "$stem" ] || continue
  mkdir -p "$GD/grit/$(dirname "tests/$stem")" || { echo "FAIL: mkdir snapshot"; exit 1; }
  git -C "$GRIT_REPO" show "$PIN:tests/$stem" > "$GD/grit/tests/$stem" \
    || { echo "FAIL: cannot materialise the pinned blob for $stem"; exit 1; }
done < "$EXPECTED_SNAP"
GRIT_SNAPSHOT="$GD/grit"; export GRIT_SNAPSHOT   # 下方扫描器一律读 $GRIT_SNAPSHOT，不读 $GRIT_REPO
# **R78/R79 P1**：Python 只读 GRIT_SNAPSHOT；EXPECTED_SNAP 已在物化循环前强制要求
GRIT_SNAPSHOT="$GRIT_SNAPSHOT" EXPECTED_SNAP="$EXPECTED_SNAP" \
  TARGET="${TARGET:-tests/command/t4_port_test.rs}" python3 - <<'PY'
import difflib, os, re, sys
grit = os.environ["GRIT_SNAPSHOT"]   # R78：只读 pin 物化副本

def norm(t):
    # 2026-07-29（ADR-CT-06 同批）：**不剥离注释**。Rust 的 `///` 以 `//` 开头，
    # 在「测试必须写详细注释」的契约下，抄上游描述是最省事的写法，剥离注释会让这条通道免检。
    return re.sub(r"\s+", " ", t).lower()

def grams(t, n=8):
    w = t.split()
    return {" ".join(w[i:i+n]) for i in range(max(0, len(w) - n + 1))}

bad = 0
# R29 P0：检查目标由 TARGET 环境变量指定（sh 包装层已给缺省值）——CT3-02 查最终测试
# 文件、CT3-04 查本轮活跃投影 `$RUN_DIR/draft.active.rs`、负例门查负例样本，三者共用本实现
targets = [os.environ["TARGET"]]
# token 筛子（三层之一，与长子串/n-gram 同一退出点；上游 harness 标识符的唯一清单）
TOKENS = ["test_expect_success", "test_expect_failure", "test_cmp",
          "test_when_finished", "TEST_DIRECTORY", "test-lib.sh"]
exp = os.environ.get("EXPECTED_SNAP", "tests/compat-ledger/t4/EXPECTED.txt")
if not os.path.exists(exp):
    print("FAIL: EXPECTED.txt missing"); sys.exit(1)
# grit 已是 $GRIT_SNAPSHOT 根，其下 layout 为 tests/<stem>
sources = [os.path.join(grit, "tests", l.strip()) for l in open(exp) if l.strip()]
if not sources:
    print("FAIL: EXPECTED.txt is empty"); sys.exit(1)

# 白名单与其摘要 sidecar 无条件必须存在（CT2-03 交付物，R35 拆分；R31 P1——不存在「无白名单」的
# 合法状态，缺任一/格式坏/不匹配一律失败，不依赖执行期环境变量）
import hashlib
# R39：路径可经环境变量指向**临时副本**（篡改负例门专用）；缺省即为 CT2-03 交付的原文件
ap = os.environ.get("PHRASE_ALLOWLIST", "tests/compat-ledger/PHRASE_ALLOWLIST.txt")   # CT2-03 交付，CT3-02 不可改
sc = os.environ.get("PHRASE_SIDECAR", "tests/compat-ledger/PHRASE_ALLOWLIST.sha256")
if not os.path.exists(ap):
    print("FAIL: PHRASE_ALLOWLIST.txt missing (CT2-03 deliverable)"); sys.exit(1)
if not os.path.exists(sc):
    print("FAIL: PHRASE_ALLOWLIST.sha256 sidecar missing"); sys.exit(1)
m_sc = re.fullmatch(r"([0-9a-f]{64})  PHRASE_ALLOWLIST\.txt\n?",
                    open(sc, encoding="utf-8").read())
if not m_sc:
    print("FAIL: PHRASE_ALLOWLIST.sha256 malformed (need exactly '<sha256>  PHRASE_ALLOWLIST.txt')")
    sys.exit(1)
actual = hashlib.sha256(open(ap, "rb").read()).hexdigest()
if actual != m_sc.group(1):
    print("FAIL: PHRASE_ALLOWLIST.txt was modified (%s != %s)" % (actual, m_sc.group(1)))
    sys.exit(1)
allow = {norm(l.strip()) for l in open(ap, encoding="utf-8") if l.strip() and not l.startswith("#")}

for t in targets:
    if not os.path.exists(t):
        print("FAIL: target missing:", t); sys.exit(1)
    raw = open(t, encoding="utf-8").read()
    # (0) token 筛子：目标内出现任一上游 harness 标识符即失败（原始文本，非规范化）
    for tok in TOKENS:
        if tok in raw:
            print("FAIL token sieve: %s contains upstream harness token %r" % (t, tok))
            bad = 1
    a = norm(raw)
    tg = {g for g in grams(a) if g not in allow}
    for srcp in sources:
        b = norm(open(srcp, encoding="utf-8", errors="replace").read())
        # (1) 长子串：>= 40 字符规范化公共子串
        for m in difflib.SequenceMatcher(None, a, b, autojunk=False).get_matching_blocks():
            if m.size >= 40:
                print("FAIL overlap %d chars: %s <-> %s :: %r"
                      % (m.size, t, os.path.basename(srcp), a[m.a:m.a + m.size]))
                bad = 1
        # (2) 8-token 滑窗：任何非白名单命中即失败（阈值 0，不是比率）
        shared = tg & grams(b)
        if shared:
            print("FAIL ngram %d shared 8-token windows: %s <-> %s :: %r"
                  % (len(shared), t, os.path.basename(srcp), sorted(shared)[0]))
            bad = 1
sys.exit(bad)          # ← 两种检测共用的唯一退出点
PY