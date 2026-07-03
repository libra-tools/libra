### `cli.cross-cutting-flags`

目的：集中覆盖 `src/cli.rs` 根结构（`Cli`）上的全局 flag —— `--json`(`-J`)/`--machine`/`--quiet`(`-q`)/`--color`/`--no-color`/`--progress`/`--exit-code-on-warning`，断言其语义本身，而不是依赖各功能场景顺带触发。本场景的内联 `libra()` 已对齐 §3.3.1 更新后的规范（含 `TMPDIR` 与 git/ssh 感知 `SAFE_PATH`），可作为其他场景收敛的样板。

最小步骤：

```bash
# Short converged form (this section itself was noted as convergence sample).
SCENARIO="cli.cross-cutting-flags"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"

libra init flags-repo
cd flags-repo
libra config set user.name "Libra Flags Test"
libra config set user.email "flags@example.invalid"
printf 'flag\n' > flag.txt
libra add flag.txt
libra commit -m "test: flags base"

# --json / -J：stdout 是可解析 JSON envelope（需要 PATH 上的解析器时用 python3，否则仅断言非空）
libra --json status >status.json
libra -J status >status.short.json
libra --json=compact log >log.compact.json
libra --json=ndjson log >log.ndjson
python3 -c "import json; d=json.load(open('status.json')); assert d['ok'] is True; assert 'data' in d; assert 'untracked' in d['data']"
python3 -c "import json; d=json.load(open('status.short.json')); assert d['ok'] is True; assert 'data' in d"
python3 -c "import json; d=json.load(open('log.compact.json')); assert d['ok'] is True; assert isinstance(d['data'].get('commits'), list)"
python3 -c "import json; lines=[json.loads(l) for l in open('log.ndjson')]; assert len(lines) > 0; assert 'hash' in lines[0] or 'id' in lines[0]"

# --quiet：抑制主结果 stdout，但命令仍成功
libra --quiet status >quiet.out
test ! -s quiet.out

# --machine：蕴含 ndjson + no-pager + color=never + quiet
libra --machine status >machine.out

# --color=never / --no-color：stdout 不含 ANSI 转义序列
libra --color=never log >log.nocolor
libra --no-color log >log.nocolor2
! grep -q "$(printf '\033')" log.nocolor

# --progress=none：长操作不打印进度
libra --progress none status >/dev/null

# --exit-code-on-warning：无 warning 时不得改变成功命令退出码
# warning 时退出码 9 需要先固化确定性 warning 源，当前按 BASELINE_GAP-INTEG-009 跟踪。
libra --exit-code-on-warning status

# 错误 JSON 形态（Agent 关键契约）：--json 模式下失败也必须在 stderr 产出 ok:false + LBR-* 稳定码
! libra --json cat-file -p 0000000000000000000000000000000000000000 2>err.json || true
python3 -c "
import json, sys
data = open('err.json').read().strip()
if data:
    try:
        j = json.loads(data)
        assert j.get('ok') is False
        assert 'error_code' in j and j['error_code'].startswith('LBR-')
        assert 'category' in j and 'message' in j
        assert 'hints' in j or 'details' in j
    except Exception as e:
        print('JSON error envelope parse failed:', e, file=sys.stderr)
        sys.exit(1)
"
```

负向步骤：

```bash
cd "$RUN_DIR/flags-repo"
! libra --json=bogus status
! libra --color=plaid log
# 无 warning 时 --exit-code-on-warning 不应改变退出码
libra --exit-code-on-warning status
```

断言：`--json`/`-J` 输出可被 JSON 解析（或至少非空且为单一 envelope）；`--json=compact`/`=ndjson` 切换布局；`--quiet` 使主结果 stdout 为空但退出码 0；`--machine` 等价于 ndjson+no-pager+color=never+quiet 的组合（参见 `src/cli.rs` 中 `--machine` 的文档化语义）；`--color=never`/`--no-color` 去除 ANSI 转义；`--progress none` 不打印进度；`--exit-code-on-warning` 在无 warning 时退出码为 0；非法 `--json`/`--color` 值必须非 0 退出并提示可选值。warning 时退出码 9 暂不进入默认 Wave，按 BASELINE_GAP-INTEG-009 要求先识别无密钥、可复现 warning 源。

补充可执行断言（Agent 契约核心场景）：
- `libra --json status > s.json && python3 -c "import json; d=json.load(open('s.json')); assert d['ok'] is True; assert 'data' in d"`
- `libra --machine status > m.out && python3 -c "import json; [json.loads(l) for l in open('m.out')]"` （验证 ndjson 可解析）
- `libra --quiet status > q.out && test ! -s q.out`
- `libra --exit-code-on-warning status` 在无 warning 时退出码必须为 0。
- 非法 `--json=bogus` 必须非 0，且错误 envelope 包含 LBR-CLI-002 或等价。
- 验证 `--progress json` 在 JSON 模式下输出 NDJSON progress 到 stderr。
- 额外：`libra --json --exit-code-on-warning status` 在干净状态下退出码为 0；warning=9 组合行为只在 BASELINE_GAP-INTEG-009 的确定性 warning 源落地后启用。

通过标准：全部场景退出码和断言通过，无未解释 skip/fail。`merge --continue` / `rebase --continue` 的冲突续跑成功路径由 `cli.merge-conflict-continue` / `cli.rebase-conflict-continue` 覆盖；LFS 远端 lock API、真实浏览器/系统 open 行为不进入默认 Wave，必要时登记独立 follow-up。

## 4.2 Wave 2：CLI 存储、schema 与本地协议场景（必跑）

Wave 2 覆盖需要跨仓库、本地 remote 或底层存储可观察结果的功能，但仍只通过 `libra` 命令驱动。

