### `cli.open-smoke`

目的：覆盖 `open` 命令的最小可观察行为，但避免默认 Wave 在 CI/headless 环境中真的打开浏览器或系统应用。

最小步骤：

```bash
SCENARIO="cli.open-smoke"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"
# Short converged (prelude)
libra init open-repo
cd open-repo
libra remote add origin git@github.com:example/open-repo.git
libra --json open >open-default.json
libra --json open origin >open-origin.json
libra --json open -b main origin >open-branch.json
libra open --print-only origin >open-print-only.txt
libra --json open --pr=123 --print-only origin >open-pr-id.json
libra --json open -p origin >open-pr-list.json
libra config open.platform gitlab
libra --json open -c a1b2c3d origin >open-gitlab-commit.json
libra config open.platform custom
libra config open.template.issue "{base_url}/tickets/{issue}"
libra --json open --issue=42 origin >open-custom-issue.json
python3 -c "import json; d=json.load(open('open-default.json')); assert d['ok'] is True; assert d['data']['remote'] == 'origin'; assert d['data']['web_url'] == 'https://github.com/example/open-repo'; assert d['data']['target_type'] == 'repo'; assert d['data']['platform'] == 'github'; assert d['data']['launched'] is False"
python3 -c "import json; d=json.load(open('open-origin.json')); assert d['ok'] is True; assert d['data']['remote'] == 'origin'; assert d['data']['web_url'] == 'https://github.com/example/open-repo'; assert d['data']['target_type'] == 'repo'; assert d['data']['platform'] == 'github'; assert d['data']['launched'] is False"
python3 -c "import json; d=json.load(open('open-branch.json')); assert d['ok'] is True; assert d['data']['web_url'] == 'https://github.com/example/open-repo/tree/main'; assert d['data']['target_type'] == 'branch'; assert d['data']['platform'] == 'github'; assert d['data']['launched'] is False"
python3 -c "s=open('open-print-only.txt').read().strip(); assert s == 'https://github.com/example/open-repo', s; assert 'Opening' not in s"
python3 -c "import json; d=json.load(open('open-pr-id.json')); assert d['ok'] is True; assert d['data']['web_url'] == 'https://github.com/example/open-repo/pull/123'; assert d['data']['target_type'] == 'pull_request'; assert d['data']['platform'] == 'github'; assert d['data']['launched'] is False"
python3 -c "import json; d=json.load(open('open-pr-list.json')); assert d['ok'] is True; assert d['data']['web_url'] == 'https://github.com/example/open-repo/pulls'; assert d['data']['target_type'] == 'pull_request'; assert d['data']['platform'] == 'github'; assert d['data']['launched'] is False"
python3 -c "import json; d=json.load(open('open-gitlab-commit.json')); assert d['ok'] is True; assert d['data']['web_url'] == 'https://github.com/example/open-repo/-/commit/a1b2c3d'; assert d['data']['target_type'] == 'commit'; assert d['data']['platform'] == 'gitlab'; assert d['data']['launched'] is False"
python3 -c "import json; d=json.load(open('open-custom-issue.json')); assert d['ok'] is True; assert d['data']['web_url'] == 'https://github.com/example/open-repo/tickets/42'; assert d['data']['target_type'] == 'issue'; assert d['data']['platform'] == 'custom'; assert d['data']['launched'] is False"
libra fsck --connectivity-only
```

负向步骤：

```bash
cd "$RUN_DIR/open-repo"
! libra --json open no-such-remote
```

断言：全局 `--json` 模式输出包含 `remote`、`remote_url`、`web_url`、`target_type`、`platform` 和 `launched=false`，不启动外部程序；指定 remote 可解析托管页面 URL；deep-link 分支页、GitLab 平台覆盖、自定义 issue 模板均生成精确 URL；缺失 remote 或不安全 URL 必须以 JSON error envelope 失败。默认 Wave 严禁运行会真实启动浏览器/系统应用的裸 `libra open`。

补充可执行断言：
- 已有 JSON 断言保持；额外验证 `libra --json open no-such-remote` 的错误 envelope 包含 `ok:false` + `LBR-CLI-003`。
- 验证所有正向路径都在 `--json` 下返回 `launched=false`，不触发浏览器启动。
- 操作后 `libra fsck --connectivity-only` 通过。
