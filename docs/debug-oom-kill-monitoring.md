# Debug：編譯/測試進程被 SIGKILL 與 OOM 監控

> 本文件記錄本開發主機（omarchy-lenovo）上「編譯與測試進程被殺（SIGKILL / OOM）」問題的
> 診斷結論與監控方案。適用於在此主機上跑的 cargo / rustc / 測試 binary
> （含 libra 與 mega2 的建置與測試）。
>
> 建立日期：2026-09-20

## 1. 背景與症狀

- 串行測試（`cargo test --lib -- --test-threads=1`）跑大量測試時，測試 binary 會中途被 **SIGKILL (signal 9)** 殺掉。
- cargo 回報特徵：`process didn't exit successfully: ... (signal: 9, SIGKILL: kill)`，進程回傳碼 101。
- 死亡的測試**不固定**（與記憶體累積相關，不是特定測試必死）。
- **只有測試 binary 死**，wrapper（cargo、agent harness）都存活 → 是針對性的殺戮。

## 2. 診斷結論（截至 2026-09-20）

| 訊號 | 結果 |
|---|---|
| 系統記憶體 | 93 Gi RAM + 186 Gi swap（zram 93.5G + swapfile 93.5G） |
| kernel OOM-kill（`journalctl -k`，近 5 天） | **零記錄** |
| cgroup `memory.events` oom_kill | **全 0**；`user@1000.service` / `app.slice` 的 `memory.max = max`（無限制） |
| systemd-oomd | 從未記錄任何 kill（已於 2026-09-19 停用；PSI 亦已關閉） |
| `vm.overcommit_memory` | 0（heuristic，預設），CommitLimit 充足 |

**結論：不是系統層面的 OOM。** 是針對性的外部 `kill -9`（userspace），
最可能來自 agent harness（cursor sandbox 的記憶體 watch dog）或自訂 cgroup 實驗腳本。
要確認兇手身份，需要 auditd 記錄 `kill()` syscall 的發送者（見 §4）。

## 3. 監控方案：oom-monitor.service

被動 watcher，**不依賴 PSI / systemd-oomd**（兩者皆已關閉）。四個獨立訊號 + 心跳：

1. **kernel OOM**：輪詢 `journalctl -k`，抓到 `Out of memory / Killed process` 立即記錄完整 kernel dump。
2. **cgroup OOM**：全 cgroup 掃描 `memory.events` 的 `oom_kill` 計數增量（涵蓋 docker scope、systemd-run scope）。
3. **建置進程死亡偵測**：盯住 `cargo / rustc / --test-threads / target/debug/deps` 進程，PID 一消失就抓快照 ——
   連不留任何記錄的外部 `kill -9` 也能捕捉。
4. **journal SIGKILL 掃描**：每 5 分鐘掃 systemd kill / SIGKILL 記錄。

每 15 秒記錄一行心跳（mem used / avail / swap / zram / top RSS / 建置進程數），
Log 位於 `~/oom-monitor/oom-monitor.log`，8 MiB 自動輪轉。

### 安裝

```bash
install -d ~/oom-monitor
# 將下方「源碼」的 oom-monitor.sh 存入 ~/oom-monitor/oom-monitor.sh
chmod +x ~/oom-monitor/oom-monitor.sh

# 以 systemd user unit 啟動（開機/重啟自動拉起，失敗自動重啟）
systemd-run --user --unit=oom-monitor --collect \
  --property=Restart=on-failure \
  --description="OOM/SIGKILL watcher for build&test" \
  /bin/bash ~/oom-monitor/oom-monitor.sh

systemctl --user status oom-monitor      # 確認 active
tail -f ~/oom-monitor/oom-monitor.log    # 觀察心跳
```

環境變數覆寫：`OOM_MONITOR_DIR`（log 目錄）、`OOM_MONITOR_INTERVAL`（輪詢秒數，預設 15）。

## 4. 監控方案：auditd 規則（抓 kill -9 的發送者）

auditd 記錄「誰對誰發了 SIGKILL」：發送者 PID / UID / 程序名 / exe / 父進程，受害進程 PID / 程序名，時間戳。

> 注意：audit 規則**沒有 `sig` 欄位**，需用 syscall 參數位置過濾：
> `kill(pid,sig)=a1`、`tkill(tid,sig)=a1`、`tgkill(tgid,tid,sig)=a2`、`pidfd_send_signal(pidfd,sig,info,flags)=a1`。

### 安裝（一次性，需 root）

```bash
sudo bash ~/oom-monitor/setup-audit.sh
```

腳本會：寫入 `/etc/audit/rules.d/oom-kill.rules`（持久規則）、加 systemd drop-in
（auditd 每次啟動重載規則）、`systemctl enable auditd`、載入規則、發一個測試 `kill -9` 自驗。
> auditd.service 有 `RefuseManualStop=yes`，`systemctl restart` 會被拒；腳本改用直接載入規則。

生效的規則：

```
-a always,exit -F arch=b64 -S kill -F a1=9 -k oomkill
-a always,exit -F arch=b64 -S tkill -F a1=9 -k oomkill
-a always,exit -F arch=b64 -S tgkill -F a2=9 -k oomkill
-a always,exit -F arch=b64 -S pidfd_send_signal -F a1=9 -k oomkill
```

### 查詢兇手

```bash
sudo bash ~/oom-monitor/check-audit.sh                      # 今天
sudo bash ~/oom-monitor/check-audit.sh --since "1 hour ago" # 指定時段
```

記錄範例（`ausearch -k oomkill -i`）：

```
type=OBJ_PID  : opid=975621 ocomm=bash            ← 受害進程
type=SYSCALL  : syscall=kill a1=SIGKILL exit=0
               pid=975473 comm=bash exe=/usr/bin/bash ppid=975472 auid=genedna
               key=oomkill                         ← 發送者身份
```

## 5. 排障流程與判定表

1. 監控 log 出現事件區塊（`KERNEL OOM EVENT` / `CGROUP OOM_KILL` / `BUILD PROC EXIT` / `SIGKILL RECORDS`）。
2. 對上時間點跑 `ausearch -k oomkill`。
3. 依下表判定兇手類型：

| 現象 | 判定 |
|---|---|
| `journalctl -k` 有 `Out of memory: Killed process`，且該 cgroup `oom_kill` 計數增加 | kernel OOM killer |
| 某 cgroup `memory.max` 非 `max`，`oom_kill` 計數 > 0 | cgroup 記憶體上限觸發 |
| 無任何 kernel / cgroup / systemd 記錄，但 `ausearch -k oomkill` 有記錄 | 外部 `kill -9`（發送者身份在 audit 記錄中） |
| 進程死了但 audit 也無記錄 | `cgroup.kill` 寫入（不留 syscall 記錄）；監控快照會顯示當時 cgroup 狀態 |

> 已知限制：audit 規則從載入時刻起才記錄，無法追溯歷史殺戮；
> `cgroup.kill` 類型的殺戮不走 kill syscall，audit 抓不到（但監控的 BUILD PROC EXIT + 快照仍會觸發）。

## 6. 源碼

### 6.1 `oom-monitor.sh`

```bash
#!/usr/bin/env bash
# oom-monitor.sh — passive OOM / SIGKILL watcher for compile & test workloads
#
# Auto-synced into libra & mega2 docs (debug-oom-kill-monitoring.md §6.1) by
# ~/oom-monitor/sync-docs.sh, triggered on change by the oom-sync.path unit.
#
# Independent signals (no PSI / systemd-oomd dependency):
#   1. Kernel OOM messages (journalctl -k, polled)      — real kernel OOM killer
#   2. cgroup v2 memory.events oom_kill counters        — container / scope limits
#   3. Build/test process death watcher (cargo/rustc/--test-threads/target/deps)
#      — fires on ANY death, even userspace kill -9 with no kernel record
#   4. Journal SIGKILL / systemd-kill scan              — systemd-level killers
# Plus rolling memory heartbeat so the state before/after every kill is visible.
#
# Env overrides: OOM_MONITOR_DIR, OOM_MONITOR_INTERVAL
set -u

BASE="${OOM_MONITOR_DIR:-$HOME/oom-monitor}"
LOG="$BASE/oom-monitor.log"
INTERVAL="${OOM_MONITOR_INTERVAL:-15}"
MAXLOG="${OOM_MONITOR_MAXLOG:-8388608}"   # 8 MiB rotate
KILL_SCAN_EVERY=20                         # journal SIGKILL scan every N cycles (~5min)
export PATH=/usr/bin:/bin:/usr/local/bin

mkdir -p "$BASE"

now()  { date '+%F %T'; }
log()  { printf '%s %s\n' "$(now)" "$*" >> "$LOG"; }

rotate() {
  [ -f "$LOG" ] || return 0
  [ "$(stat -c%s "$LOG")" -lt "$MAXLOG" ] && return 0
  mv "$LOG" "$LOG.1"
  log "=== rotated ==="
}

last_ts() { date '+%Y-%m-%d %H:%M:%S'; }

# --- detailed snapshot when a kill is detected ---
snapshot() {
  local reason="$1" from="$2"
  rotate
  log "===== DETAILED SNAPSHOT ($reason) @ $(now) ====="
  log "uptime: $(uptime -p)  load: $(cut -d' ' -f1-3 /proc/loadavg)"
  { echo "--- free ---";      free -h; }                                         >> "$LOG" 2>&1
  { echo "--- swap ---";      swapon --show; }                                   >> "$LOG" 2>&1
  { echo "--- zram mm_stat (orig_data compr_data mem_used mem_limit mem_used_max) ---"
    awk '{printf "%d %d %d %d %d\n",$1,$2,$3,$4,$5}' /sys/block/zram0/mm_stat; } >> "$LOG" 2>&1
  { echo "--- top 20 RSS ---"; ps -eo pid,user,rss,comm --sort=-rss | head -21; } >> "$LOG" 2>&1
  { echo "--- build/test procs ---"
    ps -eo pid,ppid,rss,etime,args | grep -E 'cargo|rustc|--test-threads|/target/debug/deps' | grep -v grep; } >> "$LOG" 2>&1
  { echo "--- all cgroup oom counters (nonzero only) ---"
    find /sys/fs/cgroup -name memory.events 2>/dev/null | while read -r f; do
      oom=$(awk '/^(oom|oom_kill|oom_group_kill) /{printf "%s=%s ",$1,$2}' "$f" 2>/dev/null)
      [ -n "$oom" ] && [ "$oom" != "oom=0 oom_kill=0 oom_group_kill=0 " ] && echo "$f : $oom"
    done; } >> "$LOG" 2>&1
  { echo "--- journal delta since $from (kill/oom/stop related) ---"
    journalctl --since "$from" --no-pager 2>/dev/null \
      | grep -iE 'systemd\[[0-9]+\].*(kill|stop|deactivat)|oom|Killed process|Killing process|signal 9|sigkill|memory cgroup' \
      | tail -25; } >> "$LOG" 2>&1
  log "===== end snapshot ====="
}

# --- compact heartbeat line (every cycle) ---
heartbeat() {
  read -r mu ma < <(awk '/MemTotal|MemAvailable/{printf "%d ", $2}' /proc/meminfo)
  read -r su st < <(awk '/^SwapTotal|^SwapFree/{printf "%d ", $2}' /proc/meminfo)
  zram_mem=$(awk '{print int($3/1024/1024)}' /sys/block/zram0/mm_stat 2>/dev/null)
  top1=$(ps -eo rss,comm --sort=-rss | awk 'NR==2{printf "%s(%dMB)",$2,int($1/1024)}')
  nbuild=$(ps -eo args | grep -cE 'cargo|rustc|--test-threads' | tr -d ' ')
  log "HB used=$(( (mu-ma)/1024 ))MB avail=$((ma/1024))MB swap_used=$(( (su-st)/1024 ))MB zram_mem=${zram_mem}MB top=${top1} build_procs=${nbuild}"
}

# ===== init =====
rotate
log "=== oom-monitor started (pid $$, interval=${INTERVAL}s, psi=off, oomd=off) ==="

# baseline cgroup oom_kill counters
declare -A prev
while IFS= read -r f; do
  v=$(awk '/^oom_kill/{print $2}' "$f" 2>/dev/null)
  [ -n "$v" ] && prev["$f"]="$v"
done < <(find /sys/fs/cgroup -name memory.events 2>/dev/null)

# baseline build/test procs (pid -> "lstart|cmdline")
declare -A bprocs
scan_procs() {
  local out pid rest lstart args
  # match by executable (comm) for cargo/rustc, or real test invocations:
  # --test-threads=<n> or a binary under /target/debug/deps/
  out=$(ps -eo pid=,lstart=,comm=,args= 2>/dev/null \
    | awk '$7=="cargo"||$7=="rustc"||$0~/--test-threads=[0-9]+/||$0~/\/target\/debug\/deps\//{print}')
  bprocs=()   # clear stale entries (dead pids must disappear from the tracked set)
  [ -z "$out" ] && return 0
  while IFS= read -r line; do
    read -r pid rest <<< "$line"   # handles leading whitespace; pid=first token
    lstart=$(awk '{for(i=1;i<=6&&i<=NF;i++)printf "%s%s",$i,(i<6?" ":"");exit}' <<< "$rest")
    args=$(awk '{for(i=7;i<=NF;i++)printf "%s%s",$i,(i<NF?" ":"");exit}' <<< "$rest")
    [ -n "$pid" ] && bprocs["$pid"]="${lstart}|${args}"
  done <<< "$out"
}
scan_procs

last_ts_k="$(last_ts)"
last_ts_j="$(last_ts)"
cycle=0

trap 'log "=== oom-monitor stopped ==="' EXIT

# ===== main loop =====
while true; do
  cycle=$((cycle+1))

  # 1) new kernel OOM messages since last poll
  new_k=$(journalctl -k --since "$last_ts_k" -o short-iso --no-pager 2>/dev/null)
  last_ts_k="$(last_ts)"
  if printf '%s' "$new_k" | grep -qiE 'Out of memory|oom-kill|Killed process|oom_reaper|memory cgroup out of memory'; then
    rotate
    log "===== KERNEL OOM EVENT ====="
    printf '%s\n' "$new_k" >> "$LOG"
    log "===== end kernel oom ====="
    snapshot "kernel-oom" "$last_ts_k"
  fi

  # 2) cgroup oom_kill counter deltas
  while IFS= read -r f; do
    v=$(awk '/^oom_kill/{print $2}' "$f" 2>/dev/null)
    [ -z "$v" ] && continue
    if [ "${prev[$f]:-0}" -gt 0 ] && [ "$v" -gt "${prev[$f]}" ]; then
      log "CGROUP OOM_KILL: $f now=$v prev=${prev[$f]}"
      snapshot "cgroup-oom-kill:$f" "$last_ts_k"
    fi
    prev["$f"]="$v"
  done < <(find /sys/fs/cgroup -name memory.events 2>/dev/null)

  # 3) build/test process death watcher (catches userspace kill -9 with no record)
  declare -A old
  for k in "${!bprocs[@]}"; do old["$k"]="${bprocs[$k]}"; done
  scan_procs   # repopulates global bprocs with current set
  for pid in "${!old[@]}"; do
    if [ -z "${bprocs[$pid]:-}" ]; then
      was="${old[$pid]}"
      log "BUILD PROC EXIT: pid=$pid was=[$was]"
      # full snapshot for test/deps binaries; rustc/cargo exits get one line only
      # (unless kill/oom hints appear in the journal window)
      case "$was" in
        *rustc*|*cargo*)
          if journalctl --since "$last_ts_j" --no-pager 2>/dev/null \
             | grep -qiE 'Killed process|Killing process|signal SIGKILL|out of memory|oom-kill'; then
            snapshot "build-proc-exit(abnormal?):pid=$pid" "$last_ts_j"
          fi ;;
        *)
          snapshot "build-proc-exit:pid=$pid" "$last_ts_j" ;;
      esac
    fi
  done
  unset old

  # 4) periodic journal SIGKILL / systemd-kill scan (exclude our own unit)
  if [ $((cycle % KILL_SCAN_EVERY)) -eq 0 ]; then
    hits=$(journalctl --since "$last_ts_j" --no-pager 2>/dev/null \
      | grep -iE 'Killing process .* with signal SIGKILL|signal SIGKILL|exit code=.*killed|killed by signal|Sent signal SIGKILL' \
      | grep -v 'oom-monitor' | tail -10)
    last_ts_j="$(last_ts)"
    if [ -n "$hits" ]; then
      rotate
      log "===== SIGKILL RECORDS (journal) ====="
      printf '%s\n' "$hits" >> "$LOG"
      log "===== end sigkill ====="
      snapshot "journal-sigkill" "$last_ts_j"
    fi
  fi

  heartbeat
  sleep "$INTERVAL"
done
```

### 6.2 `setup-audit.sh`

```bash
#!/usr/bin/env bash
# setup-audit.sh — install auditd rules to catch WHO sends SIGKILL (kill -9).
# Run as root:  sudo bash ~/oom-monitor/setup-audit.sh
set -euo pipefail

RULES_FILE=/etc/audit/rules.d/oom-kill.rules
DROPIN=/etc/systemd/system/auditd.service.d/oom-rules.conf

echo "==> writing persistent audit rules: $RULES_FILE"
cat > "$RULES_FILE" <<'EOF'
## oom-monitor: catch every SIGKILL (signal 9) sent via kill/tkill/tgkill/pidfd_send_signal
## NOTE: audit rules have no `sig` field; filter on syscall argument position:
##   kill(pid,sig)=a1  tkill(tid,sig)=a1  tgkill(tgid,tid,sig)=a2  pidfd_send_signal(pidfd,sig,info,flags)=a1
-a always,exit -F arch=b64 -S kill -F a1=9 -k oomkill
-a always,exit -F arch=b64 -S tkill -F a1=9 -k oomkill
-a always,exit -F arch=b64 -S tgkill -F a2=9 -k oomkill
-a always,exit -F arch=b64 -S pidfd_send_signal -F a1=9 -k oomkill
EOF

echo "==> writing systemd drop-in (reload rules on auditd start): $DROPIN"
mkdir -p "$(dirname "$DROPIN")"
cat > "$DROPIN" <<'EOF'
[Service]
ExecStartPost=-/sbin/auditctl -R /etc/audit/rules.d/oom-kill.rules
EOF

echo "==> enabling + starting auditd"
systemctl enable auditd
if ! systemctl is-active --quiet auditd; then
  systemctl start auditd
fi
systemctl is-active auditd

# NOTE: auditd.service has RefuseManualStop=yes, so `systemctl restart` is refused.
# auditd is already running; we load rules directly via auditctl (no restart needed).
# The ExecStartPost drop-in above reloads rules on every future (re)start / boot.

echo "==> loading rules now"
auditctl -D   # clear any current rules (box had none)
auditctl -R "$RULES_FILE"

echo "==> active rules:"
auditctl -l

echo "==> self-test: sending SIGKILL to a throwaway process"
sleep 1000 & T=$!
kill -9 "$T" 2>/dev/null || true
sleep 1
echo "==> ausearch result (key=oomkill, last 5 min):"
ausearch -k oomkill -ts recent -i 2>&1 | tail -40

echo
echo "DONE. To inspect who killed a build/test process later:"
echo "  sudo bash ~/oom-monitor/check-audit.sh"
```

### 6.3 `check-audit.sh`

```bash
#!/usr/bin/env bash
# check-audit.sh — show who sent SIGKILL, last 24h (or since --since).
# Run as root:  sudo bash ~/oom-monitor/check-audit.sh [--since <date>]
set -euo pipefail
SINCE="${2:-today}"
sudo ausearch -k oomkill -ts "$SINCE" -i 2>&1 | tail -100
```
