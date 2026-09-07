# 上游 Git 测试语料的族级范围裁定

本文是 [`plan-20260729.md`](../plan/plan-20260729.md) 任务卡 **CT0-02** 的产物，为 `plan-long.md` 的 **CT-01**（上游 Git 套件驱动的兼容性证据账本）确定「哪些上游测试族进入迁移范围」。裁定只覆盖**范围边界**；迁移形态与净室边界见 [`grit-gap.md`](grit-gap.md) 的决策日志与 `:93`、`:413`，本文不复制其正文。

**语料基线**：`GitButler/grit@dfb079967b9cbc99e533c21e65f674bb3f5e8b07`（2026-07-29 核对；**2026-08-27 复核：HEAD 未移动，基线仍有效**）。统计口径固定为**与上游 `git/t/` 同名的文件**（共 1,041 个），不含 Grit 自撰的 565 个文件——后者无上游 provenance，不作为兼容性证据来源。

**本次新增 3 条 D**（`D21`–`D23`），见「D 编号绑定」。

## 判定命令

下列命令可直接复制执行，产出「逐族裁定」表的全部数字。工作目录为 grit 仓库的 `tests/`。

```sh
set -eu
G=${GRIT_REPO:-/Volumes/Data/competition/GitButler/grit}
[ "$(git -C "$G" rev-parse HEAD)" = dfb079967b9cbc99e533c21e65f674bb3f5e8b07 ] \
  || { echo "FAIL: grit HEAD != pin" >&2; exit 1; }
cd "$G/tests"

# 判定纪律（FIX-01）：中间文件落 run-scoped 目录，三个 trap 保证清理。固定 `/tmp`
# 路径可被预置符号链接劫持，异常退出还会留下文件。
GD=$(mktemp -d)
gd_cleanup() {
  r=$?; [ -n "${1:-}" ] && r=$1
  trap - EXIT INT TERM
  set +e
  if ! rm -rf "$GD"; then
    echo "FATAL: cannot remove $GD — intermediate files left on disk" >&2
    if [ "$r" -eq 0 ]; then r=3; fi
  fi
  exit "$r"
}
trap 'gd_cleanup' EXIT; trap 'gd_cleanup 130' INT; trap 'gd_cleanup 143' TERM

EXT='lib-httpd\.sh|lib-git-daemon\.sh|lib-git-p4\.sh|lib-git-svn\.sh|lib-cvs\.sh|lib-gitweb\.sh|(^|[^-[:alnum:]_])git[[:space:]]+(p4|svn|daemon|cvsimport|cvsexportcommit|cvsserver)([[:space:]]|$)'
TT='(^|[^-[:alnum:]_])test-tool([[:space:]]|$)'
GITD='\.git/|\$GIT_DIR/|--git-path'
GPG='lib-gpg\.sh|GPG'
SUB='(^|[^-[:alnum:]_])git[[:space:]]+submodule([[:space:]]|$)'

printf 'fam\ttotal\tgitdir\ttesttool\text\tgpg\tsubmodule\n'
for n in 0 1 2 3 4 5 6 7 8 9; do
  files=""
  for f in t${n}[0-9][0-9][0-9]-*.sh; do
    [ -e "$G/git/t/$f" ] && files="$files $f"        # 只保留与上游同名的文件
  done
  [ -n "$files" ] || { printf '%s\t0\t0\t0\t0\t0\t0\n' "t$n"; continue; }
  tot=$(printf '%s\n' $files | wc -l | tr -d ' ')
  # 三态分流：rc=0 命中、rc=1 零命中、rc>1 是执行错误（不可读文件、正则错误…），
  # 后者必须以原退出码失败，绝不降级为「零命中」。
  c(){
    set +e
    command grep -lE "$1" $files > "$GD/grep.out" 2>"$GD/grep.err"
    rc=$?
    set -e
    case "$rc" in
      0) wc -l < "$GD/grep.out" | tr -d ' ' ;;
      1) echo 0 ;;
      *) echo "FAIL: grep exited $rc while matching '$1'" >&2
         # 把 grep 自己的诊断转出去：trap 随后会删掉 $GD，不转就永远看不到了。
         if [ -s "$GD/grep.err" ]; then cat "$GD/grep.err" >&2; fi
         exit "$rc" ;;
    esac
  }
  # 每列先赋值再打印：`$(c …)` 在子 shell 里跑，`exit` 只结束子 shell；简单赋值把
  # 它的退出码交给 `set -e`，rc>1 才真的中止整个脚本。
  n_gitdir=$(c "$GITD")
  n_tt=$(c "$TT")
  n_ext=$(c "$EXT")
  n_gpg=$(c "$GPG")
  n_sub=$(c "$SUB")
  printf 't%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$n" "$tot" "$n_gitdir" "$n_tt" "$n_ext" "$n_gpg" "$n_sub"
done
```

必须用 `command grep`（或把命令写入脚本后 `sh script.sh`）执行，避免交互 shell 的 `grep` 别名/函数包装改变结果。

**2026-07-29 实跑结果**（2026-08-27 按上方判定命令原样复跑，十族七列输出逐字复现；后续复核须复现同一张表）：

| 族 | 上游文件数 | 引用 `.git/` | 需 `test-tool` | 需外部服务/外部 VCS | 需 GPG | 需 submodule |
|---|---:|---:|---:|---:|---:|---:|
| t0 | 80 | 24 | 32 | 2 | 0 | 1 |
| t1 | 94 | 38 | 21 | 0 | 1 | 1 |
| t2 | 63 | 23 | 6 | 0 | 0 | 4 |
| t3 | 126 | 37 | 12 | 0 | 2 | 5 |
| t4 | 148 | 15 | 12 | 0 | 1 | 6 |
| t5 | 172 | 79 | 41 | 31 | 5 | 8 |
| t6 | 92 | 28 | 9 | 0 | 3 | 4 |
| t7 | 113 | 43 | 19 | 0 | 8 | 33 |
| t8 | 16 | 2 | 0 | 0 | 0 | 0 |
| t9 | 137 | 24 | 10 | 116 | 3 | 4 |
| **合计** | **1041** | 313 | 162 | 149 | 23 | 66 |

## 逐族裁定

三态取值：`in-scope`（进入 CT-01 的 wave 排期）/ `deferred`（暂不排期，写明重启条件）/ `out-of-scope`（不进入本轮，绑定 `D` 编号）。

判定规则（可复算，直接读上表）：

- **`out-of-scope`**：族内「需外部服务/外部 VCS」占比 > 50%。
- **`deferred`**：族内「引用 `.git/`」占比 ≥ 40% **或**「需 `test-tool`」占比 ≥ 20%，且未命中 `out-of-scope`。
- **`in-scope`**：其余。

| 族 | 主题 | 裁定 | 判据（按上表复算） | 绑定 `D` |
|---|---|---|---|---|
| t0 | plumbing 基础 | `deferred` | `test-tool` 32/80 = 40.0% ≥ 20% | — |
| t1 | 索引 / ref plumbing | `deferred` | `.git/` 38/94 = 40.4% ≥ 40%；`test-tool` 21/94 = 22.3% ≥ 20% | — |
| t2 | index / checkout | `in-scope` | `.git/` 23/63 = 36.5% < 40%；`test-tool` 6/63 = 9.5% < 20% | — |
| t3 | 核心命令 | `in-scope` | `.git/` 37/126 = 29.4%；`test-tool` 12/126 = 9.5% | — |
| t4 | diff / format-patch | `in-scope`（**首个 wave**） | `.git/` 15/148 = 10.1%；`test-tool` 12/148 = 8.1%——两项均为全族最低 | — |
| t5 | 传输 / pack / 协议 | `deferred` | `.git/` 79/172 = 45.9% ≥ 40%；`test-tool` 41/172 = 23.8% ≥ 20% | — |
| t6 | rev machinery | `in-scope` | `.git/` 28/92 = 30.4%；`test-tool` 9/92 = 9.8% | — |
| t7 | porcelain | `in-scope` | `.git/` 43/113 = 38.1% < 40%；`test-tool` 19/113 = 16.8% < 20% | — |
| t8 | blame | `in-scope` | `.git/` 2/16 = 12.5%；`test-tool` 0/16 = 0% | — |
| t9 | 外部工具 / 桥接 | `out-of-scope` | 外部服务/外部 VCS 116/137 = 84.7% > 50% | `D21` |

**wave 顺序**（`in-scope` 族，按「`.git/` + `test-tool` 占比之和」升序）：t8 → t4 → t3 → t6 → t2 → t7。`plan-20260729.md` 的首个 wave 取 t4（文件基数最大且阻塞比第二低，单位投入的覆盖收益最高）；t8 只有 16 个上游文件，作为验证流水线的补充切片。**（2026-08-27 复核）** t4 wave 已落地（`tests/command/t4_port_test.rs`、`tests/compat-ledger/t4/`），随 `plan-20260729.md` 的 CT3-* 与 CT4-01 发布；t8 及其余 `in-scope` 族尚未排期，归 `plan-long.md` CT-01 剩余的 S4 族 waves。

**`deferred` 族的重启条件**：

- **t5**：`.git/` 与 `test-tool` 两项占比都需先由 Libra 侧能力变化压到阈值以下——具体是 ref 存储的可观测面（`update-ref` / `show-ref` / `for-each-ref` 能替代直接 poke `.git/refs`）与 `test-tool` 的 verb 替代面（见 `D22`）。任一前置落地后重新跑上表复算并重判。
  - **（2026-08-27 待复核：重启动作的口径）** 上表七列是 **pinned 语料的纯函数**——判定命令 grep 的是 `GRIT_REPO/tests/*.sh` 的文本，Libra 侧任何能力变化都不会改变其中任何一个数值，只有 grit pin 移动才会。因此「重新跑上表复算并重判」在语料 pin 固定时不构成可触发的重启动作。三条 ref 命令 Libra 本身早在本文成稿前即已具备（`src/cli.rs:591` `update-ref` / `:472` `show-ref` / `:492` `for-each-ref`，另有 `:501` `symbolic-ref`；最晚一条落于 2026-06-30），故前置的实质是「这些命令能否在迁移后的用例中**替代**直接 poke `.git/refs`」这一语义问题，而非命令存在与否——该语义前置是否已满足本次未判定。族级 `deferred` 裁定本身不受影响（t5 的两项占比确实超阈值），但重启动作须改口径：或按「族内因该前置而不再构成阻塞的文件」收窄阻塞计数，或改用其它判据，须在推进 `plan-long.md` CT-01 剩余 S4 族 wave 前确定。
- **t0 / t1**：同上（含上条的待复核注记），主要卡在 `test-tool`（`D22`）；`.git/` 部分对 t1 也超阈值。这两族是 plumbing 的核心，重启优先级高于 t5。

`deferred` 不绑定 `D` 编号——它们是**暂缓**而非**拒绝**，语料本身没有被排除出兼容治理范围。

## D 编号绑定

`out-of-scope` 的每条原因都必须绑定一个 `D` 编号，供 `tests/compat-ledger/` 的 `declined` 行解析。既有编号复用，新原因自 `D21` 起（不复用 `D11`–`D14` 空档）。

| 排除原因 | 绑定 | 说明 |
|---|---|---|
| 外部 VCS / 服务桥接（svn、p4、cvs、gitweb、httpd、git-daemon） | **`D21`**（新增） | 覆盖 t9 整族的裁定依据 |
| 上游 `test-tool` C helper 依赖 | **`D22`**（新增） | 跨族出现（162 个上游文件）；同时是 t0/t1/t5 的 `deferred` 依据 |
| GPG keyring fixture 依赖 | **`D23`**（新增） | 跨族出现（23 个上游文件） |
| `submodule` 相关场景 | `D1`（既有） | 66 个上游文件；`COMPATIBILITY.md` 已列 `unsupported` |
| `sparse-checkout` / `clone --sparse` 场景 | `D10`（既有） | `COMPATIBILITY.md` 已列 `unsupported` |
| `send-email` 场景 | `D19`（既有） | `COMPATIBILITY.md` 已列 `unsupported` |
| 交互式 patch mode / `rebase -i` 场景 | `D15` / `D16`（既有） | 跨族出现于 t2/t3 的交互路径 |

新增的 `D21`–`D23` 条目正文见 [`../commands/_compatibility.md`](../commands/_compatibility.md) 的「拒绝与延后决策」节。

## 与其它文档的关系

- 迁移形态、净室边界与 `GGT-00A` 的机制细节：[`grit-gap.md`](grit-gap.md)（本文只引用，不复制）。
- 阶段划分、wave 准入/准出与账本 schema：[`../plan/plan-20260729.md`](../plan/plan-20260729.md) 与 `plan-long.md` 的 CT-01 节。
- 本文的裁定与 `plan-long.md` CT-01 的 S4 段一致：t9 出局、t5 延后，且 wave 顺序以「族级阻塞比」为准。（2026-08-27 待复核：[`../plan/plan-long.md`](../plan/plan-long.md) 的 S4 行本身只写「逐族 clean-room wave……t4 首个 wave」，未逐字复述 t9 / t5 裁定与「族级阻塞比」措辞，故本句的一致性属语义层面而非逐字锚；该措辞源自 CT0-02 的原始验收条目，本次不回改。）

## 刷新记录

### 2026-08-27（第一次刷新）

**校验基线**：竞品 checkout `/Volumes/Data/competition/GitButler/grit`，`git rev-parse HEAD` = `dfb079967b9cbc99e533c21e65f674bb3f5e8b07`；Libra `main` HEAD `89081277a`、`Cargo.toml` `version = "0.21.27"`。

**漂移量：竞品侧为零。** grit HEAD 与 2026-07-29 语料基线逐字一致（[`../plan/plan-long.md`](../plan/plan-long.md) 的第九次竞品审计快照亦记 grit「已是最新」）。「判定命令」小节按原文落盘复跑，十族七列输出与上表逐行逐字相同；两项口径统计同样复现——与上游 `git/t/` 同名的文件 1,041 个，Grit 自撰的 565 个（`tests/` 下不与上游同名的 `t*.sh` 共 568 个，扣除 `test-lib-commit-bulk.sh` / `test-lib-harness.sh` / `test-lib-tap.sh` 三个 harness 库）。三态判定规则与 wave 顺序全部复算通过，零漂移。

**本次修正要点：**

1. **合计行的「引用 `.git/`」列由 `311` 订正为 `313`**（十族之和 24+38+23+37+15+79+28+43+2+24）。这是笔误订正，不涉及任何裁定改判——其余五列合计（1041 / 162 / 149 / 23 / 66）同样无需联动：[`../commands/_compatibility.md`](../commands/_compatibility.md) 的 `D21`（116/137 = 84.7%，`:233`）、`D22`（162 = 15.6%，`:240`）、`D23`（23，`:247`）三处引用数均正确；`D1` 的 `66` 则只见于本文下方「D 编号绑定」表（`_compatibility.md` 的 `D1` 节只写产品边界与重启条件，不复述文件数量）。
2. **语料基线括注与实跑结果标题**补记 2026-08-27 复核事实，revision 本身不动。
3. **wave 顺序段**补记 t4 wave 的落地位置与 t8 未排期的现状。
4. **`deferred` 重启条件（t5，t0 / t1 同上）**标为「待复核」：上表七列是 pinned 语料的纯函数，Libra 侧能力变化不改变其数值；三条 ref 命令（`src/cli.rs:591` / `:472` / `:492`）在本文成稿前即已存在。族级裁定不变，重启动作的口径待定。
5. **「与其它文档的关系」末条**标为「待复核」：`plan-long.md` 的 S4 行未逐字复述 t9 / t5 裁定，一致性属语义层面。

**复核订正（同日，对抗式复核判 PASS 后收口的三条 P2）：** 三条均为出处/措辞精度，不涉及任何裁定、编号或统计口径的改判，`D21`–`D23` 与十族七列表保持原状。

6. **上列第 1 点的出处订正**：`D1` 的 `66` 并不在 `_compatibility.md`（`grep -n '66' docs/development/commands/_compatibility.md` 零命中，其 `D1` 节仅四行、不复述文件数量），该数字只存在于本文的「D 编号绑定」表；已改为只把 `D21` / `D22` / `D23` 三处引用数归给 `_compatibility.md`（`:233` / `:240` / `:247`）。「66 正确、无需联动」的实质结论不变。
7. **「计划治理对齐」段消歧**：原句「全部任务卡已完成」与同段随后的「`CT3-07` 转 `blocked`」字面冲突；已补上例外口径——该「全部完成」是 `plan-20260729.md:148` 在 `CT3-07` 退出 REL-01 固定成员集之后的记账口径（`CT3-07` 卡 `Lifecycle / Acceptance` 实测为 `blocked` / 空）。
8. **「`D21`–`D23` 状态」段去全称化**：「其 `blocked` 行」改为「其中 86 条 `blocked` 行」。实测 `tests/compat-ledger/t4/*.toml` 按场景解析为 86 条 `blocked` 带 `ADR-CT-06 rule 3` + 15 条 `blocked` 走其它理由 + 77 条 `direct` 带 `rule 5`；核心断言（178 条 = 77 `direct` + 101 `blocked` + 0 `declined`，`decision_id` 零次出现）不变。

本轮未新增待复核项，下方「仍待复核项」清单原样保留。

**Libra 侧核对结果（全部仍成立，不改动）：** `D` 编号绑定表的五条外部依据未漂移——`submodule` / `sparse-checkout` / `send-email` 在 `COMPATIBILITY.md:259-261` 仍列 `unsupported`（`sparse-view` 是明确划出的只读补集，materializing 的 `sparse-checkout` 仍属 `D10`）；`D15`（跨命令 patch mode）与 `D16`（交互式 rebase）在 [`../commands/_compatibility.md`](../commands/_compatibility.md)`:180`/`:186` 仍为「拒绝」；`src/cli.rs::Commands` 仍无 `svn` / `p4` / `cvs*` / `daemon` / `gitweb` variant（`D21` 成立）。[`grit-gap.md`](grit-gap.md) 的 `:93`（迁移形态）与 `:413`（GPLv2→MIT 净室边界）两处行锚未漂移。

**`D21`–`D23` 状态：维持 `out-of-scope`，不改判、不重编。** 三条编号已交付并可被账本解析（`tests/compat-ledger/README.md` 要求每条 `declined` 绑定一个可解析的 `### D<n>` 标题），但截至本次刷新尚无账本行引用——t4 账本共 178 条场景，77 条 `direct` + 101 条 `blocked` + 0 条 `declined`，`decision_id` 字段零次出现；其中 86 条 `blocked` 行显式记「尚无 `D` 编号覆盖，按 ADR-CT-06 规则 3 处理而非 `declined`」（余 15 条 `blocked` 走「Libra 未文档化该 flag，不可作为受支持面引用」等其它理由，同样不记 `declined`）。

**计划治理对齐：** [`../plan/plan-20260729.md`](../plan/plan-20260729.md) 除已正式延后、并已退出 REL-01 固定成员集的 `CT3-07` 外，全部任务卡已完成（`plan-20260729.md:148` 的收口句「至此 plan-20260729 全部任务卡完成」即按此口径；本文的产出卡 **CT0-02** 为 `done` / `complete`，随 **CT4-01** 聚合收口发布 v0.21.21 推进），但 `plan-long.md` 的 **CT-01** 仍为「实施中」——剩余 S4 族 waves 与 S2 离线发现器（前置 DEP-01 + SB-04）。`plan-20260729.md` 的 `CT3-07` 转 `blocked` 并登记为 `DEFER-09`，现由 [`../plan/plan-20260825.md`](../plan/plan-20260825.md) 的 `TA-01`..`TA-07` 承接（截至本次刷新 `TA-01` / `TA-02` / `TA-04` 为 `done` / `locally-accepted`，`TA-03` 仍 `pending`，而 `DEFER-09` 的关闭句由 `TA-03` 负责写入，故该延后仍开着）。该轴是测试串行标注与并行度治理，与上游语料的族级裁定正交，不影响本文任何裁定。

**仍待复核项：**

- `deferred` 重启动作的口径（见「逐族裁定」节 t5 条目的注记）——`plan-20260729.md` 的 CT0-02 验收条目只要求「`t5` 标 `deferred` 并写明重启条件」，未约束该条件的可触发性，故不替原作者定论。
- 本文与 `plan-long.md` CT-01 S4 段的一致性口径（见「与其它文档的关系」末条注记）。
- 跨文档锚点漂移（**不在本文写集内，只登记不改**）：`plan-20260729.md` 的七列基线门注释把本文表头 `printf` 标为 `grit-suite-scope.md:26`，实际在 `:41`。该门内联了表头字面量、不消费该行号，故不影响门的执行。
