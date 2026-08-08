# 上游 Git 测试语料的族级范围裁定

本文是 [`plan-20260729.md`](../plan/plan-20260729.md) 任务卡 **CT0-02** 的产物，为 `plan-long.md` 的 **CT-01**（上游 Git 套件驱动的兼容性证据账本）确定「哪些上游测试族进入迁移范围」。裁定只覆盖**范围边界**；迁移形态与净室边界见 [`grit-gap.md`](grit-gap.md) 的决策日志与 `:93`、`:413`，本文不复制其正文。

**语料基线**：`GitButler/grit@dfb079967b9cbc99e533c21e65f674bb3f5e8b07`（2026-07-29 核对）。统计口径固定为**与上游 `git/t/` 同名的文件**（共 1,041 个），不含 Grit 自撰的 565 个文件——后者无上游 provenance，不作为兼容性证据来源。

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

**2026-07-29 实跑结果**（后续复核须复现同一张表）：

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
| **合计** | **1041** | 311 | 162 | 149 | 23 | 66 |

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

**wave 顺序**（`in-scope` 族，按「`.git/` + `test-tool` 占比之和」升序）：t8 → t4 → t3 → t6 → t2 → t7。`plan-20260729.md` 的首个 wave 取 t4（文件基数最大且阻塞比第二低，单位投入的覆盖收益最高）；t8 只有 16 个上游文件，作为验证流水线的补充切片。

**`deferred` 族的重启条件**：

- **t5**：`.git/` 与 `test-tool` 两项占比都需先由 Libra 侧能力变化压到阈值以下——具体是 ref 存储的可观测面（`update-ref` / `show-ref` / `for-each-ref` 能替代直接 poke `.git/refs`）与 `test-tool` 的 verb 替代面（见 `D22`）。任一前置落地后重新跑上表复算并重判。
- **t0 / t1**：同上，主要卡在 `test-tool`（`D22`）；`.git/` 部分对 t1 也超阈值。这两族是 plumbing 的核心，重启优先级高于 t5。

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
- 本文的裁定与 `plan-long.md` CT-01 的 S4 段一致：t9 出局、t5 延后，且 wave 顺序以「族级阻塞比」为准。
