#!/bin/sh
# tests/compat-ledger/t4/PRECHECK.sh —— 迁移预检驱动（CT3-04）
# 用法：DRAFT=<草稿测试文件路径> sh tests/compat-ledger/t4/PRECHECK.sh
set -eu
export LIBRA_SKIP_WEB_BUILD=1     # 本门与 web 无关；干净 target 下可避免触发 Next 构建
: "${DRAFT:?set DRAFT to the draft t4_port_test.rs}"
# run-scoped 输出（R26/R27 P0）：RUN_DIR 由**调用方**创建并 export——子进程无法把变量传回
# 父 shell，脚本自建则各门拿不到同一目录。契约调用形态（R33 起唯一入口）：
#   sh tests/compat-ledger/t4/PRECHECK_ALL.sh
# （其内部等价于：RUN_DIR=$(mktemp -d); export RUN_DIR;
#   sh tests/compat-ledger/t4/PRECHECK.sh && sh tests/compat-ledger/t4/GATES.sh）
: "${RUN_DIR:?create first: RUN_DIR=\$(mktemp -d); export RUN_DIR (owned by the caller)}"
[ -d "$RUN_DIR" ] && [ -w "$RUN_DIR" ] || { echo "FAIL: RUN_DIR is not a writable directory" >&2; exit 2; }
# **ER-11 确定性 redactor 的唯一生成器（R71 P1）**：除路径 / 已知 token 前缀 / 邮箱外，
# 还把**已知 secret 环境变量的实际值**逐一替换为占位符——按前缀猜必然漏掉无固定前缀的
# API/storage 凭据。生成的 `redact.sed` 供本脚本所有摘要点复用；脚本本身不写任何密文。
# R72 P1：必须**先加载测试环境再生成 redactor**——否则按值替换拿不到任何 secret 变量。
. ./.env.test
# 供其它门复用：`build_redactor_into <out>`（`build_redactor` 是它在本脚本的默认调用）
# ---- GC-17 注入锁的唯一取/放实现（计划级全局规范，逐字内联）----
# ——— GC-17 唯一实现（三处注入点逐字内联，不得改写）———
LOCKDIR=tests/compat-ledger/t4/.precheck.lock
_boot_token() { sysctl -n kern.boottime 2>/dev/null || cat /proc/sys/kernel/random/boot_id 2>/dev/null || echo unknown; }
_write_owner() {
  printf 'pid=%s host=%s boot=%s ts=%s\n' "$$" "$(hostname)" "$(_boot_token)" "$(date +%s)" \
    > "$LOCKDIR/owner" || { echo "FAIL: cannot record lock ownership" >&2; return 2; }
}
# **R75 P1**：`_write_owner` 失败后必须回滚目录——否则留下一个**无 owner 记录的锁**，而调用点的
# 清理 trap 是在取锁成功之后才安装的，没有任何路径能释放它；此后每一次运行都报
# 「lock directory exists without an owner record」并要求人工介入。
# R76 P1：两步清理的状态都不能丢——`rmdir` 失败（例如目录里冒出别的文件）会留下一个
# **已被删掉 owner** 的锁目录，此后每次取锁都停在「ownerless lock」并要求人工介入。
# 故：先尝试整体回滚；`rmdir` 失败时**把 owner 记录写回去**（带 abort 标记），使后续运行
# 至少能读到可判定的所有权信息，而不是一个无从裁决的空壳。
_abort_lock() {
  rm -f "$LOCKDIR/owner" 2>/dev/null
  if rmdir "$LOCKDIR" 2>/dev/null; then return 2; fi
  printf 'pid=%s host=%s boot=%s ts=%s abort=1\n' "$$" "$(hostname)" "$(_boot_token)" "$(date +%s)" \
    > "$LOCKDIR/owner" 2>/dev/null
  echo "FATAL: cannot roll back $LOCKDIR after a failed ownership write — resolve manually" >&2
  return 2
}
acquire_lock() {
  if mkdir "$LOCKDIR" 2>/dev/null; then _write_owner || _abort_lock || return 2; return 0; fi
  # 取锁失败：只有「同主机 + 同 boot token + owner PID 已消失」才允许具名回收并登记
  [ -f "$LOCKDIR/owner" ] \
    || { echo "FAIL: lock directory exists without an owner record — resolve manually" >&2; return 2; }
  lp=$(sed -n 's/.*pid=\([0-9]*\).*/\1/p' "$LOCKDIR/owner")
  lh=$(sed -n 's/.*host=\([^ ]*\).*/\1/p' "$LOCKDIR/owner")
  lb=$(sed -n 's/.*boot=\(.*\) ts=.*/\1/p' "$LOCKDIR/owner")
  if [ "$lh" = "$(hostname)" ] && [ "$lb" = "$(_boot_token)" ] && [ -n "$lp" ] && ! kill -0 "$lp" 2>/dev/null; then
    # **R76 P1（回收本身的 TOCTOU）**：两个进程可以**同时**通过上面的 stale 判定，各自
    # `rmdir` + `mkdir`——B 会把 A 刚建好的锁连同 owner 一起删掉再重建，于是两个注入运行
    # 同时进入临界区。回收动作必须自己也是互斥的：先取一把独立的 `.reclaim` 原子锁，
    # **持锁后重读并重新校验** owner（另一方可能已经完成回收，此时应放弃并按常规重试）。
    # **R79–R84**：`.reclaim` 互斥的唯一合法取得原语是 **`mkdir`（目录尚不存在时原子成功）**。
    # R84 实测：`mv "$_prep" "$LOCKDIR.reclaim"` 在目标**已是目录**时会成功地把 prep **嵌进**目标
    # （exit 0、原 owner 不变），输家误判为取得互斥。禁止用 `mv` 做「就位」；`mv` 只用于把
    # **整个** stale `.reclaim` 改名到私有 tomb。owner 写入后必须用本进程 pid **回读校验**。
    # 无 owner：一律「retry shortly」（可能是同伴正在写 owner），**不**回收、**不**嵌套 mv。
    if mkdir "$LOCKDIR.reclaim" 2>/dev/null; then
      printf 'pid=%s host=%s boot=%s ts=%s\n' "$$" "$(hostname)" "$(_boot_token)" "$(date +%s)" \
        > "$LOCKDIR.reclaim/owner" 2>/dev/null \
        || {
             # **R85 P1**：printf/重定向可能已创建空/半截 owner，裸 rmdir 会失败并被 `|| true` 吞掉，
             # 留下不可回收的 `.reclaim`（无合法 pid → 永远「held」）。先删 owner 再 rmdir，清不掉则 FATAL。
             rm -f "$LOCKDIR.reclaim/owner" 2>/dev/null || true
             if ! rmdir "$LOCKDIR.reclaim" 2>/dev/null; then
               echo "FATAL: cannot roll back .reclaim after owner write failure — resolve manually" >&2
             else
               echo "FAIL: cannot write .reclaim owner" >&2
             fi
             return 2
           }
    else
      if [ ! -f "$LOCKDIR.reclaim/owner" ]; then
        echo "FAIL: .reclaim present but owner not yet visible — retry shortly" >&2
        return 2
      fi
      rp=$(sed -n 's/.*pid=\([0-9]*\).*/\1/p' "$LOCKDIR.reclaim/owner")
      rh=$(sed -n 's/.*host=\([^ ]*\).*/\1/p' "$LOCKDIR.reclaim/owner")
      rb=$(sed -n 's/.*boot=\(.*\) ts=.*/\1/p' "$LOCKDIR.reclaim/owner")
      if [ -z "$rp" ]; then
        echo "FAIL: .reclaim owner has no parseable pid — retry shortly or resolve manually" >&2
        return 2
      fi
      if [ "$rh" = "$(hostname)" ] && [ "$rb" = "$(_boot_token)" ] && [ -n "$rp" ] \
           && ! kill -0 "$rp" 2>/dev/null; then
        echo "NOTE: claiming stale .reclaim (pid=$rp gone) via atomic directory rename" >&2
        _tomb="$LOCKDIR.reclaim.claim.$$"
        if mv "$LOCKDIR.reclaim" "$_tomb" 2>/dev/null; then
          rm -rf "$_tomb" 2>/dev/null || true
          # 认领成功后目标名必须空缺——再用 mkdir 独占创建（不得 mv prep 进去）
          if mkdir "$LOCKDIR.reclaim" 2>/dev/null; then
            printf 'pid=%s host=%s boot=%s ts=%s\n' "$$" "$(hostname)" "$(_boot_token)" "$(date +%s)" \
              > "$LOCKDIR.reclaim/owner" 2>/dev/null \
              || {
                   rm -f "$LOCKDIR.reclaim/owner" 2>/dev/null || true
                   if ! rmdir "$LOCKDIR.reclaim" 2>/dev/null; then
                     echo "FATAL: cannot roll back .reclaim after owner write failure — resolve manually" >&2
                   else
                     echo "FAIL: cannot write .reclaim owner after claim" >&2
                   fi
                   return 2
                 }
          else
            echo "FAIL: lost the race recreating .reclaim after claim — retry shortly" >&2
            return 2
          fi
        else
          echo "FAIL: lost the race to rename stale .reclaim — retry shortly" >&2
          return 2
        fi
        unset _tomb
      else
        echo "FAIL: another process holds .reclaim — retry shortly" >&2
        return 2
      fi
      unset rp rh rb
    fi
    # 回读校验：owner 必须是本进程（防止极端并发写穿）
    _op=$(sed -n 's/.*pid=\([0-9]*\).*/\1/p' "$LOCKDIR.reclaim/owner" 2>/dev/null || true)
    [ "$_op" = "$$" ] \
      || { echo "FAIL: .reclaim owner is not this process after acquire" >&2; return 2; }
    unset _op
    _rc=0
    _rc=0
    if [ ! -f "$LOCKDIR/owner" ]; then
      echo "FAIL: lock state changed during reclaim — retry shortly" >&2; _rc=2
    else
      lp2=$(sed -n 's/.*pid=\([0-9]*\).*/\1/p' "$LOCKDIR/owner")
      if [ "$lp2" != "$lp" ]; then
        echo "FAIL: the lock was reclaimed by another process — retry shortly" >&2; _rc=2
      else
        echo "NOTE: reclaiming a stale lock (owner pid=$lp is gone on this boot) — record this in the task log" >&2
        if rm -f "$LOCKDIR/owner" && rmdir "$LOCKDIR" && mkdir "$LOCKDIR" 2>/dev/null; then
          _write_owner || { _abort_lock; _rc=2; }
        else
          # **R78 P0**：`rm owner` 成功而 `rmdir` 失败时会留下 **ownerless 锁目录**——
          # 此后每次取锁都停在「lock directory exists without an owner record」并永久阻塞。
          # 任何部分失败都必须把可判定的 owner 写回（abort=1），让后续运行按 stale 规则自愈。
          if [ -d "$LOCKDIR" ] && [ ! -f "$LOCKDIR/owner" ]; then
            printf 'pid=%s host=%s boot=%s ts=%s abort=1\n' "$$" "$(hostname)" "$(_boot_token)" "$(date +%s)" \
              > "$LOCKDIR/owner" 2>/dev/null || true
          fi
          echo "FAIL: cannot reclaim the stale lock" >&2; _rc=2
        fi
      fi
    fi
    # **R77 P0（自审）**：`.reclaim` 释放失败时原文只把 `_rc` 置 2 就返回——而此刻**主锁已经取到**。
    # 调用方写的是 `acquire_lock || exit 2`，于是进程带着 `$LOCKDIR` 直接退出，而清理 trap 要到
    # 取锁**成功之后**才安装：主锁没有任何路径被释放。返回非零就绝不能仍然持锁——先把主锁还回去
    # （还不掉就写回带 `abort=1` 的 owner 记录，让后续运行至少能按 stale 规则自愈），再返回。
    # R79：先删 owner 再 rmdir；失败时写回 abort=1 owner 以便 stale 自愈
    rm -f "$LOCKDIR.reclaim/owner" 2>/dev/null
    if ! rmdir "$LOCKDIR.reclaim" 2>/dev/null; then
      printf 'pid=%s host=%s boot=%s ts=%s abort=1\n' "$$" "$(hostname)" "$(_boot_token)" "$(date +%s)" \
        > "$LOCKDIR.reclaim/owner" 2>/dev/null || true
      echo "FATAL: cannot release $LOCKDIR.reclaim — resolve manually" >&2
      [ "$_rc" -eq 0 ] && { _abort_lock; }
      _rc=2
    fi
    return "$_rc"
  fi
  echo "FAIL: another injection run holds the lock (owner is live or on another host/boot)" >&2; return 2
}
release_lock() {                      # 返回 0=已释放；非 0=释放失败（调用方必须置 rc=3）
  [ -d "$LOCKDIR" ] || return 0
  # **R78 P0**：先整体 rename 到 tombstone 再删除——避免「owner 已删、rmdir 失败」留下
  # ownerless 目录导致后续全部永久阻塞。rename 失败再回退到 owner 回写路径。
  _tomb="${LOCKDIR}.tomb.$$"
  if mv "$LOCKDIR" "$_tomb" 2>/dev/null; then
    rm -rf "$_tomb" 2>/dev/null || {
      echo "FATAL: cannot dispose of released lock tomb — resolve manually: $_tomb" >&2
      return 1
    }
    return 0
  fi
  rm -f "$LOCKDIR/owner" 2>/dev/null
  if rmdir "$LOCKDIR" 2>/dev/null; then return 0; fi
  printf 'pid=%s host=%s boot=%s ts=%s abort=1\n' "$$" "$(hostname)" "$(_boot_token)" "$(date +%s)" \
    > "$LOCKDIR/owner" 2>/dev/null || true
  echo "FATAL: cannot release the injection lock (owner restored with abort=1)" >&2
  return 1
}
# ——— GC-17 实现块结束 ———

# ---- GC-15 冻结锚点「按提交快照消费」协议的唯一实现（逐字内联）----
# snapshot_frozen <run-dir> <path>...  —— 断言已跟踪且无未提交改动，再把 HEAD blob 快照到私有目录
# **R76 P0**：本函数总是作为 `||` 的左操作数被调用，`set -e` 在其内部完全失效——所以**每一条**
# 命令都必须自带 `|| return 1`。原文的 `>> "$_rd/snap.manifest"` 没有：磁盘写满/权限变化时，
# 已写入的若干行仍在，函数继续跑到 `unset` 并返回 0；`verify_frozen` 又只拿被截断的清单自比，
# 于是「已验条数 == 清单行数」成立而漏掉的锚点**从未被检查**。改为逐条 guard + 请求数校验。
snapshot_frozen() {
  _rd=$1; shift
  _want_n=$#
  mkdir -p "$_rd/snap" || { echo "FAIL: cannot create the snapshot dir" >&2; return 1; }
  : > "$_rd/snap.manifest" || return 1
  for _f in "$@"; do
    libra ls-files --cached --error-unmatch -- "$_f" > /dev/null \
      || { echo "FAIL: frozen input is not tracked: $_f" >&2; return 1; }
    _st=$(libra status --porcelain=v1 -- "$_f") || { echo "FAIL: libra status failed on $_f" >&2; return 1; }
    [ -z "$_st" ] || { echo "FAIL: frozen input has uncommitted modifications: $_f" >&2; return 1; }
    mkdir -p "$_rd/snap/$(dirname "$_f")" || return 1
    libra show "HEAD:$_f" > "$_rd/snap/$_f" \
      || { echo "FAIL: cannot snapshot HEAD:$_f" >&2; return 1; }
    _h=$(shasum -a 256 "$_rd/snap/$_f") || { echo "FAIL: cannot hash the snapshot of $_f" >&2; return 1; }
    printf '%s  %s\n' "${_h%% *}" "$_f" >> "$_rd/snap.manifest" \
      || { echo "FAIL: cannot append $_f to the snapshot manifest" >&2; return 1; }
  done
  # R76 P0：清单行数必须等于**请求快照的路径条数**——只比「写了几行」无法发现整行丢失
  _got_n=$(wc -l < "$_rd/snap.manifest") || { echo "FAIL: cannot count the snapshot manifest" >&2; return 1; }
  [ "$_got_n" -eq "$_want_n" ] \
    || { echo "FAIL: snapshotted $_got_n of $_want_n frozen inputs" >&2; return 1; }
  unset _rd _f _st _h _want_n _got_n
}
# verify_frozen <run-dir> —— 消费窗口结束时复验：状态仍为空且工作树内容仍等于快照
# **R75 P0：本函数此前在三种情形下静默「通过」**——① 清单为空（零次迭代直接走到 `unset`）；
# ② 清单文件不存在（重定向失败，但本函数总是作为 `||` 的左操作数被调用，`set -e` 在其内部
# 完全失效，于是继续执行到 `unset` 并返回 0）；③ 某行解析不出 `_f` 而被 `continue` 跳过。
# 三种都会让调用点打印「every frozen anchor is unchanged」而实际一个都没验。改为 fail-closed：
# 显式确认清单可读且非空、逐行强制两字段、末尾断言**已验条数 == 清单行数**。
verify_frozen() {
  _rd=$1
  [ -r "$_rd/snap.manifest" ] \
    || { echo "FAIL: snapshot manifest is missing or unreadable: $_rd/snap.manifest" >&2; return 1; }
  _total=$(wc -l < "$_rd/snap.manifest") || { echo "FAIL: cannot count the snapshot manifest" >&2; return 1; }
  [ "$_total" -gt 0 ] || { echo "FAIL: snapshot manifest is empty — nothing was snapshotted" >&2; return 1; }
  _seen=0
  while IFS=' ' read -r _want _f; do
    [ -n "$_want" ] && [ -n "$_f" ] \
      || { echo "FAIL: malformed snapshot manifest record" >&2; return 1; }
    _st=$(libra status --porcelain=v1 -- "$_f") \
      || { echo "FAIL: libra status failed on $_f" >&2; return 1; }
    [ -z "$_st" ] || { echo "FAIL: $_f changed during the consumption window" >&2; return 1; }
    _now=$(shasum -a 256 "$_f") || { echo "FAIL: cannot hash $_f" >&2; return 1; }
    [ "${_now%% *}" = "$_want" ] \
      || { echo "FAIL: $_f content changed during the consumption window" >&2; return 1; }
    _seen=$((_seen+1))
  done < "$_rd/snap.manifest"
  [ "$_seen" -eq "$_total" ] \
    || { echo "FAIL: verified $_seen of $_total frozen anchors" >&2; return 1; }
  unset _rd _want _f _st _now _total _seen
}

build_redactor_into() {
  _out=$1
  {
    printf '%s\n' 's#(/Users|/home|/Volumes|/var/folders|/private/tmp|/tmp)/[^ "]*#<REDACTED_PATH>#g'
    printf '%s\n' 's#(gh[pousr]_|github_pat_|ghs_|sk-|cfut_|xox[baprs]-|AKIA|ASIA|Bearer )[A-Za-z0-9_.-]{8,}#<REDACTED_SECRET>#g'
    printf '%s\n' 's#[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}#<REDACTED_EMAIL>#g'
    # 按值替换：遍历已知的 secret 变量名，非空且长度 >= 8 的值全部转义后加入替换表
    for v in LIBRA_TEST_GITHUB_TOKEN GITHUB_TOKEN GH_TOKEN \
             DEEPSEEK_API_KEY MOONSHOT_API_KEY ZHIPU_API_KEY GEMINI_API_KEY \
             OPENAI_API_KEY ANTHROPIC_API_KEY \
             LIBRA_D1_API_TOKEN LIBRA_STORAGE_ACCESS_KEY LIBRA_STORAGE_SECRET_KEY \
             LIBRA_TEST_S3_ACCESS_KEY LIBRA_TEST_S3_SECRET_KEY; do
      eval "val=\${$v:-}"
      [ -n "$val" ] && [ "${#val}" -ge 8 ] || continue
      esc=$(printf '%s' "$val" | sed -e 's/[][\\.^$*+?(){}|/#-]/\\&/g')
      printf 's#%s#<REDACTED_SECRET>#g\n' "$esc"
    done
  } > "$_out" || { echo "FAIL: cannot build the redactor" >&2; exit 2; }
  unset val esc _out
}
build_redactor() { build_redactor_into "$RUN_DIR/redact.sed"; }
build_redactor
[ -f "$DRAFT" ] || { echo "FAIL: draft not found: $DRAFT" >&2; exit 1; }
# 草稿绑定（R28 P0）：记录本轮**实际使用**的草稿摘要；verify 模式必须等于已提交 sidecar
# （防止「同名测试、不同断言」的替换草稿生成 canonical 结果或冒充复验）
h=$(shasum -a 256 "$DRAFT") || { echo "FAIL: cannot hash draft" >&2; exit 2; }
printf '%s  DRAFT.rs.txt\n' "${h%% *}" > "$RUN_DIR/draft.sha256"
if [ "${PRECHECK_MODE:-record}" = "verify" ]; then
  diff "$RUN_DIR/draft.sha256" tests/compat-ledger/t4/DRAFT.sha256 \
    || { echo "FAIL: verify mode must use the committed draft (DRAFT.sha256 mismatch)" >&2; exit 2; }
fi
# 快照消费（R37/R41）：快照由前置卡 **CT3-06** 生成、提交并推送——本脚本只消费：断言其存在、
# 已提交且无未提交改动、三列非空 schema（生成逻辑唯一归属 CT3-06 的快照生成门，本卡无生成分支）
SNAP=tests/compat-ledger/t4/DIRECT_SNAPSHOT.tsv
. ./.env.test                      # R37 P1：首个 cargo 调用前加载测试环境
[ -f "$SNAP" ] || { echo "FAIL: DIRECT_SNAPSHOT.tsv missing — run CT3-06 first (snapshot owner)" >&2; exit 2; }
# R58 P1：空状态不等于「已提交」——ignore/配置可隐藏 untracked 文件，先证明它在 index 里
libra ls-files --cached --error-unmatch -- "$SNAP" > /dev/null \
  || { echo "FAIL: DIRECT_SNAPSHOT.tsv is not tracked (never committed)" >&2; exit 2; }
snapst=$(libra status --porcelain=v1 -- tests/compat-ledger/t4/DIRECT_SNAPSHOT.tsv) \
  || { echo "FAIL: libra status failed" >&2; exit 2; }
[ -z "$snapst" ] || { echo "FAIL: DIRECT_SNAPSHOT.tsv has uncommitted modifications" >&2; exit 2; }
awk -F'\t' 'NF!=3 || $1=="" || $2=="" || $3=="" {exit 1}' "$SNAP" \
  || { echo "FAIL: snapshot violates three-column non-empty schema" >&2; exit 2; }
# R44/R45：草稿侧锚点必须是 CT3-06 已提交且未改动的版本——**冻结草稿永不编辑**；
# 改判只影响运行时的「活跃投影」（下文机械生成），不触碰任何已冻结文件
# R70 P1：`PROBES.allow` 与 `REPLAY_SOURCES.sh` 同为本卡只读消费的冻结锚点，一并纳入
for f in tests/compat-ledger/t4/DRAFT.rs.txt tests/compat-ledger/t4/DRAFT.sha256 \
         tests/compat-ledger/t4/PRECHECK_ASSERTIONS.lock tests/compat-ledger/t4/PROJECT_DRAFT.sh \
         tests/compat-ledger/t4/CLEANROOM.sh tests/compat-ledger/t4/PROBES.allow \
         tests/compat-ledger/t4/REPLAY_SOURCES.sh; do
  [ -s "$f" ] || { echo "FAIL: $f missing (CT3-06 deliverable)" >&2; exit 2; }
  libra ls-files --cached --error-unmatch -- "$f" > /dev/null \
    || { echo "FAIL: $f is not tracked (never committed by CT3-06)" >&2; exit 2; }   # R58 P1
  st=$(libra status --porcelain=v1 -- "$f") || { echo "FAIL: libra status failed" >&2; exit 2; }
  [ -z "$st" ] || { echo "FAIL: $f has uncommitted modifications (frozen by CT3-06)" >&2; exit 2; }
done
# **GC-15**：快照后只消费副本；窗口结束调用 `verify_frozen "$RUN_DIR"`
snapshot_frozen "$RUN_DIR" \
  tests/compat-ledger/t4/DRAFT.rs.txt tests/compat-ledger/t4/DRAFT.sha256 \
  tests/compat-ledger/t4/PRECHECK_ASSERTIONS.lock tests/compat-ledger/t4/PROJECT_DRAFT.sh \
  tests/compat-ledger/t4/CLEANROOM.sh tests/compat-ledger/t4/PROBES.allow \
  tests/compat-ledger/t4/REPLAY_SOURCES.sh tests/compat-ledger/t4/DIRECT_SNAPSHOT.tsv \
  tests/compat-ledger/t4/EXPECTED.txt \
  || { echo "FAIL: cannot snapshot the frozen anchors (GC-15)" >&2; exit 2; }
# **R73/R78 P1（GC-15 ③ 落地）**：以下所有锚点消费一律走快照副本变量，不得再写工作树字面路径。
SNAPD="$RUN_DIR/snap"
A_PROJECT="$SNAPD/tests/compat-ledger/t4/PROJECT_DRAFT.sh"
A_DRAFT="$SNAPD/tests/compat-ledger/t4/DRAFT.rs.txt"
A_LOCK="$SNAPD/tests/compat-ledger/t4/PRECHECK_ASSERTIONS.lock"
A_DIRECT="$SNAPD/tests/compat-ledger/t4/DIRECT_SNAPSHOT.tsv"
A_CLEANROOM="$SNAPD/tests/compat-ledger/t4/CLEANROOM.sh"
A_REPLAY="$SNAPD/tests/compat-ledger/t4/REPLAY_SOURCES.sh"
A_PROBES="$SNAPD/tests/compat-ledger/t4/PROBES.allow"
A_EXPECTED="$SNAPD/tests/compat-ledger/t4/EXPECTED.txt"
# R78 P1：`$SNAP` 原先指向工作树 DIRECT_SNAPSHOT.tsv——快照后仍被 cut/join 消费。
# 立即重绑定到快照副本，后续所有读取只走 $SNAP / $A_*。
SNAP="$A_DIRECT"
[ -s "$SNAP" ] || { echo "FAIL: snapshotted DIRECT_SNAPSHOT.tsv missing" >&2; exit 2; }
# 解析器守卫存在性（零匹配的 cargo 过滤器仍返回 0——先锁定具名函数，R42）
LIBRA_SKIP_WEB_BUILD=1 cargo test --test compat_ledger_schema ledger_dump -- --list > "$RUN_DIR/live_list.txt" \
  || { echo "FAIL: --list failed" >&2; exit 2; }
# R64 P1：锚定到模块边界并要求**恰一次**命中（裸子串可被 `fake_ledger_dump_direct_ids` 冒充）
if command grep -cE '(^|::)ledger_dump_direct_ids: test$' "$RUN_DIR/live_list.txt" > "$RUN_DIR/nhit.txt"; then :; else
  rc=$?; [ "$rc" -eq 1 ] || { echo "FAIL: grep failed with $rc" >&2; exit "$rc"; }
  echo 0 > "$RUN_DIR/nhit.txt"
fi
[ "$(cat "$RUN_DIR/nhit.txt")" -eq 1 ] || { echo "FAIL: ledger_dump_direct_ids guard missing" >&2; exit 2; }
LIBRA_SKIP_WEB_BUILD=1 cargo test --test compat_ledger_schema ledger_dump_direct_ids -- --exact --nocapture > "$RUN_DIR/live_ids.out" \
  || { echo "FAIL: cannot dump live direct ids" >&2; exit 2; }
sed -n 's/^DIRECT_ID //p' "$RUN_DIR/live_ids.out" > "$RUN_DIR/live_ids.rows" || { echo "FAIL: sed DIRECT_ID" >&2; exit 2; }
cut -f1 "$RUN_DIR/live_ids.rows" > "$RUN_DIR/live_ids.raw" || { echo "FAIL: cut DIRECT_ID col1" >&2; exit 2; }
[ -s "$RUN_DIR/live_ids.raw" ] || { echo "FAIL: ledger_dump_direct_ids produced zero DIRECT_ID rows" >&2; exit 2; }
LC_ALL=C sort -u "$RUN_DIR/live_ids.raw" > "$RUN_DIR/live_ids.txt"
# **活跃集派生规范（R42 唯一定义；GATES.sh 与本脚本共用同一算法）**：
#   FROZEN  = 快照前两列（冻结全集，只读）
#   ACTIVE  = FROZEN 中 scenario_id ∈ 当前账本 direct 集的行（= 尚未被改判的场景）
#   RECLASS = FROZEN − ACTIVE（被 CT3-03 改判移出 direct 的场景）
# 活跃集**永不落盘进仓库**，每轮从冻结基与实时账本重新派生——这消除了 R37–R42 反复出现的
# 「冻结文件必须随改判收缩」死锁。
cut -f1,2 "$SNAP" > "$RUN_DIR/frozen.raw" || { echo "FAIL: cut snapshot" >&2; exit 2; }
LC_ALL=C sort "$RUN_DIR/frozen.raw" > "$RUN_DIR/frozen.txt"
join -t "$(printf '\t')" "$RUN_DIR/frozen.txt" "$RUN_DIR/live_ids.txt" > "$RUN_DIR/active_pairs.txt" \
  || { echo "FAIL: derive active pairs" >&2; exit 2; }
[ -s "$RUN_DIR/active_pairs.txt" ] \
  || { echo "FAIL: active set is empty (every direct scenario reclassified) — see 改判重跑流程 的终态边界" >&2; exit 2; }
comm -23 "$RUN_DIR/frozen.txt" "$RUN_DIR/active_pairs.txt" > "$RUN_DIR/reclass_pairs.txt"
# **活跃草稿投影（R45 P0，唯一定义）**：冻结草稿是不可变的；本轮实际注入的是它的**投影**——
# 删除已改判场景的测试函数、保留其余函数与四个固定守卫。投影是「冻结草稿 + 当前账本」的纯函数，
# 可由任何人确定性重算（CT3-02 的草稿逐字门即重算它并与最终入库测试 `cmp`）。
awk -F'\t' '{print $2}' "$RUN_DIR/reclass_pairs.txt" > "$RUN_DIR/drop_fns.txt"
sh "$A_PROJECT" "$A_DRAFT" \
   "$RUN_DIR/drop_fns.txt" "$RUN_DIR/draft.active.rs" \
  || { echo "FAIL: project frozen draft onto the active set" >&2; exit 2; }
DRAFT="$RUN_DIR/draft.active.rs"      # 本轮注入的目标 = 活跃投影（冻结草稿保持只读）
cut -f1 "$SNAP" > "$RUN_DIR/snap_ids.raw" || { echo "FAIL: cut snapshot ids" >&2; exit 2; }
LC_ALL=C sort -u "$RUN_DIR/snap_ids.raw" > "$RUN_DIR/snap_ids.txt"
comm -23 "$RUN_DIR/live_ids.txt" "$RUN_DIR/snap_ids.txt" > "$RUN_DIR/extra_ids.txt"
if [ -s "$RUN_DIR/extra_ids.txt" ]; then
  echo "FAIL: live direct set grew beyond frozen snapshot:" >&2; exit 2   # R67 P1（ER-11）：ID 清单留在 RUN_DIR
fi
# R43 P1 / R58 P1：注入—测试—复原窗口取独占锁；**存在性检查必须在锁内重做**（原文先检查
# 后取锁，两者之间是 TOCTOU 窗口）。R46 P1：锁一旦获得立刻安装清理，fail-closed。
# **锁的所有权与失效回收（R71 P1）**：`mkdir` 锁在 `SIGKILL`/主机重启/shell 崩溃后 trap 不会
# 运行，目录永久残留，而「确认没有运行再删除」在没有 PID/主机/启动 token 的情况下**不可验证**，
# 执行者只能永久阻塞或冒险破锁。契约：取锁成功后立即在锁目录内写 `owner` 文件，内容为
# `pid=<PID> host=<hostname> boot=<启动 token> ts=<epoch>`；取锁失败时读该文件判定——
# 同主机且同 boot token 而 PID 已不存在（`kill -0` 失败）即为 **stale**，可具名回收并登记；
# 其余情形一律阻塞并要求人工裁决。回收动作必须记入任务记录。
# **注入互斥协议（R58 P1，全计划唯一：三处注入点共用同一把锁）**：先取独占锁，**在锁内**
# 重检目标不存在，再以 `set -C`（noclobber）原子创建——「先检查后复制」之间存在 TOCTOU
# 覆盖窗口，两个进程可同时通过存在性检查并互相覆盖草稿 / 重复追加 `mod.rs`。
# **R73 P1**：取锁走 **GC-17 的唯一实现**——`LOCKDIR` 定义、`_boot_token`、`_write_owner`、
# `acquire_lock`、`release_lock` 五段逐字内联于本脚本头部（此前只有本处实现了 owner 记录与
# stale 回收，另两处是裸 `mkdir`，跨脚本组合必然阻塞）。
acquire_lock || exit 2
# R63 P1：只释放锁的 handler 也必须**退出**，且信号路径保留 130/143（否则收到 INT/TERM
# 后 shell 会继续执行备份与注入，而另一进程此刻已能取得同一把锁）
lock_only() {
  r=$?; [ -n "${1:-}" ] && r=$1
  trap - EXIT INT TERM              # 自审 P2：先清 trap，否则本函数的 exit 会二次触发自身
  release_lock || r=3               # R73 P1：GC-17 唯一释放实现
  exit "$r"
}
trap 'lock_only' EXIT; trap 'lock_only 130' INT; trap 'lock_only 143' TERM
[ ! -e tests/command/t4_port_test.rs ] \
  || { echo "FAIL: tests/command/t4_port_test.rs already exists (re-checked inside the lock)" >&2; exit 1; }
BAK="$RUN_DIR/mod.rs.bak"           # run-scoped 备份，避免与并发运行互相覆盖（R27 P0）
cp tests/command/mod.rs "$BAK"
bh=$(shasum -a 256 "$BAK"); BAK_SHA=${bh%% *}
cat "$BAK" > "$RUN_DIR/mod.rs.expected"
printf 'mod t4_port_test;\n' >> "$RUN_DIR/mod.rs.expected"
cleanup() {
  rc=$?
  [ -n "${1:-}" ] && rc=$1          # 信号路径显式传入 130/143，$? 在此不可靠
  set +e   # R70 P1：handler 内禁用隐式 set -e 中止——任一 rm 失败都不得让锁与复原半途终止
  trap - EXIT INT TERM              # R41 P1：先清除 traps，防信号路径二次触发 EXIT trap
  # R38 P1：复原走 CAS——只有当前 mod.rs 恰为「备份 + 本脚本追加的一行」时才回写备份；
  # 并发节点若在预检期间改动了 mod.rs，宁可保留现场、告警并以非零码退出（GC-12）
  # R46 P1：目标文件消失属内部错误（不得静默通过）
  [ -f tests/command/mod.rs ] || { echo "FATAL: tests/command/mod.rs disappeared during the run" >&2; rc=3; }
  if [ -f tests/command/mod.rs ]; then
    if cmp -s tests/command/mod.rs "$RUN_DIR/mod.rs.expected"; then
      cp "$BAK" tests/command/mod.rs || { echo "FATAL: restore of mod.rs failed" >&2; rc=3; }
    elif nh=$(shasum -a 256 tests/command/mod.rs) && [ "${nh%% *}" = "$BAK_SHA" ]; then
      : # 已是原状（复原过或注入未生效），无需动作
    else
      echo "FATAL: mod.rs changed concurrently; backup kept at $BAK — resolve manually" >&2; rc=3
    fi
  fi
  # 草稿副本删除同样 CAS：仅当其内容仍等于 $DRAFT 时才删除
  if [ -f tests/command/t4_port_test.rs ]; then
    if cmp -s tests/command/t4_port_test.rs "$DRAFT"; then
      rm -f tests/command/t4_port_test.rs || { echo "FATAL: draft removal failed" >&2; rc=3; }
    else
      echo "FATAL: t4_port_test.rs diverged from draft during run — not deleting" >&2; rc=3
    fi
  fi
  [ "$rc" -eq 3 ] || rm -f "$BAK"
  release_lock || rc=3              # R73 P1：GC-17 唯一释放实现
  exit "$rc"                        # ← 复原失败必须让最终退出码非零
}
trap cleanup EXIT; trap 'cleanup 130' INT; trap 'cleanup 143' TERM   # ← 任何异常退出都会复原（R41：信号码保留）
# R58 P1：noclobber 原子创建——若在锁内到写入之间仍被抢先创建，`set -C` 会让重定向失败
( set -C; : > tests/command/t4_port_test.rs ) \
  || { echo "FAIL: injection target appeared concurrently — aborting" >&2; exit 2; }
cp "$DRAFT" tests/command/t4_port_test.rs
printf 'mod t4_port_test;\n' >> tests/command/mod.rs
# 枚举：编译或枚举失败必须与「用例失败」区分开
if cargo test --test command_test t4_port -- --list > "$RUN_DIR/list.txt" 2>"$RUN_DIR/list.err"; then
  :
else
  echo "FAIL: draft does not compile or enumerate (stderr kept at \$RUN_DIR/list.err)" >&2; exit 2   # R65 P1（ER-11）：不直接 cat 未脱敏输出
fi
# 逐个具名运行，输出机器可解析记录 "<test_fn>\t<pass|fail>"
# （R24 P0-01）--list 的行形如 `<module>::<fn>: test`；--exact 必须用**完整模块限定名**，
# 否则裸 fn 名零匹配、cargo 仍退出 0，会把没跑的用例记成 pass。因此：
# ① 提取时保留完整限定名；② 断言枚举非空；③ 每次 --exact 运行都断言恰命中 1 个用例。
: > "$RUN_DIR/results.tsv"
# 2026-08-06 收紧：按**模块前缀**提取草稿模块内的全部测试（原按 `t4_port_` 子串提取时，
# 草稿内不带该前缀的杂散 `#[test]` 会逃过枚举与下方的集合相等门）。
# 前缀必须含顶层 `command::`——`tests/command_test.rs:8` 声明 `mod command;`，草稿在
# `tests/command/mod.rs` 内注册为其子模块，实跑 `--list` 输出形如
# `command::config_test::config_bare_read_single_value: test`（R33 P0 实测）
sed -n 's/^\(command::t4_port_test::[A-Za-z0-9_:]*\): test$/\1/p' "$RUN_DIR/list.txt" > "$RUN_DIR/fns.txt"
[ -s "$RUN_DIR/fns.txt" ] || { echo "FAIL: --list enumerated zero tests under command::t4_port_test::" >&2; exit 2; }
# 记录用裸 fn 名（与冻结快照 / DEFECTS.md 同一身份口径）；跨模块裸名冲突即失败
sed 's/^.*:://' "$RUN_DIR/fns.txt" > "$RUN_DIR/bare.txt"
LC_ALL=C sort -u "$RUN_DIR/bare.txt" > "$RUN_DIR/bare.uniq"
[ "$(wc -l < "$RUN_DIR/bare.txt")" -eq "$(wc -l < "$RUN_DIR/bare.uniq")" ] \
  || { echo "FAIL: duplicate bare test_fn across modules" >&2; exit 2; }
# 场景/守卫拆分（R30 P0）：草稿与最终文件逐字相等（CT3-02 的 cmp 门），因此草稿也含四个
# 具名守卫；守卫不是迁移场景，不得逐个执行、不得进结果记录。GUARDS 是固定守卫集合的
# **唯一清单**（CT4-01 的 integrity ⑤ 与 --list 承诺门引用它）；场景集 = 派生的**活跃**集（R42）
GUARDS="t4_port_integrity
t4_port_no_foreign_harness
t4_port_tests_document_scope_and_boundaries
t4_port_direct_rows_have_tests"
printf '%s\n' $GUARDS > "$RUN_DIR/guards.txt"
awk -F'\t' '{print $2}' "$RUN_DIR/active_pairs.txt" > "$RUN_DIR/scen_fns.txt" \
  || { echo "FAIL: parse active pairs" >&2; exit 2; }
[ -s "$RUN_DIR/scen_fns.txt" ] || { echo "FAIL: active pair set has no rows" >&2; exit 2; }
# --list 全集必须恰等于「场景函数 + 固定守卫集合」（多余或缺失都失败）
cat "$RUN_DIR/scen_fns.txt" "$RUN_DIR/guards.txt" > "$RUN_DIR/expected_all.raw" || { echo "FAIL: cat expected sets" >&2; exit 2; }
LC_ALL=C sort "$RUN_DIR/expected_all.raw" > "$RUN_DIR/expected_all.txt"
LC_ALL=C sort "$RUN_DIR/bare.txt" > "$RUN_DIR/bare.sorted"
diff "$RUN_DIR/expected_all.txt" "$RUN_DIR/bare.sorted" \
  || { echo "FAIL: --list set != scenario functions + fixed guard set" >&2; exit 2; }
# 判定只认 libtest 的**行首汇总行**（防测试输出自身含 "1 failed" 之类短语被误判；
# 基础设施错误既无 ok 也无 FAILED 汇总行 → 一律 exit 2）；守卫函数直接跳过
while read -r fn; do
  bare=${fn##*::}
  if command grep -qx "$bare" "$RUN_DIR/guards.txt"; then continue; fi
  mkdir -p "$RUN_DIR/out"
  if cargo test --test command_test "$fn" -- --exact > "$RUN_DIR/out/$bare.log" 2>&1; then
    command grep -q '^test result: ok\. 1 passed; 0 failed;' "$RUN_DIR/out/$bare.log" \
      || { echo "FAIL: --exact matched zero tests for $fn (log kept at \$RUN_DIR/out/$bare.log)" >&2; exit 2; }
    printf '%s\tpass\n' "$bare" >> "$RUN_DIR/results.tsv"
    # R64 P1（ER-11）：通过用例同样留下未脱敏 `.log`，就地删除——清除不分成败
    rm -f "$RUN_DIR/out/$bare.log" || { echo "FAIL: cannot purge raw log for $bare" >&2; exit 2; }
  else
    command grep -q '^test result: FAILED\. 0 passed; 1 failed;' "$RUN_DIR/out/$bare.log" \
      || { echo "FAIL: run error (not a single-test failure) for $fn (log kept at \$RUN_DIR/out/$bare.log)" >&2; exit 2; }
    printf '%s\tfail\n' "$bare" >> "$RUN_DIR/results.tsv"
    # R48 P1：逐场景保留实际输出摘要（≤200 字符，供 `DEFECTS.md` 的 libra_actual 列填写与绑定）
    # R49 P1：`sed -n …p` 只打印发生替换的行，普通 libtest 日志会变成空串；拆成三段并各查退出码
    sed 's/[[:cntrl:]]//g' "$RUN_DIR/out/$bare.log" > "$RUN_DIR/out/$bare.s0" \
      || { echo "FAIL: sed on $bare log" >&2; exit 2; }
    # ER-11 确定性 redactor（R50/R51；R57 补 `/Volumes`——**本仓库当前 checkout 就在
    # `/Volumes/Data/...`**，原清单会让它原样进入证据并通过全部拒绝式）：绝对路径、token/key、
    # 邮箱一律替换为稳定占位符
    # **R71 P1：按前缀猜 token 必然漏**——`.env.test` 里实际存在 `github_pat_`、`cfut_` 以及
    # **无稳定前缀**的 API/storage 凭据。除扩充前缀清单外，更关键的是**按已知 secret 环境变量
    # 的实际值**逐一替换（值本身不写进计划）。生成器见下方 `build_redactor`。
    sed -E -f "$RUN_DIR/redact.sed" \
           "$RUN_DIR/out/$bare.s0" > "$RUN_DIR/out/$bare.s1" \
      || { echo "FAIL: redact on $bare log" >&2; exit 2; }
    tr '\n' ' ' < "$RUN_DIR/out/$bare.s1" > "$RUN_DIR/out/$bare.s2" \
      || { echo "FAIL: tr on $bare log" >&2; exit 2; }
    cut -c1-200 < "$RUN_DIR/out/$bare.s2" > "$RUN_DIR/out/$bare.summary" \
      || { echo "FAIL: cut on $bare log" >&2; exit 2; }
    printf '%s\t%s\n' "$bare" "$(cat "$RUN_DIR/out/$bare.summary")" >> "$RUN_DIR/actuals.tsv"
    # **ER-11 就地清除（R66 P1）**：摘要已落盘，原始日志与三个中间态立即删除——它们是未脱敏
    # 的原始工具输出。少了这一步，任何**预期中的**失败用例都会把 `.log/.s0/.s1/.s2` 留到
    # `pa_cleanup`，后者把退出码改成 2，defect/reclassify 流程永远无法成功收尾。
    rm -f "$RUN_DIR/out/$bare.log" "$RUN_DIR/out/$bare.s0" \
          "$RUN_DIR/out/$bare.s1" "$RUN_DIR/out/$bare.s2" "$RUN_DIR/out/$bare.summary" \
      || { echo "FAIL: cannot purge raw logs for $bare" >&2; exit 2; }
  fi
done < "$RUN_DIR/fns.txt"
# **改判行复验（R54 P1，取代「依赖不可变 canonical 证据」的弱保证）**：validate 阶段对每个
# `reclassify` 行，把注入点的内容**临时换成冻结全量草稿**并只跑该函数，断言它**仍然失败**——
# 「原始 fail」因此在任何时刻都可复验，不再依赖 canonical 未被改写；其脱敏摘要并入 actuals。
# 注意（R55 自审 P0）：此处 `tests/command/t4_port_test.rs` **已由本脚本注入活跃投影**、
# `tests/command/mod.rs` **已注册该模块**，故本段只做**同一路径的内容置换**：不新建注册行、
# 不删除文件，结束时把内容原样还原为 `$DRAFT`，使外层 `cleanup` 的 CAS 复原判据继续成立。
# 整段放入**子 shell**：内层 trap 只作用于子 shell，不覆盖外层 `cleanup`（R48 P1 的既定模式）。
if [ "${PRECHECK_MODE:-record}" = "verify" ] && [ -s tests/compat-ledger/t4/DEFECTS.md ]; then
  awk -F'\t' 'NR>1 && $6=="reclassify" {print $1}' tests/compat-ledger/t4/DEFECTS.md \
    > "$RUN_DIR/rc_fns.txt" || { echo "FAIL: cannot extract reclassify rows" >&2; exit 2; }
  if [ -s "$RUN_DIR/rc_fns.txt" ]; then
    # R58 P1：本段在 `PRECHECK.sh` 已持有的 `$LOCKDIR` 内运行（注入窗口尚未关闭），故只做
    # 不变量断言、不再取锁；置换是**同一路径的内容改写**，不新建文件，无 TOCTOU 创建竞争。
    [ -f tests/command/t4_port_test.rs ] && cmp -s tests/command/t4_port_test.rs "$DRAFT" \
      || { echo "FAIL: injection point is not the active projection — internal invariant broken" >&2; exit 2; }
    : > "$RUN_DIR/rc_actuals.tsv"
    (
      rc_restore() {                    # 只在本子 shell 内注册；外层 cleanup 不受影响
        r=$?; [ -n "${1:-}" ] && r=$1   # 信号路径显式传入 130/143（$? 在此不可靠）
        trap - EXIT INT TERM
        set +e   # R70 P1：handler 内禁用隐式 set -e 中止——任一 rm 失败都不得让锁与复原半途终止
        # R55 P1：复原同样走 **CAS**——无条件 `cp` 会覆盖并发节点在本段期间对该路径的改动，
        # 绕过外层 cleanup 的同类保护。三态：仍是冻结全量草稿 → 还原为活跃投影；已是活跃投影
        # → no-op；其余（被第三方改写）→ 保留现场、告警、以 3 退出。
        # R78 P1：冻结草稿内容一律取自 $A_DRAFT（GC-15 快照），不得再读工作树字面路径
        if cmp -s tests/command/t4_port_test.rs "$A_DRAFT"; then
          cp "$DRAFT" tests/command/t4_port_test.rs \
            || { echo "FATAL: cannot restore the active projection at the injection point" >&2; r=3; }
        elif cmp -s tests/command/t4_port_test.rs "$DRAFT"; then
          :                             # 已是活跃投影（复原过或置换未生效）
        else
          echo "FATAL: t4_port_test.rs changed concurrently during the re-verify — left in place" >&2
          r=3
        fi
        exit "$r"
      }
      trap rc_restore EXIT; trap 'rc_restore 130' INT; trap 'rc_restore 143' TERM
      # R56 P2：本段唯一承载语义的命令，必须显式查退出码——子 shell 作为 `||` 的左操作数时
      # `set -e` 在其内部失效，静默失败会让「以冻结草稿复验」退化为「以活跃投影复验」
      cp "$A_DRAFT" tests/command/t4_port_test.rs \
        || { echo "FAIL: cannot swap in the frozen full draft" >&2; exit 2; }
      cmp -s tests/command/t4_port_test.rs "$A_DRAFT" \
        || { echo "FAIL: injection point is not the frozen full draft after the swap" >&2; exit 2; }
      while read -r rf; do
        [ -n "$rf" ] || continue
        if cargo test --test command_test "command::t4_port_test::$rf" -- --exact \
             > "$RUN_DIR/rc.out" 2>&1; then
          echo "FAIL: reclassified $rf PASSES on the frozen draft — not a classification error" >&2
          exit 2
        fi
        command grep -q '^test result: FAILED\. 0 passed; 1 failed;' "$RUN_DIR/rc.out" \
          || { echo "FAIL: $rf did not produce a single-test failure (log kept at \$RUN_DIR/rc.out)" >&2; exit 2; }
        # 与主循环同一套三段式 + ER-11 确定性 redactor
        sed 's/[[:cntrl:]]//g' "$RUN_DIR/rc.out" > "$RUN_DIR/rc.s0" \
          || { echo "FAIL: sed on $rf log" >&2; exit 2; }
        # R72 P1：改判复验同样复用按值生成的 `redact.sed`
      sed -E -f "$RUN_DIR/redact.sed" "$RUN_DIR/rc.s0" > "$RUN_DIR/rc.s1" \
        || { echo "FAIL: redact on $rf log" >&2; exit 2; }
        tr '\n' ' ' < "$RUN_DIR/rc.s1" > "$RUN_DIR/rc.s2" || { echo "FAIL: tr on $rf log" >&2; exit 2; }
        cut -c1-200 < "$RUN_DIR/rc.s2" > "$RUN_DIR/rc.sum" || { echo "FAIL: cut on $rf log" >&2; exit 2; }
        printf '%s\t%s\n' "$rf" "$(cat "$RUN_DIR/rc.sum")" >> "$RUN_DIR/rc_actuals.tsv"
        rm -f "$RUN_DIR/rc.out" "$RUN_DIR/rc.s0" "$RUN_DIR/rc.s1" "$RUN_DIR/rc.s2" "$RUN_DIR/rc.sum" \
          || { echo "FAIL: cannot purge reclassify raw logs" >&2; exit 2; }   # R66 P1（ER-11）
      done < "$RUN_DIR/rc_fns.txt"
    ) || { echo "FAIL: reclassify re-verify failed (see the message above)" >&2; exit 2; }
    # 复原核验（子 shell 的 trap 之外再独立断言一次，信号路径同样覆盖）
    cmp -s tests/command/t4_port_test.rs "$DRAFT" \
      || { echo "FAIL: injection point was not restored to the active projection" >&2; exit 2; }
    # 覆盖门：每个 reclassify 行都必须留下一条摘要（信号中断导致的截断在此暴露）
    [ "$(wc -l < "$RUN_DIR/rc_actuals.tsv")" -eq "$(wc -l < "$RUN_DIR/rc_fns.txt")" ] \
      || { echo "FAIL: re-verify did not cover every reclassify row" >&2; exit 2; }
    cat "$RUN_DIR/rc_actuals.tsv" >> "$RUN_DIR/actuals.tsv"
  fi
fi
# 持久化模式（R26 P0；R42 改为 **write-once**）：canonical `PRECHECK_RESULTS.tsv` 只在**首轮
# collect**（此时 ACTIVE == FROZEN，尚无改判）落盘，此后任何一轮都不得改写。R54 P1：write-once
# 只是**约定**，不构成不可变证据（等行数改写无法被行数门发现），因此改判行的「原始 fail」不再
# 依赖它——由上方「改判行复验」段每轮以冻结全量草稿实跑重新证明；canonical 退化为一致性记录。
case "${PRECHECK_MODE:-record}" in
  record)
    if [ -f tests/compat-ledger/t4/PRECHECK_RESULTS.tsv ]; then
      echo "FAIL: PRECHECK_RESULTS.tsv already exists — canonical results are write-once (rerun with PRECHECK_MODE=verify)" >&2; exit 2
    fi
    diff "$RUN_DIR/frozen.txt" "$RUN_DIR/active_pairs.txt" \
      || { echo "FAIL: reclassification happened before the initial collect — restart from CT3-06" >&2; exit 2; }
    cp "$RUN_DIR/results.tsv" tests/compat-ledger/t4/PRECHECK_RESULTS.tsv ;;
  verify) : ;;
  *) echo "FAIL: unknown PRECHECK_MODE=${PRECHECK_MODE}" >&2; exit 2 ;;
esac
# 正文摘要（R43/R47；R65 P1 统一命名）：调用 CT3-06 交付的唯一词法实现 `--digest`，对**本轮
# 注入的投影**逐函数取「整个规范化函数体」的 sha256——`ASSERTIONS.lock` 与 `PRECHECK_ASSERTIONS.lock`
# 自此共用同一算法，不再有「断言行摘要 vs 正文摘要」的漂移
sh "$A_PROJECT" --digest "$DRAFT" "$RUN_DIR/assertions.lock" \
  || { echo "FAIL: cannot compute per-fn digests" >&2; exit 2; }
if [ "${PRECHECK_MODE:-record}" = "verify" ]; then
  # R44 P1：锁必须存在且非空——条件式跳过等于取消该门
  [ -s "$A_LOCK" ] \
    || { echo "FAIL: PRECHECK_ASSERTIONS.lock missing or empty (CT3-06 deliverable)" >&2; exit 2; }
  # R44 P1：比较键 = 活跃测试函数 ∪ 四个固定守卫（否则 collect 后可弱化 t4_port_integrity 等守卫）
  awk -F'\t' '{print $2}' "$RUN_DIR/active_pairs.txt" > "$RUN_DIR/act_fn.txt"
  cat "$RUN_DIR/guards.txt" >> "$RUN_DIR/act_fn.txt"
  LC_ALL=C sort -u "$RUN_DIR/act_fn.txt" > "$RUN_DIR/act_fn.sorted"
  LC_ALL=C sort "$A_LOCK" > "$RUN_DIR/lock.sorted"
  join -t "$(printf '\t')" "$RUN_DIR/act_fn.sorted" "$RUN_DIR/lock.sorted" > "$RUN_DIR/lock.active"
  LC_ALL=C sort "$RUN_DIR/assertions.lock" > "$RUN_DIR/now.sorted"
  join -t "$(printf '\t')" "$RUN_DIR/act_fn.sorted" "$RUN_DIR/now.sorted" > "$RUN_DIR/now.active"
  # join 行数必须等于键集合大小——缺项（函数被删/改名）同样是失败
  [ "$(wc -l < "$RUN_DIR/lock.active")" -eq "$(wc -l < "$RUN_DIR/act_fn.sorted")" ] \
    || { echo "FAIL: frozen lock does not cover every active test + guard" >&2; exit 2; }
  diff "$RUN_DIR/lock.active" "$RUN_DIR/now.active" \
    || { echo "FAIL: bodies of active tests/guards changed since freeze (weakening?)" >&2; exit 2; }
fi
# **GC-15**：消费窗口结束——复验全部冻结锚点仍与快照一致（R73 P1：此前 `verify_frozen` 从未被调用）
verify_frozen "$RUN_DIR" || { echo "FAIL: a frozen anchor changed during the consumption window" >&2; exit 2; }
echo "OK: $(wc -l < "$RUN_DIR/results.tsv") cases executed"
