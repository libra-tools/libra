#!/bin/sh
# tests/compat-ledger/t4/PRECHECK_ALL.sh —— 端到端预检唯一入口（CT3-04 交付，无占位符）
set -eu
cd "$(dirname "$0")/../../.."                  # 定位仓库根（脚本位于 tests/compat-ledger/t4/）
[ -f COMPATIBILITY.md ] && [ -d .libra ] || { echo "FAIL: not at repo root" >&2; exit 2; }
# R34 P0：DRAFT 无条件固定为提交路径，不接受环境变量覆盖——否则替代草稿可让 sidecar
# 绑定到未提交文件、真正提交的 DRAFT.rs.txt 完全未受检
DRAFT=tests/compat-ledger/t4/DRAFT.rs.txt; export DRAFT
# **两阶段（R42 P0）**：`DEFECTS.md` 由人工依据首轮结果撰写，故收集与校验不能在同一次调用里
# 完成（原单阶段入口在首次合法执行时必然撞上尚不存在的 `DEFECTS.md`）：
#   PRECHECK_ALL_PHASE=collect  —— 跑预检、write-once 落 canonical 结果，**不跑** GATES；
#                                   随后由人工撰写 `DEFECTS.md`（schema 见 AC）。
#   PRECHECK_ALL_PHASE=validate —— 复跑预检（verify，不改写 canonical）+ GATES 三段 + 净室
#                                   + `DEFECTS.md` token 筛子。改判后的每一轮都跑本阶段。
PHASE=${PRECHECK_ALL_PHASE:?set PRECHECK_ALL_PHASE=collect|validate}
RUN_DIR=$(mktemp -d); export RUN_DIR
# **ER-11（R64 P1）**：本脚本自建 `RUN_DIR`，因此必须自己负责收尾——成功即整目录删除，失败
# 保留并打印路径（保留态是已知例外，须在任务记录登记）。任何路径都先断言「无未脱敏原始日志
# 残留」，`find` 独立落盘并显式查退出码（不得用 `2>/dev/null` + 命令替换吞掉它）。
pa_cleanup() {
  r=$?; [ -n "${1:-}" ] && r=$1
  trap - EXIT INT TERM
  # **R69 P1（ER-11，闭集而非 denylist）**：denylist 必然漏项（`live_list.txt`、`live_ids.out`、
  # `list.txt` 等原始载荷此前完全不在名单里），且命中后只置失败、不删除。改为**允许保留文件
  # 的闭集**：先可靠删除闭集之外的一切，再验证剩余集合，最后只打印 basename。
  # **R70 P1：闭集必须落在「叶文件」而不是目录上**——把 `out/` 整个白名单化，等于放任
  # `out/<fn>.log` 这类原始日志存活；`.[!.]*` 又漏掉 `..name`。改为 `find` 递归枚举叶文件，
  # 以**相对路径闭集**判定，删除与复验都递归。
  KEEP='results.tsv actuals.tsv assertions.lock frozen.txt active_pairs.txt reclass_pairs.txt'
  # **R71 P1（自伤回归）**：清单文件绝不能落在被扫描的目录里——shell 在 `find` 执行**之前**
  # 就创建了 `$RUN_DIR/.left`，`find` 随即枚举到它，而它不在 `KEEP` 中，于是**每一次成功运行**
  # 都会被判 `r=2`。改为把两份清单放在 `RUN_DIR` **之外**的独立临时文件里。
  # R72 P1：`mktemp` 失败时绝不能带着空路径继续——否则既不清除也无法复验，却仍会打印
  # 「raw payloads purged」。改为 fail-closed：直接删除整个已验证归属的目录并以 2 退出。
  if ! _all=$(mktemp) || ! _left=$(mktemp); then
    rm -f "${_all:-}" 2>/dev/null
    rm -rf "$RUN_DIR"
    echo "FATAL: cannot create the cleanup manifests; RUN_DIR removed wholesale" >&2
    exit 2
  fi
  find "$RUN_DIR" -mindepth 1 \( -type f -o -type l \) -print > "$_all" 2>/dev/null \
    || { echo "FATAL: cannot enumerate RUN_DIR" >&2; r=2; }
  while IFS= read -r f; do
    rel=${f#"$RUN_DIR"/}
    case " $KEEP " in *" $rel "*) continue ;; esac
    rm -f "$f" || { echo "FATAL: cannot purge $rel from RUN_DIR" >&2; r=2; }
  done < "$_all"
  find "$RUN_DIR" -mindepth 1 \( -type f -o -type l \) -print > "$_left" 2>/dev/null \
    || { echo "FATAL: cannot re-enumerate RUN_DIR" >&2; r=2; }
  while IFS= read -r f; do
    rel=${f#"$RUN_DIR"/}
    case " $KEEP " in *" $rel "*) : ;; *) echo "FATAL: $rel survived the purge" >&2; r=2 ;; esac
  done < "$_left"
  rm -f "$_all" "$_left"
  find "$RUN_DIR" -mindepth 1 -type d -empty -delete 2>/dev/null || :
  if [ "$r" -eq 0 ]; then rm -rf "$RUN_DIR"
  else echo "NOTE: keeping $(basename "$RUN_DIR")/ for inspection (redacted artefacts only)" >&2; fi
  exit "$r"
}
trap 'pa_cleanup' EXIT; trap 'pa_cleanup 130' INT; trap 'pa_cleanup 143' TERM
case "$PHASE" in
  collect)
    [ ! -f tests/compat-ledger/t4/DEFECTS.md ] \
      || { echo "FAIL: DEFECTS.md exists — collect is the first phase only; use validate" >&2; exit 2; }
    if [ -f tests/compat-ledger/t4/PRECHECK_RESULTS.tsv ] && [ "${PRECHECK_RECOLLECT:-0}" = "1" ]; then
      rst=$(libra status --porcelain=v1 -- tests/compat-ledger/t4/PRECHECK_RESULTS.tsv) \
        || { echo "FAIL: libra status failed" >&2; exit 2; }
      case "$rst" in
        ?"?"*|"??"*) rm -f tests/compat-ledger/t4/PRECHECK_RESULTS.tsv ;;
        *) echo "FAIL: canonical results are already committed/staged — recollect is not allowed" >&2; exit 2 ;;
      esac
    fi
    PRECHECK_MODE=record; export PRECHECK_MODE
    sh tests/compat-ledger/t4/PRECHECK.sh
    # R44：断言锁与草稿由 CT3-06 在 collect **之前**提交（不可变锚点），本阶段不再安装任何锁；
    # 半写入恢复：collect 只写 canonical 结果一件产物，若中途失败且 DEFECTS.md 尚未撰写，
    # 用 `PRECHECK_RECOLLECT=1` 显式重来（删除未提交的 canonical 结果后重跑）
    # collect 的产物 `results.tsv` 是人工撰写 `DEFECTS.md` 的输入，必须**留存**——因此把它
    # 复制到调用方指定的、已登记为脱敏产物的归档位置，再让 `pa_cleanup` 删除 RUN_DIR。
    : "${PRECHECK_ARCHIVE:?export PRECHECK_ARCHIVE=<dir> to receive the redacted collect artefacts}"
    mkdir -p "$PRECHECK_ARCHIVE" || { echo "FAIL: cannot create $PRECHECK_ARCHIVE" >&2; exit 2; }
    cp "$RUN_DIR/results.tsv" "$PRECHECK_ARCHIVE/results.tsv" \
      || { echo "FAIL: cannot archive results.tsv" >&2; exit 2; }
    [ ! -f "$RUN_DIR/actuals.tsv" ] || cp "$RUN_DIR/actuals.tsv" "$PRECHECK_ARCHIVE/actuals.tsv" \
      || { echo "FAIL: cannot archive actuals.tsv" >&2; exit 2; }
    # R71 P1（ER-11）：证据输出只给 basename 与内容摘要，绝对路径不进任务记录
    _rsha=$(shasum -a 256 "$PRECHECK_ARCHIVE/results.tsv") || { echo "FAIL: shasum results"; exit 2; }
    echo "COLLECT OK — author tests/compat-ledger/t4/DEFECTS.md from $(basename "$PRECHECK_ARCHIVE")/results.tsv (sha256=${_rsha%% *}), then run PRECHECK_ALL_PHASE=validate"
    exit 0 ;;
  validate)
    [ -s tests/compat-ledger/t4/DEFECTS.md ] \
      || { echo "FAIL: DEFECTS.md missing or empty — run collect first and author it" >&2; exit 2; }
    PRECHECK_MODE=verify; export PRECHECK_MODE
    ;;
  *) echo "FAIL: PRECHECK_ALL_PHASE must be collect or validate" >&2; exit 2 ;;
esac
sh tests/compat-ledger/t4/PRECHECK.sh          # verify：不改写 canonical PRECHECK_RESULTS.tsv
sh tests/compat-ledger/t4/GATES.sh             # 三方门 + 结果绑定门 + DEFECTS 结构门（同一 RUN_DIR）
# R51 P1：净室必须查**活跃投影**（子进程里的 DRAFT 赋值不会传回父 shell）——投影由
# PRECHECK.sh 写入同一 RUN_DIR；断言其存在后再检查
[ -s "$RUN_DIR/draft.active.rs" ] || { echo "FAIL: active projection missing (run PRECHECK.sh first)" >&2; exit 2; }
# R56 P1：canonical 入口显式清除白名单/sidecar 覆盖（覆盖只允许出现在篡改负例的子进程内联赋值中）
unset PHRASE_ALLOWLIST PHRASE_SIDECAR
# **R79 P1**：净室脚本与 EXPECTED 均走 PRECHECK 已建立的 GC-15 快照（PRECHECK 必须把
# EXPECTED.txt 纳入 snapshot_frozen 清单）。禁止再执行工作树 CLEANROOM.sh。
A_CLEANROOM="${A_CLEANROOM:-$RUN_DIR/snap/tests/compat-ledger/t4/CLEANROOM.sh}"
A_EXPECTED="${A_EXPECTED:-$RUN_DIR/snap/tests/compat-ledger/t4/EXPECTED.txt}"
[ -s "$A_CLEANROOM" ] || { echo "FAIL: snapshotted CLEANROOM.sh missing" >&2; exit 2; }
[ -s "$A_EXPECTED" ] || { echo "FAIL: snapshotted EXPECTED.txt missing — PRECHECK must snapshot it" >&2; exit 2; }
TARGET="$RUN_DIR/draft.active.rs" EXPECTED_SNAP="$A_EXPECTED" sh "$A_CLEANROOM"
# R34 P0：DEFECTS.md 的 token 筛子必须真实执行（六 token，与账本筛子同清单；0/1/>1 分流）
if rg -n "test_expect_success|test_expect_failure|test_cmp|test_when_finished|TEST_DIRECTORY|test-lib\.sh" tests/compat-ledger/t4/DEFECTS.md; then
  echo "FAIL: upstream harness text in DEFECTS.md" >&2; exit 1
else
  rc=$?
  if [ "$rc" -ne 1 ]; then echo "ERROR: rg failed with exit $rc" >&2; exit "$rc"; fi
fi
echo "PRECHECK_ALL validate OK (RUN_DIR=$RUN_DIR)"
