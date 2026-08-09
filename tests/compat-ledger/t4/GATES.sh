#!/bin/sh
# tests/compat-ledger/t4/GATES.sh —— CT3-04 的三段门，依序执行（内容 = 卡内三个 fenced block）。
# 第一段：三方 ID 集合；第二段：结果绑定门；第三段：DEFECTS.md 结构门。
# 三段各自 `set -eu`，在同一 RUN_DIR 会话内由 PRECHECK_ALL.sh 于 PRECHECK.sh 之后调用。
# 每段包在自己的子 shell 里（各段的 `set -eu`、变量与 `exit` 互不影响）；顶层这行 `set -eu`
# 不可省——没有它，第一段或第二段以 1 退出只是子 shell 失败，脚本会继续往下跑，最终退出码取自
# 最后一段，前两道门等于形同虚设。
set -eu

# ============ 第一段：三方 ID 集合精确相等且非空 ============
(
set -eu
export LIBRA_SKIP_WEB_BUILD=1     # 本门与 web 无关；干净 target 下可避免触发 Next 构建
# RUN_DIR 由同一调用方在运行 PRECHECK.sh **之前**创建并 export（R26/R27 P0：脚本与各门
# 消费同一 RUN_DIR 连续执行，未设置即拒绝运行——防止旧中间产物冒充本轮枚举结果）
: "${RUN_DIR:?export the same caller-owned RUN_DIR used by PRECHECK.sh in this session}"
. ./.env.test                     # R37 P1：本门内的 cargo 调用（ledger_dump_direct_ids）前加载测试环境
# 期望集 = **活跃集**（R42：由 PRECHECK.sh 依「活跃集派生规范」写入同一 RUN_DIR 的
# active_pairs.txt；冻结基 DIRECT_SNAPSHOT.tsv 只读，绝不随改判收缩）
[ -s "$RUN_DIR"/active_pairs.txt ] \
  || { echo "FAIL: active_pairs.txt missing — run PRECHECK.sh in this RUN_DIR first"; exit 1; }
awk -F'\t' 'NF!=2 || $1=="" || $2=="" {exit 1} {print $2}' "$RUN_DIR"/active_pairs.txt > "$RUN_DIR"/a.raw \
  || { echo "FAIL: active_pairs.txt malformed"; exit 1; }
[ -s "$RUN_DIR"/a.raw ] || { echo "FAIL: active set is empty (a wave with zero live direct rows must be recorded explicitly)"; exit 1; }
LC_ALL=C sort -u "$RUN_DIR"/a.raw > "$RUN_DIR"/a.txt
# 覆盖门：活跃集的 scenario_id 必须与**当前**账本 direct 行的 id 集合精确相等
awk -F'\t' '{print $1}' "$RUN_DIR"/active_pairs.txt > "$RUN_DIR"/sid.raw || { echo "FAIL: cut scenario ids"; exit 1; }
LC_ALL=C sort -u "$RUN_DIR"/sid.raw > "$RUN_DIR"/sid.txt
# 两列各自唯一，且行数 == 唯一数（双射）
n_rows=$(wc -l < "$RUN_DIR"/active_pairs.txt)
[ "$n_rows" -eq "$(wc -l < "$RUN_DIR"/sid.txt)" ] || { echo "FAIL: duplicate scenario_id"; exit 1; }
[ "$n_rows" -eq "$(wc -l < "$RUN_DIR"/a.txt)" ] || { echo "FAIL: duplicate test_fn"; exit 1; }
# 账本 direct 行的 id 集合（与守卫同一 TOML 解析口径：id 行紧邻其 category 行所属的 [[scenario]] 块）
cargo test --test compat_ledger_schema ledger_dump_direct_ids -- --exact --nocapture > "$RUN_DIR"/direct_ids.raw \
  || { echo "FAIL: cannot dump direct ids"; exit 1; }
sed -n 's/^DIRECT_ID //p' "$RUN_DIR"/direct_ids.raw > "$RUN_DIR"/direct_ids.rows || { echo "FAIL: sed DIRECT_ID"; exit 1; }
cut -f1 "$RUN_DIR"/direct_ids.rows > "$RUN_DIR"/direct_ids.pre || { echo "FAIL: cut DIRECT_ID col1"; exit 1; }
LC_ALL=C sort -u "$RUN_DIR"/direct_ids.pre > "$RUN_DIR"/direct_ids.txt
diff "$RUN_DIR"/direct_ids.txt "$RUN_DIR"/sid.txt \
  || { echo "FAIL: active set does not cover exactly the current ledger direct rows"; exit 1; }
# 快照分割门（R37 P0）：冻结快照 = 活跃 direct ∪ DEFECTS 的 reclassify 行（精确分割且两侧不相交）
awk -F'\t' 'NR>1 && $6=="reclassify" {print $2}' tests/compat-ledger/t4/DEFECTS.md > "$RUN_DIR"/rc_sids.raw \
  || { echo "FAIL: extract reclassify sids"; exit 1; }
LC_ALL=C sort -u "$RUN_DIR"/rc_sids.raw > "$RUN_DIR"/rc_sids.txt
ovl=$(comm -12 "$RUN_DIR"/direct_ids.txt "$RUN_DIR"/rc_sids.txt)
[ -z "$ovl" ] || { echo "FAIL: scenario both live-direct and reclassified:"; printf '%s\n' "$ovl"; exit 1; }
cat "$RUN_DIR"/direct_ids.txt "$RUN_DIR"/rc_sids.txt > "$RUN_DIR"/part.raw
LC_ALL=C sort -u "$RUN_DIR"/part.raw > "$RUN_DIR"/part.txt
# R39 P1：**完整 pair 分割**——活跃 (sid,fn) ∪ reclassify (sid,fn) 排序后与快照前两列逐行精确
# 相等（活跃行也不得偷换 test_fn；两侧不相交已由上方 comm -12 断言）
cp "$RUN_DIR"/active_pairs.txt "$RUN_DIR"/act_pairs.raw \
  || { echo "FAIL: reuse derived active pairs"; exit 1; }
awk -F'\t' 'NR>1 && $6=="reclassify" {print $2 "\t" $1}' tests/compat-ledger/t4/DEFECTS.md > "$RUN_DIR"/rc_pairs.raw \
  || { echo "FAIL: extract reclassify pairs"; exit 1; }
cat "$RUN_DIR"/act_pairs.raw "$RUN_DIR"/rc_pairs.raw > "$RUN_DIR"/all_pairs.raw || { echo "FAIL: cat pairs"; exit 1; }
LC_ALL=C sort "$RUN_DIR"/all_pairs.raw > "$RUN_DIR"/all_pairs.txt
cut -f1,2 tests/compat-ledger/t4/DIRECT_SNAPSHOT.tsv > "$RUN_DIR"/snap12.raw || { echo "FAIL: cut snapshot"; exit 1; }
LC_ALL=C sort "$RUN_DIR"/snap12.raw > "$RUN_DIR"/snap12.txt
diff "$RUN_DIR"/snap12.txt "$RUN_DIR"/all_pairs.txt \
  || { echo "FAIL: frozen snapshot pairs != active pairs + reclassify pairs"; exit 1; }
# 枚举集 = 本轮**实际执行的场景函数**（守卫已在 PRECHECK 内被拆出，--list 全集 =
# 场景 + 固定守卫已单独断言）；不做 -u 去重比较——先断言行级无重复，再逐行 diff
awk -F'\t' '{print $1}' "$RUN_DIR"/results.tsv > "$RUN_DIR"/exec_fn.raw \
  || { echo "FAIL: parse results for enumerated set"; exit 1; }
LC_ALL=C sort "$RUN_DIR"/exec_fn.raw > "$RUN_DIR"/b.txt
# R38 P0：集合比较只取非 reclassify 行（reclassify 行的场景已退出活跃集，无本轮执行）；
# test_fn/scenario_id 唯一性另以**全行**独立断言
awk -F'\t' 'NR>1 && $1!="" {print $1}' tests/compat-ledger/t4/DEFECTS.md > "$RUN_DIR"/c_all.raw \
  || { echo "FAIL: awk DEFECTS.md (all rows)"; exit 1; }
LC_ALL=C sort -u "$RUN_DIR"/c_all.raw > "$RUN_DIR"/c_all.uniq
[ "$(wc -l < "$RUN_DIR"/c_all.raw)" -eq "$(wc -l < "$RUN_DIR"/c_all.uniq)" ] \
  || { echo "FAIL: duplicate test_fn in DEFECTS.md"; exit 1; }
awk -F'\t' 'NR>1 && $2!="" {print $2}' tests/compat-ledger/t4/DEFECTS.md > "$RUN_DIR"/csid.raw \
  || { echo "FAIL: awk DEFECTS.md sids"; exit 1; }
LC_ALL=C sort -u "$RUN_DIR"/csid.raw > "$RUN_DIR"/csid.uniq
[ "$(wc -l < "$RUN_DIR"/csid.raw)" -eq "$(wc -l < "$RUN_DIR"/csid.uniq)" ] \
  || { echo "FAIL: duplicate scenario_id in DEFECTS.md"; exit 1; }
awk -F'\t' 'NR>1 && $1!="" && $6!="reclassify" {print $1}' tests/compat-ledger/t4/DEFECTS.md > "$RUN_DIR"/c.raw \
  || { echo "FAIL: awk DEFECTS.md (non-reclassify)"; exit 1; }
LC_ALL=C sort "$RUN_DIR"/c.raw > "$RUN_DIR"/c.txt
diff "$RUN_DIR"/a.txt "$RUN_DIR"/b.txt || { echo "FAIL: ledger vs enumerated test set"; exit 1; }
diff "$RUN_DIR"/b.txt "$RUN_DIR"/c.txt || { echo "FAIL: enumerated vs DEFECTS.md test set"; exit 1; }
echo OK
)

# ============ 第二段：结果绑定门 ============
(
set -eu
: "${RUN_DIR:?export the same caller-owned RUN_DIR used by PRECHECK.sh in this session}"
# ⓪ 结果文件 schema 与全集门（R25 P0：空文件/未知状态/缺用例都不得静默过门）：
#    恰两列、状态闭集 {pass, fail}、非空；canonical 是**首轮 collect 的冻结全集**（write-once），
#    故其 test_fn 全集 == FROZEN（R43 P0：此前误按 ACTIVE 校验，任一 reclassify 必然失败）
RES=tests/compat-ledger/t4/PRECHECK_RESULTS.tsv
[ -s "$RES" ] || { echo "FAIL: PRECHECK_RESULTS.tsv missing or empty"; exit 1; }
awk -F'\t' 'NF!=2 || $1=="" || ($2!="pass" && $2!="fail") {bad=1} END{exit bad?1:0}' "$RES" \
  || { echo "FAIL: PRECHECK_RESULTS.tsv malformed (need <test_fn>\t<pass|fail>)"; exit 1; }
awk -F'\t' '{print $1}' "$RES" > "$RUN_DIR"/r_fn.raw || { echo "FAIL: parse results"; exit 1; }
awk -F'\t' '{print $2}' "$RUN_DIR"/frozen.txt > "$RUN_DIR"/frozen_fn.raw \
  || { echo "FAIL: parse frozen pairs"; exit 1; }
awk -F'\t' '{print $2}' "$RUN_DIR"/active_pairs.txt > "$RUN_DIR"/exp_fn.raw \
  || { echo "FAIL: parse active pairs"; exit 1; }
LC_ALL=C sort "$RUN_DIR"/r_fn.raw > "$RUN_DIR"/r_fn.sorted
LC_ALL=C sort "$RUN_DIR"/frozen_fn.raw > "$RUN_DIR"/frozen_fn.sorted
LC_ALL=C sort "$RUN_DIR"/exp_fn.raw > "$RUN_DIR"/exp_fn.sorted
diff "$RUN_DIR"/frozen_fn.sorted "$RUN_DIR"/r_fn.sorted \
  || { echo "FAIL: canonical results test_fn set != frozen set"; exit 1; }
# canonical 的**活跃投影**：按 ACTIVE 过滤 canonical 行，必须与本轮 results.tsv 逐行相等
LC_ALL=C sort "$RES" > "$RUN_DIR"/res.all.sorted
join -t "$(printf '\t')" "$RUN_DIR"/exp_fn.sorted "$RUN_DIR"/res.all.sorted > "$RUN_DIR"/res.active.txt \
  || { echo "FAIL: project canonical onto active set"; exit 1; }
LC_ALL=C sort "$RUN_DIR"/results.tsv > "$RUN_DIR"/run.sorted
diff "$RUN_DIR"/res.active.txt "$RUN_DIR"/run.sorted \
  || { echo "FAIL: canonical(active projection) != this run's results"; exit 1; }
# ① 行级唯一性前置：两份文件的 test_fn 均不得重复（重复即失败，而不是被 sort -u 吸收）
LC_ALL=C sort -u "$RUN_DIR"/r_fn.raw > "$RUN_DIR"/r_fn.uniq
[ "$(wc -l < "$RUN_DIR"/r_fn.sorted)" -eq "$(wc -l < "$RUN_DIR"/r_fn.uniq)" ] \
  || { echo "FAIL: duplicate test_fn in PRECHECK_RESULTS.tsv"; exit 1; }
awk -F'\t' 'NR>1 && $1!="" {print $1}' tests/compat-ledger/t4/DEFECTS.md > "$RUN_DIR"/d_fn.raw \
  || { echo "FAIL: parse DEFECTS.md"; exit 1; }
LC_ALL=C sort -u "$RUN_DIR"/d_fn.raw > "$RUN_DIR"/d_fn.uniq
[ "$(wc -l < "$RUN_DIR"/d_fn.raw)" -eq "$(wc -l < "$RUN_DIR"/d_fn.uniq)" ] \
  || { echo "FAIL: duplicate test_fn in DEFECTS.md"; exit 1; }
# ② 本轮实测 fail ↔ DEFECTS 的 defect 行逐行对应（R37：reclassify 行的场景已退出活跃集、
#    无本轮 RES 行——其对应关系由三方门的快照分割门承担，此处只比 defect）
# R44 P0：失败集必须取自**本轮**结果（canonical 是冻结全集，含改判行的原始 fail，
# 与只含 defect 行的 DEFECTS 比较必然不等）
awk -F'\t' '$2=="fail" {print $1}' "$RUN_DIR"/results.tsv > "$RUN_DIR"/f.raw \
  || { echo "FAIL: parse this run's results"; exit 1; }
LC_ALL=C sort "$RUN_DIR"/f.raw > "$RUN_DIR"/fails.txt
awk -F'\t' 'NR>1 && $6=="defect" {print $1}' tests/compat-ledger/t4/DEFECTS.md > "$RUN_DIR"/d.raw \
  || { echo "FAIL: parse DEFECTS.md"; exit 1; }
LC_ALL=C sort "$RUN_DIR"/d.raw > "$RUN_DIR"/defects.txt
diff "$RUN_DIR"/fails.txt "$RUN_DIR"/defects.txt \
  || { echo "FAIL: failing cases and DEFECTS defect rows differ"; exit 1; }
# reclassify 行（R43 P0 订正；R54 P1 加固）：其 test_fn 必须在 **canonical** 中存在且原始结果
# 为 `fail`，同时**不得**出现在本轮 results.tsv（其测试已随改判从草稿移除）。canonical 只是
# 记录；「原始 fail」本身由 `PRECHECK.sh` 的改判行复验段**每轮重新证明**（以冻结全量草稿逐个
# `--exact` 重跑并断言仍为单例失败），故等行数改写 canonical 无法伪造改判场景的原始结果。
awk -F'\t' 'NR>1 && $6=="reclassify" {print $1}' tests/compat-ledger/t4/DEFECTS.md > "$RUN_DIR"/rcfn.raw \
  || { echo "FAIL: extract reclassify fns"; exit 1; }
while read -r fn; do
  [ -z "$fn" ] && continue
  command grep -qxF "$(printf '%s\tfail' "$fn")" "$RES" \
    || { echo "FAIL: reclassified $fn has no original 'fail' row in canonical results"; exit 1; }
  if command grep -nF "$(printf '%s\t' "$fn")" "$RUN_DIR"/results.tsv; then
    echo "FAIL: reclassified scenario still executed in this run: $fn"; exit 1
  else
    rc=$?; [ "$rc" -eq 1 ] || { echo "ERROR: grep failed with $rc"; exit "$rc"; }
  fi
done < "$RUN_DIR"/rcfn.raw
# ②b result 列与判定列的机械一致性（R26 P1）：DEFECTS 的 (test_fn, result) 与 RES 逐行相等；
#    result=pass ⇔ 判定=pass 且 defect_id=-；result=fail ⇔ 判定 ∈ {defect, reclassify}
awk -F'\t' 'NR>1 && $6!="reclassify" {print $1 "\t" $5}' tests/compat-ledger/t4/DEFECTS.md > "$RUN_DIR"/dres.raw \
  || { echo "FAIL: extract DEFECTS result column (non-reclassify rows)"; exit 1; }
LC_ALL=C sort "$RUN_DIR"/dres.raw > "$RUN_DIR"/dres.txt
# 与 canonical 的**活跃投影**比较（R43 P0：canonical 是冻结全集，含已改判行）
diff "$RUN_DIR"/res.active.txt "$RUN_DIR"/dres.txt \
  || { echo "FAIL: DEFECTS (non-reclassify) result column differs from canonical(active)"; exit 1; }
# **R67 P1：`git_expected`（第 3 列）也必须逐行重放**——此前只校验三段格式，
# `grit-doc-anchor|missing:999|<64个0>` 即可通过。重放器与 `PROVENANCE.md` 完全同一套
# （同一份 `PROBES.allow`、同一 pin、同一三种 `source_kind`），实现落在 `GATES.sh` 的
# 「来源重放」段，本卡与 CT3-02 共用；`PROBES.allow` 由前置卡 **CT3-06** 冻结交付（R67：
# 上移自 CT3-02——执行顺序上 CT3-02 在本卡之后，本卡此前根本拿不到白名单）。
awk -F'\t' 'NR>1 && $3!="" {print $3}' tests/compat-ledger/t4/DEFECTS.md > "$RUN_DIR"/def_sources.txt \
  || { echo "FAIL: extract git_expected column"; exit 1; }
[ -s "$RUN_DIR"/def_sources.txt ] || { echo "FAIL: no git_expected rows to replay"; exit 1; }
SOURCES_FILE="$RUN_DIR/def_sources.txt" GD="$RUN_DIR" \
  sh tests/compat-ledger/t4/REPLAY_SOURCES.sh \
  || { echo "FAIL: git_expected replay failed"; exit 1; }
# R48 P1 / R54 P1：**全部** fail 行（含 `reclassify`）的 `libra_actual`（第 4 列）必须与本轮
# 捕获的实际输出摘要逐行一致——改判行的摘要来自 `PRECHECK.sh` 的「改判行复验」段（以冻结全量
# 草稿重跑该函数），因此不再存在「≤200 字符任意文本即可」的空档。
awk -F'\t' 'NR>1 && $5=="fail" {print $1 "\t" $4}' tests/compat-ledger/t4/DEFECTS.md \
  > "$RUN_DIR"/dact.raw || { echo "FAIL: extract DEFECTS actual column"; exit 1; }
if [ -s "$RUN_DIR"/dact.raw ]; then
  # fail-closed：存在 fail 行却没有本轮捕获文件 = 证据缺失，不得跳过本门
  [ -f "$RUN_DIR"/actuals.tsv ] \
    || { echo "FAIL: DEFECTS has fail rows but this run captured no actuals"; exit 1; }
  LC_ALL=C sort "$RUN_DIR"/dact.raw > "$RUN_DIR"/dact.txt
  LC_ALL=C sort "$RUN_DIR"/actuals.tsv > "$RUN_DIR"/act.txt
  while IFS= read -r line; do   # 自审 P2：IFS= 关闭字段分割 = 不剥离首尾空白
    command grep -qxF "$line" "$RUN_DIR"/act.txt \
      || { echo "FAIL: DEFECTS libra_actual does not match captured output: $line"; exit 1; }
  done < "$RUN_DIR"/dact.txt
fi
# reclassify 行的 result 必须冻结为 `fail`
awk -F'\t' 'NR>1 && $6=="reclassify" && $5!="fail" {bad=1} END{exit bad?1:0}' tests/compat-ledger/t4/DEFECTS.md \
  || { echo "FAIL: a reclassify row does not carry the original result=fail"; exit 1; }
awk -F'\t' 'NR>1 && (($5=="pass" && ($6!="pass" || $7!="-")) || ($5=="fail" && $6!="defect" && $6!="reclassify")) {bad=1} END{exit bad?1:0}' tests/compat-ledger/t4/DEFECTS.md \
  || { echo "FAIL: result/judgment/defect_id combination violates closed-set rules"; exit 1; }
# ③ (test_fn, scenario_id) 配对与 defect_id 三元组精确对应（R25 P0：防交换 scenario_id）：
#    DEFECTS.md 的非 reclassify 行 (test_fn, scenario_id) 对必须与**活跃**双射逐行一致；
#    判定=defect 的行提取 (test_fn, scenario_id, defect_id)，defect_id 格式合法且无重复，
#    其集合与计划中已实例化的 `### Task CTF-P` 卡 ID 集合**精确相等**（不是只比数量）
awk -F'\t' 'NR>1 && $1!="" && $6!="reclassify" {print $1 "\t" $2}' tests/compat-ledger/t4/DEFECTS.md > "$RUN_DIR"/pair_d.raw \
  || { echo "FAIL: extract DEFECTS pairs (non-reclassify rows)"; exit 1; }
LC_ALL=C sort "$RUN_DIR"/pair_d.raw > "$RUN_DIR"/pair_d.txt
awk -F'\t' '{print $2 "\t" $1}' "$RUN_DIR"/active_pairs.txt > "$RUN_DIR"/pair_s.raw \
  || { echo "FAIL: extract SCENARIO pairs"; exit 1; }
LC_ALL=C sort "$RUN_DIR"/pair_s.raw > "$RUN_DIR"/pair_s.txt
diff "$RUN_DIR"/pair_s.txt "$RUN_DIR"/pair_d.txt \
  || { echo "FAIL: (test_fn, scenario_id) pairs differ from the derived active bijection"; exit 1; }
awk -F'\t' 'NR>1 && $6=="defect" {print $1 "\t" $2 "\t" $7}' tests/compat-ledger/t4/DEFECTS.md > "$RUN_DIR"/trip.raw \
  || { echo "FAIL: parse defect triples"; exit 1; }
awk -F'\t' 'NF!=3 || $1=="" || $2=="" || $3 !~ /^CTF-P[0-9][0-9]*$/ {bad=1} END{exit bad?1:0}' "$RUN_DIR"/trip.raw \
  || { echo "FAIL: defect row with malformed triple or defect_id"; exit 1; }
awk -F'\t' '{print $3}' "$RUN_DIR"/trip.raw > "$RUN_DIR"/did.raw
LC_ALL=C sort "$RUN_DIR"/did.raw > "$RUN_DIR"/did.sorted
LC_ALL=C sort -u "$RUN_DIR"/did.raw > "$RUN_DIR"/did.uniq
diff "$RUN_DIR"/did.sorted "$RUN_DIR"/did.uniq || { echo "FAIL: duplicate defect_id"; exit 1; }
sed -n 's/^### Task \(CTF-P[0-9][0-9]*\):.*$/\1/p' docs/development/plan/plan-20260729.md > "$RUN_DIR"/cards.raw
LC_ALL=C sort "$RUN_DIR"/cards.raw > "$RUN_DIR"/cards.txt
diff "$RUN_DIR"/did.sorted "$RUN_DIR"/cards.txt \
  || { echo "FAIL: defect_id set != instantiated CTF-P card ID set"; exit 1; }
# ④ 持久化真实性（R25/R26 P0；R43 改为**活跃投影**比较——canonical 是首轮冻结全集，含已改判
#    行，与本轮结果直接 diff 必然失败）：上文 ⓪ 的 `res.active.txt` vs `run.sorted` 即本门，
#    此处再断言 canonical 自 collect 起未被改写（行数恒 == FROZEN 行数）
[ "$(wc -l < "$RES")" -eq "$(wc -l < "$RUN_DIR"/frozen.txt)" ] \
  || { echo "FAIL: canonical PRECHECK_RESULTS.tsv row count != frozen set (was rewritten?)"; exit 1; }
# ⑤ 草稿绑定（R28 P0）：本轮实际使用的草稿摘要必须等于已提交的 sidecar——canonical 结果
#    由此绑定到提交草稿，任意替换草稿都会在此失败
diff "$RUN_DIR/draft.sha256" tests/compat-ledger/t4/DRAFT.sha256 \
  || { echo "FAIL: draft used by this run differs from committed DRAFT.sha256"; exit 1; }
# ⑥ reclassify 证据门（R35 P1；R39 改为与**冻结快照第三列**精确配对——与改判后的活跃账本比较
#    可被补充提交同时新造证据，冻结列才证明证据先于预检存在）
awk -F'\t' 'NR>1 && $6=="reclassify" {print $2 "\t" $1 "\t" $8}' tests/compat-ledger/t4/DEFECTS.md > "$RUN_DIR"/reclass.raw \
  || { echo "FAIL: extract reclassify rows"; exit 1; }
LC_ALL=C sort "$RUN_DIR"/reclass.raw > "$RUN_DIR"/reclass.txt
# 每个 reclassify (sid, fn, evidence) 三元组必须逐字等于冻结快照中的同 sid 行
while IFS= read -r line; do   # 自审 P2：IFS= 关闭字段分割 = 不剥离首尾空白
  [ -z "$line" ] && continue
  command grep -qxF "$line" tests/compat-ledger/t4/DIRECT_SNAPSHOT.tsv \
    || { echo "FAIL: reclassify (sid, fn, evidence) not frozen in DIRECT_SNAPSHOT.tsv: $line"; exit 1; }
done < "$RUN_DIR"/reclass.txt
echo OK
)

# ============ 第三段：DEFECTS.md 结构门 ============
(
set -eu
D=tests/compat-ledger/t4/DEFECTS.md
[ -s "$D" ] || { echo "FAIL: DEFECTS.md missing or empty"; exit 1; }
awk -F'\t' '
  NR==1 { if (NF!=8 || $1!="test_fn" || $2!="scenario_id" || $3!="git_expected" || $4!="libra_actual" || $5!="result" || $6!="判定" || $7!="defect_id" || $8!="reclassify_evidence") bad=1; next }
  { if (NF!=8 || $1=="" || $2=="" || $3=="" || ($5!="pass" && $5!="fail") \
      || length($4)>200 || ($5=="fail" && ($4=="" || $4=="-")) \
      || ($6!="pass" && $6!="defect" && $6!="reclassify") \
      || ($5=="pass" && ($6!="pass" || $7!="-")) \
      || ($5=="fail" && $6!="defect" && $6!="reclassify") \
      || ($6=="defect" && $7 !~ /^CTF-P[0-9][0-9]*$/) || ($6!="defect" && $7!="-") \
      || ($6=="reclassify" && ($8=="" || $8=="-")) || ($6!="reclassify" && $8!="-") \
      || ($4 ~ /\/(Users|home|Volumes|var\/folders|private\/tmp|tmp)\//) \
      || ($4 ~ /(gh[pousr]_|sk-|ghs_|Bearer )[A-Za-z0-9_.-]{10,}/) \
      || ($4 ~ /[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z][A-Za-z]+/) \
      || ($3 ~ /\/(Users|home|Volumes|var\/folders|private\/tmp|tmp)\//) \
      || ($3 ~ /(gh[pousr]_|sk-|ghs_|Bearer )[A-Za-z0-9_.-]{10,}/) \
      || ($3 ~ /[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z][A-Za-z]+/) \
      || ($3 !~ /^(libra-doc-anchor|grit-doc-anchor|git-probe)\|[^|]+\|[0-9a-f]{64}$/)) bad=1 }
  END { exit bad?1:0 }' "$D" \
  || { echo "FAIL: DEFECTS.md violates header/8-column/closed-set schema"; exit 1; }
SC=tests/compat-ledger/t4/DRAFT.sha256
[ -f "$SC" ] || { echo "FAIL: DRAFT.sha256 sidecar missing"; exit 1; }
[ "$(wc -l < "$SC")" -le 1 ] || { echo "FAIL: DRAFT.sha256 must be exactly one line"; exit 1; }
command grep -qx '[0-9a-f]\{64\}  DRAFT\.rs\.txt' "$SC" || { echo "FAIL: DRAFT.sha256 malformed"; exit 1; }
h=$(shasum -a 256 tests/compat-ledger/t4/DRAFT.rs.txt); h=${h%% *}
[ "$h  DRAFT.rs.txt" = "$(cat "$SC")" ] || { echo "FAIL: DRAFT.sha256 does not match DRAFT.rs.txt"; exit 1; }
echo OK
)
