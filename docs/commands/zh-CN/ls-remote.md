# `libra ls-remote`

列出远程仓库通告的引用，不下载对象，也不更新本地引用。

```bash
libra ls-remote [OPTIONS] <repository> [patterns...]
```

在 Libra 仓库内运行时，`<repository>` 可以是已配置的远程名称，也可以是 URL，或本地 Git/Libra 仓库路径。

## 选项

| 标志 | 说明 | 示例 |
|------|-------------|---------|
| `--heads` | 只显示 `refs/heads/*` 分支引用 | `libra ls-remote --heads origin` |
| `-t`, `--tags` | 只显示 `refs/tags/*` 标签引用 | `libra ls-remote --tags origin` |
| `--refs` | 省略 `HEAD` 和以 `^{}` 结尾的 peeled 标签引用 | `libra ls-remote --refs origin` |
| `--symref` | 在对应 OID 行之前打印 symbolic-ref 目标（如 `ref: refs/heads/main\tHEAD`）。远端通告的 `symref=` capability 优先；缺失 capability 时（尤其本地 Libra 源），使用与 fetch 相同的 HEAD OID / 分支 tip 解析器合成 `HEAD`。 | `libra ls-remote --symref origin` |
| `patterns...` | 匹配完整引用名或尾部路径组件；`*` 和 `?` 遵循 Git 风格 glob 行为，并且可以匹配 `/` | `libra ls-remote origin main 'refs/heads/*'` |

## 人类可读输出

每个匹配引用按如下格式打印：

```text
<object-id>	<refname>
```

示例：

```text
4f3c2d1a...	HEAD
4f3c2d1a...	refs/heads/main
```

## JSON 输出

使用 `--json` 时，输出使用标准命令信封：

```json
{
  "ok": true,
  "command": "ls-remote",
  "data": {
    "remote": "origin",
    "url": "https://example.com/repo.git",
    "heads_only": false,
    "tags_only": false,
    "refs_only": false,
    "patterns": [],
    "entries": [
      {
        "hash": "4f3c2d1a...",
        "refname": "refs/heads/main"
      }
    ]
  }
}
```

## 示例

```bash
# 列出具名远程的所有引用
libra ls-remote origin

# 直接列出 URL 的所有引用（不需要注册远程）
libra ls-remote https://example.com/repo.git

# 限制为匹配模式的分支
libra ls-remote --heads origin main

# 面向代理的结构化 JSON 信封，仅标签
libra --json ls-remote --tags origin
```

`libra ls-remote --help` 会渲染同一横幅，因此文档和 CLI 表面保持同步（跨命令 `--help` EXAMPLES 推出，见 `docs/development/commands/_general.md` 条目 B）。

## 说明

- `ls-remote` 只执行协议发现（对本地 Git 仓库等价于 `git-upload-pack --advertise-refs`）。
- 它不会写入对象、远程跟踪引用、配置或工作树文件。
- `--heads` 和 `--tags` 可以组合使用，以同时显示分支和标签引用，同时排除 `HEAD`。

## 畸形 HTTP(S) discovery 响应

在 HTTP(S) 引用发现（discovery）期间，Libra 会拒绝零字节广告和畸形
pkt-line 帧，包括不完整或非十六进制标头、小于四的帧长度以及截断的 payload。
合法的 `0000` flush 与未收到响应有明确区别；合法的空仓库广告仍受支持。
不支持的 object-format capability 使用固定错误消息
`Unsupported object format capability`，不回显远端提供的值。
请确认 URL 指向 Git smart HTTP 服务，并检查代理是否截断或替换了响应，然后重试。

## pkt-line 错误归类

检测到的 pkt-line 帧格式错误返回 `LBR-NET-002`（退出码128），包括空的 HTTP(S)
discovery 广告。普通连接失败、连接重置和超时返回 `LBR-NET-001`（退出码128）。
协议错误发生时请核对 Git 服务及代理响应。discovery 帧错误的提示为
`check that the remote serves Git data and that a proxy has not altered the response`。

此归类用于引用 discovery；SSH 认证及主机信任错误见下节，本地配置读取错误保持原有码。

## SSH 广告错误处理

SSH advertisement 长度 `0001` 至 `0003`、不完整标头（包括零字节 EOF）及截断
payload 返回 `LBR-NET-002`。固定协议原因与 marker 保留，不插入捕获的 SSH
stdout/stderr。

必需标头不完整时有一项主机信任例外：本地 SSH 退出码为255，且 stderr 前64 KiB
包含受识别的 host-key 诊断时，返回固定主机核验指引与 `LBR-NET-001`。这项分类
本身不验证远端指纹。其它缺失广告（含认证失败）仍用 `LBR-NET-002`；能够观察到
非零本地退出状态时，追加 `SSH exited with status N` 与固定连接、可信主机、
ssh-agent 及仓库访问指引，不显示原始 SSH 诊断。

必需标头不完整时最多用100毫秒观察 SSH 退出状态，再按需请求终止；其它读取
错误立即请求终止。状态观察、直接子程序回收及输出收集共用两秒清理截止时间。
协议错误与带类型的主机信任错误优先于次要清理警告。普通 IO/超时保留传输错误
分类，可追加固定本地清理警告。终止程序可能改变观察到的退出状态；这不承诺
回收任意后代程序。

Clone 将主机核验指引放在结构化 hints 中；其它命令边界在 message 中保留固定
主机指引，并沿用 `LBR-NET-001` 的网络 hint。human、JSON 与 machine 诊断均
不包含捕获的远端 stderr 原文。

`git://` 取对象阶段已将上述帧错误归为 `LBR-NET-002`；Git discovery 与异步
非 ASCII/非 hex 标头分类仍由后继处理，HTTP(S) 帧行为不变。

## SSH 认证与捕获诊断

无论是否由终端调用，Libra 都以 `BatchMode=yes` 启动 SSH，不在 Libra 命令中
询问私钥口令或进行交互式主机信任决定。重试前请先在 `ssh-agent` 中加载或解锁
加密私钥。主机信任应先通过可信服务商控制台或其它可信渠道核对指纹，再手动
更新 `~/.ssh/known_hosts`；也可以单独建立交互 SSH 连接，核对显示的指纹后才
接受。`ssh -T git@github.com` 是 GitHub 示例，请使用实际仓库 SSH 用户、主机
和端口，不要接受未经核验的指纹。

`ssh.strictHostKeyChecking` 保留既有 `ask`、`yes`、`accept-new`、`no` 设置。
`ask` 不向 SSH 传递该选项，由用户 SSH 配置决定；`BatchMode=yes` 仍禁止
交互决定。显式设置会转交 SSH，请按仓库需求选择主机信任策略。

SSH stderr 在终端会话中也始终捕获，从子程序启动时便持续读取，最多保留64 KiB，
其余字节继续计数并计算摘要。用户错误只含固定文字与可用的本地退出状态，不
打印或记录远端 stderr 原文。debug 诊断只含退出状态、总字节数、保留字节数及
已收集字节流的 SHA-256；收集失败或取消时可能没有这些元数据，不声称已有完整
摘要。摘要计算的工作量与实际读取字节数成正比。

SSH 引用广告与 receive-pack 响应各有16 MiB累计上限。广告超限以 `LBR-NET-001`
失败并提示在服务可用时使用仓库的 HTTPS URL，否则请维护者减少引用；push 响应超限以 `LBR-NET-001` 失败并提示减少推送
引用，绝不把截断响应视为成功。极大的引用集或更新可能受到影响；流式 fetch
pack 不受该上限约束。push 响应失败不代表服务端回滚了引用，重试前应检查远端
实际状态。既有 IO 超时仍然生效。

完整 discovery 广告之后，Libra 最多给 SSH 100毫秒退出，再请求终止；共用两秒
清理截止时间。捕获任务在所属操作退出或截止时间到期时取消，也覆盖后代程序
继续持有管道的情况。

### SSH 主机身份变更与诊断收集

SSH 报告主机身份已经变更时，Libra 保留独立的固定警告：可能发生拦截，也可能是合法密钥轮换。必须先通过可信渠道核对新指纹，才可替换 `~/.ssh/known_hosts` 中的旧条目；不要绕过主机密钥检查。此情况与首次未信任主机均使用 `LBR-NET-001`，但消息与操作提示不同。

如果 stderr 管道在有界收集期限后仍未关闭，已取得的完整协议输出及本地退出状态仍可使用；不会仅因诊断收集失败而拒绝完整传输。已观察到的非零退出状态及主要读取错误仍会导致失败。缺失的诊断仅记录固定的 debug 提示，不伪造空流字节数或摘要；stdout 收集或进程等待失败仍按原错误处理。

### SSH 上限与主机分类边界

上述16 MiB广告与receive-pack响应累计上限仅适用于Libra的SSH传输。
HTTPS和Git传输没有这一特定上限。若服务器提供HTTPS端点，SSH广告超限时可改用
该仓库的HTTPS远端URL；只读用户无需修改服务器引用。否则请仓库维护者减少广告中的
引用集合。流式fetch pack仍不受此累计上限约束。

主机信任分类必须同时满足：首个必需标头未完成、未观察到任何stdout字节、本地退出码255，
以及保留stderr前缀中的已知模式。一旦读到任何stdout字节（包括部分标头），
类似主机密钥错误的stderr不能触发特定信任指引；完整广告之后的失败保留固定通用诊断。
广告之前的模式仍只是诊断启发式，不等于指纹核验。

成功discovery的子程序若等待请求，通常会耗尽100 ms原生退出观察窗口，每次discovery
分别承担该成本。这与两秒直接子程序清理预算分开，不构成性能基准或任意后代清理保证。
