# `libra mega2`

`libra mega2 traces` 读取限定范围内的 Agent 历史，也可以取回一条已校验的 stream。

```
libra mega2 traces capabilities --server <BASE-URL> [--token-file <PATH>] [--require-search]
libra mega2 traces targets --server <BASE-URL> --path </project/foo>
libra mega2 traces streams --server <BASE-URL> --target <UUID> [--source <ID>] [--owner <UUID>] [--epoch <UUID>] [--cursor <CURSOR>] [--limit <N>]
libra mega2 traces sessions --server <BASE-URL> --stream <UUID> [--since <RFC3339>] [--until <RFC3339>] [--agent <KIND>] [--code-commit <OID>] [--session <ID>] [--cursor <CURSOR>]
libra mega2 traces show --server <BASE-URL> --stream <UUID> (--session <ID> | --checkpoint <ID>)
libra mega2 traces checkpoints --server <BASE-URL> --stream <UUID> --session <ID> [--cursor <CURSOR>]
libra mega2 traces search --server <BASE-URL> --target </project/foo> --query <TEXT> [--limit <N>] [--max-bytes <N>]
libra mega2 traces transcript --server <BASE-URL> --stream <UUID> --checkpoint <ID> [--max-bytes <N>] [--cursor <CURSOR>]
libra mega2 traces fetch --remote <NAME> --stream <UUID>
```

搜索文本必须是 1 到 256 个 UTF-8 字节，否则命令不会打开网络连接。客户端原样发送查询，ASCII 大小写折叠由服务端完成。未指定的列表 `limit` 是 50；未指定的搜索是 20 条和 16384 字节。超过 200 或 100 的 limit 在网络前拒绝。`--owner`、`--epoch`、`--agent`、`--session`、`--cursor` 按原值传递。`--code-commit` 必须是 40 或 64 位十六进制。`--since` / `--until` 按 RFC3339 解析后以 UTC 发送，区间是 `[since, until)`；since 晚于 until 在网络前拒绝，相等是空区间。`--session` 与 `--checkpoint` 不能同时使用。`--require-search` 只在 `search_ready` 为布尔 `true` 时成功；字段缺失或不是布尔值会拒绝，而不带该旗标的 `capabilities` 仍输出其余能力。跨源重定向不转发 `Authorization`。超过 1 MiB 的响应会被拒绝。`fetch` 在 `remote.<name>.url` 的同一源上以 Git upload-pack 读取 `/.traces/by-scope/<stream>.git`：HTTP(S) 需要 token（匿名 HTTP 在打开连接前拒绝），SSH 使用 `traces@` 登录名与已配置的密钥。它从已认证的广告读取 `server_id`，校验并索引收到的 pack，只有广告的 tip 是该 pack 中的 commit 且哈希类型与仓库对象格式一致时，才写入 `refs/remotes/<remote>/traces/<stream>/<sha1|sha256>`。它记录服务指纹，拒绝静默改指另一台服务，也不改 `refs/heads/traces`、`refs/libra/traces`、`HEAD`、index 或工作区。未知或无权限的 stream 返回 `unknown_scope`，不发布任何 ref。

```
libra mega2 traces fetch --remote origin --stream 3f1d2c4b-5a6e-4f70-8a9b-0c1d2e3f4a5b --token-file ~/.mega2-token
```

# `libra mega2 browser`

以 HTTP 浏览远端 Mega2 仓库：终端采用**左侧目录树、右侧文件内容**的双栏界面；
脚本与 Agent 仍可通过 `--list`、`--json` 或 `--machine` 获取单层目录列表。

`mega2` 是 Libra 专属扩展，**没有 Git 等价契约**：它不 clone、不 fetch、不 push
Git 对象，浏览的是远端元数据与文件预览，而非本地 tree 对象。需要检查本地对象时请使用
[`libra ls-tree`](ls-tree.md)。

## 概要

```
libra mega2 browser --server <BASE-URL> [PATH] [--ref <COMMIT-OR-TAG>] [--json|--machine]
libra mega2 browser --server <BASE-URL> [PATH] [--ref <COMMIT-OR-TAG>] --list [--json|--machine]
```

## 说明

命令在接触终端或网络**之前**会先校验全部输入：

- `--server` 必须是 `https://…`；仅当主机为 loopback（`127.0.0.1`、`::1`、
  `localhost`）时才允许 `http://…`。含 userinfo、query、fragment 或 base path
  的 URL 一律拒绝；客户端使用固定的 `/api/v1/tree` 与 `/api/v1/blob` 读取路由。
- `PATH` 必须为 rooted 路径（默认 `/`），不得包含 `.`/`..` 组件、NUL、控制字符
  或平台分隔符。
- `--ref` 可选，用于指定 commit 或 tag；省略时使用服务端默认 revision。

每次展开或刷新目录发送一个匿名 `GET /api/v1/tree`；选中文件发送一个匿名
`GET /api/v1/blob?path=…&refs=…`（未指定 `--ref` 时省略 `refs`）。目录间移动光标、
折叠分支和滚动已加载的内容均不请求网络。两种读取均不附带 `Authorization` 头，
不打开本地仓库（可在仓库外直接使用），也不读写任何本地状态（index、对象库、
数据库、配置）。

客户端有界：禁用 redirect 与 proxy，10 秒超时，响应体上限 1 MiB、条目上限
2000，且 `content_type` 只接受 `directory` 或 `file`。名称含 `..`、路径分隔符
或终端控制字符的条目一律 fail-closed；服务端返回的条目 `path` 不作为导航依据。

> **Revision 注意：** `--ref` 只选择浏览/读取的 revision。当前交互模式的创建、删除、
> 移动和重命名快捷键仍作用于服务端默认 revision，而不是屏幕选择的 commit/tag。
> 使用 `--ref` 浏览时只读，不要确认交互写入；此行为修复已延期。非交互写操作
> 与 `--ref` 的组合继续在发请求前被拒绝。

### 交互模式（默认）

交互模式要求 **stdin 与 stdout 都是终端**。任一不是 TTY 时立即以稳定错误拒绝，
并提示改用 `--json`；拒绝发生在改动终端之前。

浏览期间终端进入 raw mode，目录按需展开，不递归请求或后台预取。左栏保留已展开
分支及其兄弟节点；选中文件后，右栏显示带行号的文本，可独立滚动。
加载中、空文件、文件不存在、二进制/不支持的类型、超大文件均有明确提示；预览失败
不会退出浏览器，也不会继续显示上一份文件。服务端需支持 `/api/v1/blob` UTF-8 文本
接口。文本上限 256 KiB、HTTP 响应上限 1 MiB、请求超时 10 秒，不保存本地副本。
Tab 字符显示为四个空格，终端控制字符和双向文本控制字符会被替换。展开的目录树
最多 8192 行、路径和名称共 4 MiB、128 层子节点；折叠分支可释放空间。
Unix 终端缩放时自动重绘；小于 49 列 × 10 行时显示调整窗口大小提示。

文件预览根据文件名/扩展名（或脚本首行的 shebang）识别语法。支持 Rust、Python、
JavaScript、C/C++、Shell、JSON、YAML、Markdown、HTML/CSS 与 TOML/INI 配置；
`.buckconfig`、`.gitconfig`、`.editorconfig` 和 `BUCK`/`BUILD` 也有文件名规则。
关键字、字符串、数字、名称和注释使用终端自身调色板，内容标题显示检测到的类型。
未知类型保持纯文本。`--no-color`、`--color=never` 以及自动模式下的 `NO_COLOR`/
`TERM=dumb` 会关闭语法颜色；显式 `--color=always` 优先于自动环境策略。
颜色仅影响交互预览，机器模式和单层列表输出不变，也不会增加网络请求。
高亮按文件缓存，多行语法在滚动后保持一致。显示规范化后超过 64 KiB、单行超过
4096 字节或耗尽高亮工作限额时，会回退到纯文本并在标题提示；文本预览本身仍受
256 KiB 上限约束。

| 按键 | 动作 |
|------|------|
| `↑`/`↓` 或 `k`/`j` | 移动目录树选择；内容区获得焦点时逐行滚动 |
| `Enter` | 展开/折叠目录；选中文件时将焦点切到内容区 |
| `→` | 展开目录或选择第一个子节点；内容区中向右滚动 |
| `←`、`Backspace` 或 `h` | 折叠分支或选择父节点；位于子树顶层时加载上级目录（不高于 `/`）；内容区 `←` 向左滚动 |
| `Tab` | 在目录树与文件内容区之间切换焦点 |
| `PageUp` / `PageDown` | 按屏滚动文件内容 |
| `a` | 在所选目录内新建子目录（见下） |
| `d` | 删除选中目录（弹出确认框；文件为惰性） |
| `m` | 通过目标目录选择器移动选中的文件或目录 |
| `e` | 原地重命名选中目录（同层移动；`r` 仍是重新加载） |
| `t` | 开关 tag 面板（见下） |
| `r` | 刷新选中的目录或文件；空目录树时刷新根路径 |
| `q`（或 `Ctrl-C`、`Esc`） | 退出 |

主要快捷键均为无需 Shift 的单键；tag 面板中同样按 `a` 新建。
旧的 `+`（新建）和 `R`（改名）保留为兼容别名。
输入名称或 tag 说明时，可打印的快捷键字符（包括 `q`）按普通文本输入；Enter 提交，
Backspace 删除字符，Esc 取消。目录名称及 tag 名称/说明都在居中的弹出框内输入，带有
字段标签和可见光标；长输入自动显示末尾，校验错误直接显示在框内。删除也使用弹出确认框。
移动使用单独的目录选择器：`→` 进入目标目录，第一个 `Enter` 选择目标，第二个
`Enter` 才确认写入。弹出框适配最小 49 × 10 的终端并支持无颜色模式，取消后保留原来的目录树/tag 选中项。

所有退出路径（含错误与被处理的信号）都会还原终端状态（raw mode 与备用屏幕）。
目录修改快捷键仅在目录树获得焦点时生效。`a` 在选中目录内部建立子目录，无论该
目录展开还是折叠，完成后刷新其子项。删除、重命名作用于选中目录；移动则作用于
选中的文件或目录，并使用其实际父路径（包括深层节点），完成后刷新父分支。
从深层分支跨父路径移动时，还会重新加载可见根并折叠分支，避免目标位置保留过时内容。
JSON/机器模式的 schema 和单次请求行为保持不变。

### 机器模式（`--json` / `--machine`）

使用 `--json`（或 `--machine`，等价于 `--json=ndjson --no-pager --color=never
--quiet`）且不带操作 flag 时，命令与 `--list` 一样列出 PATH（见「非交互操作」）：
执行**恰好一次**请求，并输出标准 Libra JSON envelope：

```json
{
  "ok": true,
  "command": "mega2 browser",
  "data": {
    "operation": "list",
    "server": "https://mega2.example.com",
    "ref": "v1.2",
    "path": "/src",
    "items": [
      { "name": "pkg", "content_type": "directory" },
      { "name": "main.rs", "content_type": "file" }
    ]
  }
}
```

`items` 顺序确定：目录优先，其后按名称升序。`server` 为校验后 URL 的规范
scheme/host/port origin。

没有机器输出模式、也没有操作 flag 时，`--quiet` 会被拒绝（会破坏交互渲染）；带操作
flag 时，`--quiet` 只是不输出纯文本摘要。

### 非交互操作

browser 的功能另有非交互形式，供脚本、CI 与黑盒测试使用：每种形式是同一个
`mega2 browser` 命令上的一个操作 flag，每次调用至多接受一个操作 flag。下表列出本版本
已提供的形式。

| 交互键 | Flag | 请求 |
|--------|------|------|
| 启动、展开/刷新目录、加载上级根目录 | `--list`（PATH 即目标目录；不带 flag 的 `--json` 与之相同） | `GET /api/v1/tree` |
| `a` | `--create-dir <NAME>`（PATH 为父目录） | `POST /api/v1/create-entry` |
| `d` | `--delete-dir <NAME>`（PATH 为父目录） | `POST /api/v1/delete-entry` |
| `m` | `--move-dir <NAME> <PARENT-PATH>`（仅目录；PATH 为 NAME 当前的父目录；名称不变） | `POST /api/v1/move-entry` |
| `e` | `--rename-dir <NAME> <NEW-NAME>`（PATH 为父目录，保持不变） | `POST /api/v1/move-entry` |
| `t`，再按 `n`/`p` | `--list-tags [--page <N>] [--per-page <N>]`（PATH 只能为 `/`） | `GET /api/v1/tags/list` |
| `t`，再按 `a` | `--create-tag <NAME> [--message <TEXT>]`（PATH 只能为 `/`） | `POST /api/v1/tags` |
| `t`，再按 `d` | `--delete-tag <NAME>`（PATH 只能为 `/`） | `DELETE /api/v1/tags/{name}` |

所有非交互调用遵守同一组规则：

- **不用终端、不读输入。** stdin 可以关闭或重定向，stdout/stderr 可以是管道；
  从不读取 stdin、从不提示、从不改动终端状态。
- **一次请求。** 先在本地校验输入；通过校验的调用恰好发出一次 HTTP 请求，未通过的
  一次也不发。不 reload、不 preflight、不重试。
- **输出。** 成功时 stdout 只有 JSON envelope（`--json`、`--machine`）或纯文本摘要
  （控制字符已替换；`--quiet` 时不输出）。失败时 stdout 为空，错误写到 stderr：
  `--json`/`--machine` 下是 JSON 错误信封（见「机器可读的错误细节」），否则是人读错误。
- **凭据。** 读操作拒绝 token flag：读操作匿名，拒绝 `--token` 与 `--token-file`
  （`LBR-CLI-002`，不发请求），也不读取 `LIBRA_MEGA2_TOKEN`。写操作在人读与机器模式下
  都至多取一个 token——依次为 `--token-file`、`LIBRA_MEGA2_TOKEN`、`--token`——并以一个
  `Authorization: Bearer` 头发送；没有任何来源时匿名写入（适用于 `push_auth=none` 的
  服务端）。token 从不持久化、从不输出，服务端拒绝它时也不例外。
- **一次一个操作，写操作不带 `--ref`。** 同时给出两个操作 flag 是用法错误
  （`LBR-CLI-002`），写操作带 `--ref` 也是（写入总是作用于服务端默认 revision）；
  两者都不发请求。
- **tag 只在根上，不分 revision。** tag 操作只作用于 root tag：PATH 只能为 `/`（默认值），
  并拒绝 `--ref`；两者都以 `LBR-CLI-002` 失败，不发请求。
- **写请求不重试。** 结果未知的写请求 Libra 绝不重发。以下情形结果未知：在收到任何
  响应之前超时或失败（`LBR-NET-001`，`details.transport` 为 `timeout` 或 `request`）；
  读取回执时连接中断（`LBR-NET-001`，带 `details.http_status`）；`2xx` 但回执不合规
  （`LBR-NET-002`，`details.http_status` 为 `2xx`）。重新调用之前先核对结果：目录写操作之后用
  `--list`；tag 写操作之后用 `--list-tags` 翻页直到 `has_next` 为 `false`——annotated tag 一定出现在
  某一页上，lightweight tag 却可能哪一页都不出现（见 `--list-tags` 的分页说明）。
  `details.transport` 为 `connect` 表示请求根本没有到达服务端。
- **退出码。** 成功为 `0`；失败时取 stable code 的退出码（用法错误如
  `LBR-CLI-002`、`LBR-CLI-003` 为 `129`，其余为 `128`），设置
  `LIBRA_FINE_EXIT_CODES=1` 时为类别码（`2`–`9`）。

`--list` 每个条目输出一行，目录在前，格式为 `dir  <名称>` 或 `file  <名称>`；
带 `--json`/`--machine` 时输出上面的 payload。

`--create-dir <NAME>` 以一次 `POST /api/v1/create-entry` 在 PATH 下建立目录 NAME，
之后不重新列出。纯文本摘要为 `created directory <path> (commit <commit_id>)`；
带 `--json`/`--machine` 时，payload 把「请求了什么」与「服务端回答了什么」分开：

```json
{
  "operation": "create-dir",
  "server": "https://mega2.example.com",
  "target": { "parent": "/src", "name": "pkg", "path": "/src/pkg" },
  "receipt": { "commit_id": "…", "new_oid": "…", "path": "/src/pkg", "cl_link": null }
}
```

`target` 由已校验的本地输入组成；`receipt` 是服务端回执原样（`path` 与 `cl_link`
可能为 `null`）。错误：

| 情形 | stable code | `details` |
|------|-------------|-----------|
| NAME 为 `.`/`..`、为空、过长，或含分隔符、控制字符 | `LBR-CLI-002` | 无（不发请求） |
| 与另一个操作 flag 或 `--ref` 同用 | `LBR-CLI-002` | 无（不发请求） |
| 目录已存在（mega2 目前回 HTTP 500） | `LBR-NET-002` | `http_status: 500` |
| HTTP 400（名称或父目录被拒） | `LBR-CLI-003` | `http_status: 400` |
| HTTP 401（服务端要求写 token）/ 403（token 无权写此处） | `LBR-AUTH-001` / `LBR-AUTH-002` | `http_status` |
| HTTP 409 | `LBR-CONFLICT-002` | `http_status: 409` |
| 其它非 2xx，或 2xx 但回执不合规 | `LBR-NET-002` | `http_status` |
| 收到响应之前超时或连接失败 | `LBR-NET-001` | `transport` |
| 读取回执时连接中断（结果未知） | `LBR-NET-001` | `http_status` |

`--delete-dir <NAME>` 以一次 `POST /api/v1/delete-entry` 删除 PATH 下的目录 NAME，
之后不重新列出。没有确认行：flag 本身写明了确切目标。纯文本摘要为
`deleted directory <path> (commit <commit_id>)`；带 `--json`/`--machine` 时 payload 为：

```json
{
  "operation": "delete-dir",
  "server": "https://mega2.example.com",
  "target": { "parent": "/src", "name": "pkg", "path": "/src/pkg" },
  "receipt": { "commit_id": "…", "path": "/src/pkg", "cl_link": null }
}
```

Libra 不预先检查 NAME 是否存在、是否为目录：两者都由服务端回答。目标是文件时为
`LBR-CLI-003`；目标不存在时目前为 `LBR-NET-002`，以 `details.http_status` 与其它失败
区分。错误：

| 情形 | stable code | `details` |
|------|-------------|-----------|
| NAME 为 `.`/`..`、为空、过长，或含分隔符、控制字符 | `LBR-CLI-002` | 无（不发请求） |
| 与另一个操作 flag 或 `--ref` 同用 | `LBR-CLI-002` | 无（不发请求） |
| HTTP 400：NAME 是文件、PATH 途经文件，或服务端拒绝该路径（例如 trunk 服务端上的顶层目录） | `LBR-CLI-003` | `http_status: 400` |
| NAME 或 PATH 本身不存在（HTTP 404） | `LBR-NET-002` | `http_status: 404` |
| HTTP 401 / 403 / 409、其它失败与结果未知 | 同 `--create-dir` | 同 `--create-dir` |

`--move-dir <NAME> <PARENT-PATH>` 以一次 `POST /api/v1/move-entry` 把 PATH 下的目录
NAME 移到已存在的目录 PARENT-PATH 下，名称不变，之后不重新列出。PARENT-PATH 在发请求
之前按与 PATH 相同的规则校验。纯文本摘要为
`moved directory <from> -> <to> (commit <commit_id>)`；带 `--json`/`--machine` 时
payload 为：

```json
{
  "operation": "move-dir",
  "server": "https://mega2.example.com",
  "target": {
    "from": { "parent": "/src", "name": "pkg", "path": "/src/pkg" },
    "to": { "parent": "/lib", "name": "pkg", "path": "/lib/pkg" }
  },
  "receipt": { "commit_id": "…", "from_path": "/src/pkg", "to_path": "/lib/pkg", "cl_link": null }
}
```

与 `--delete-dir` 相同，能否移动由服务端回答。错误：

| 情形 | stable code | `details` |
|------|-------------|-----------|
| NAME 为 `.`/`..`、为空、过长，或含分隔符、控制字符 | `LBR-CLI-002` | 无（不发请求） |
| PARENT-PATH 不是 rooted 路径，或含 `.`/`..` 组件、`\` 或控制字符 | `LBR-CLI-003` | 无（不发请求） |
| 与另一个操作 flag 或 `--ref` 同用 | `LBR-CLI-002` | 无（不发请求） |
| HTTP 400：PARENT-PATH 下已有名为 NAME 的条目、PARENT-PATH 就是 PATH 或位于 NAME 之内、NAME 是文件、PATH 或 PARENT-PATH 途经文件，或服务端拒绝该路径（例如 trunk 服务端上跨两个顶层目录的移动） | `LBR-CLI-003` | `http_status: 400` |
| HTTP 404：NAME、PATH 或 PARENT-PATH 不存在 | `LBR-NET-002` | `http_status: 404` |
| HTTP 401 / 403 / 409、其它失败与结果未知 | 同 `--create-dir` | 同 `--create-dir` |

`--rename-dir <NAME> <NEW-NAME>` 以一次同父目录的 `POST /api/v1/move-entry` 把 PATH 下的
目录 NAME 改名为 NEW-NAME，之后不重新列出。纯文本摘要为
`renamed directory <from> -> <to> (commit <commit_id>)`；带 `--json`/`--machine` 时
payload 与 `--move-dir` 同形，`target.to.parent` 等于 `target.from.parent`：

```json
{
  "operation": "rename-dir",
  "server": "https://mega2.example.com",
  "target": {
    "from": { "parent": "/src", "name": "old", "path": "/src/old" },
    "to": { "parent": "/src", "name": "new", "path": "/src/new" }
  },
  "receipt": { "commit_id": "…", "from_path": "/src/old", "to_path": "/src/new", "cl_link": null }
}
```

错误：

| 情形 | stable code | `details` |
|------|-------------|-----------|
| NAME 或 NEW-NAME 为 `.`/`..`、为空、过长，或含分隔符、控制字符 | `LBR-CLI-002` | 无（不发请求） |
| 与另一个操作 flag 或 `--ref` 同用 | `LBR-CLI-002` | 无（不发请求） |
| HTTP 400：NEW-NAME 与 NAME 相同、PATH 下已有名为 NEW-NAME 的条目、NAME 是文件、PATH 途经文件，或服务端拒绝该路径（例如 trunk 服务端上的顶层目录） | `LBR-CLI-003` | `http_status: 400` |
| HTTP 404：NAME 或 PATH 不存在 | `LBR-NET-002` | `http_status: 404` |
| HTTP 401 / 403 / 409、其它失败与结果未知 | 同 `--create-dir` | 同 `--create-dir` |

`--list-tags` 以一次匿名 `GET /api/v1/tags/list`（query 为 `page`、`per_page` 与 `path=/`）
列出一页 root tag。`--page`（1–1000，默认 1）与 `--per-page`（1–100，默认 20，即交互
面板的页大小）选择页面；不带 `--list-tags` 时两者都被拒绝。每个 tag 输出一行
`<name>  <object_type>  <tagger>`，有 message 时下一行以四个空格缩进输出 message，
最后一行为 `page <page> · per_page <per_page> · total <total>`；服务端字符串中的控制字符
输出为 `?`。带 `--json`/`--machine` 时 payload 为：

```json
{
  "operation": "list-tags",
  "server": "https://mega2.example.com",
  "path": "/",
  "page": 1,
  "per_page": 20,
  "total": 42,
  "has_next": true,
  "items": [
    { "name": "v1.0", "tag_id": "…", "object_id": "…", "object_type": "commit",
      "tagger": "…", "message": "…", "created_at": "…" }
  ]
}
```

`has_next` 为 `page * per_page < total`；`items` 原样保留服务端的每个字段。mega2 目前在
数据库中对 annotated tag 分页，再用 `refs/tags/*` ref 补满本页剩余的位置：每页都从第一个
ref 重新开始取，只跳过本页已有的 annotated tag。annotated tag 也有 ref，因此一页可能重复
出现其它页的 tag；这类条目与 lightweight tag 一样只带 ref 字段（`object_type` 为 `commit`，
`tagger` 与 `message` 为空，`tag_id` 等于 `object_id`）。`total` 会把这些 ref 全部加到
annotated 的计数上，因此可能大于实际不同 tag 的数目、随页不同，`has_next` 也随之受影响。错误：

| 情形 | stable code | `details` |
|------|-------------|-----------|
| `--page` 不在 1–1000 或 `--per-page` 不在 1–100 | `LBR-CLI-002` | 无（不发请求） |
| 不带 `--list-tags` 而给出 `--page` 或 `--per-page` | `LBR-CLI-002` | 无（不发请求） |
| PATH 格式不合规（不是 rooted 路径，或含 `.`/`..` 组件） | `LBR-CLI-003` | 无（不发请求） |
| PATH 是 `/` 以外的合规路径、带 `--ref`、带 token flag，或与另一个操作 flag 同用 | `LBR-CLI-002` | 无（不发请求） |
| HTTP、传输或响应失败 | 该失败的 stable code（见「机器可读的错误细节」） | `http_status` 或 `transport` |

`--create-tag <NAME>` 以一次 `POST /api/v1/tags`（`path_context` 为 `/`）建立 root tag
NAME。不带 `--message` 时为 lightweight tag；`--message <TEXT>` 使它成为 annotated tag。
与交互面板「空 message 即 lightweight」不同，非交互的 `--message` 必须非空、不超过 1024
字节且不含控制字符（包括换行）：不合规即拒绝，绝不改写。root tag 需要 paths 覆盖 `/` 的
写 token，或 `push_auth=none` 的服务端；只覆盖其它路径的 token 得到 HTTP 403
（`LBR-AUTH-002`）。纯文本摘要为 `created <kind> tag <name> -> <object_id>`；带
`--json`/`--machine` 时 payload 为：

```json
{
  "operation": "create-tag",
  "server": "https://mega2.example.com",
  "target": { "name": "v1.0", "kind": "annotated", "path": "/" },
  "receipt": { "name": "v1.0", "tag_id": "…", "object_id": "…", "object_type": "commit",
               "tagger": "…", "message": "…", "created_at": "…" }
}
```

`target.kind` 为 `lightweight` 或 `annotated`；`receipt` 是服务端返回的 tag 原样。错误：

| 情形 | stable code | `details` |
|------|-------------|-----------|
| NAME 违反 tag 名规则（见 Tag 面板） | `LBR-CLI-002` | 无（不发请求） |
| `--message` 为空、超过 1024 字节或含控制字符，或不带 `--create-tag` 而给出 `--message` | `LBR-CLI-002` | 无（不发请求） |
| PATH 格式不合规（不是 rooted 路径，或含 `.`/`..` 组件） | `LBR-CLI-003` | 无（不发请求） |
| PATH 是 `/` 以外的合规路径、带 `--ref`，或与另一个操作 flag 同用 | `LBR-CLI-002` | 无（不发请求） |
| HTTP 400（例如 tag 已存在）、405 或 422 | `LBR-CLI-002` | `http_status` |
| HTTP 401（服务端要求写 token）/ 403（token 未覆盖 `/`） | `LBR-AUTH-001` / `LBR-AUTH-002` | `http_status` |
| HTTP 404 / 409 | `LBR-CLI-003` / `LBR-CONFLICT-002` | `http_status` |
| 其它非 2xx 或回执不合规 / 超时或连接中断 | `LBR-NET-002` / `LBR-NET-001`，同 `--create-dir` | `http_status` 或 `transport` |

`--delete-tag <NAME>` 以一次 `DELETE /api/v1/tags/{name}?path=/`（NAME 经百分号编码）删除
root tag NAME，没有确认行，之后不重新列出。与建立 root tag 相同，删除也需要 paths 覆盖
`/` 的写 token，或 `push_auth=none` 的服务端。纯文本摘要为 `deleted tag <name>`；带
`--json`/`--machine` 时 payload 为：

```json
{
  "operation": "delete-tag",
  "server": "https://mega2.example.com",
  "target": { "name": "v1.0", "path": "/" },
  "receipt": { "deleted_tag": "v1.0", "message": "…" }
}
```

错误：

| 情形 | stable code | `details` |
|------|-------------|-----------|
| NAME 违反 tag 名规则（见 Tag 面板） | `LBR-CLI-002` | 无（不发请求） |
| PATH 格式不合规（不是 rooted 路径，或含 `.`/`..` 组件） | `LBR-CLI-003` | 无（不发请求） |
| PATH 是 `/` 以外的合规路径、带 `--ref`，或与另一个操作 flag 同用 | `LBR-CLI-002` | 无（不发请求） |
| tag 不存在（HTTP 404） | `LBR-CLI-003` | `http_status: 404` |
| HTTP 401（服务端要求写 token）/ 403（token 未覆盖 `/`） | `LBR-AUTH-001` / `LBR-AUTH-002` | `http_status` |
| HTTP 400、405 或 422 / 409 | `LBR-CLI-002` / `LBR-CONFLICT-002` | `http_status` |
| 其它非 2xx 或回执不合规 / 超时或连接中断 | `LBR-NET-002` / `LBR-NET-001`，同 `--create-dir` | `http_status` 或 `transport` |

### 建目录（`a`，仅交互模式）

按 `a` 打开居中的名称输入框，在选中目录内建立子目录（目录树为空时，在可见根目录下
建立子目录）。输入框显示父路径：选中 `/artifact` 并输入 `rust` 会创建
`/artifact/rust`，而非 `/rust`，选中目录无需先展开。若当前选中项是**文件**，编辑器拒绝打开（选择项必须是目录或空白
区域）。按 `Enter` 会先用与 wire 相同的规则校验名称（拒绝 `/`、`\`、`.`、`..`、
NUL 与控制字符），通过后才发送**一次** `POST /api/v1/create-entry`
（`is_directory=true`、`skip_build=true`、无 `content`）；按 `Esc` 直接取消，完全不发
请求。

确认建立后恰好重新加载一次目标目录并显示其子项。失败情形（401/403、重名、超时、响应格式
错误）会保留屏幕上最后一次安全列表，并显示**不含秘密**的状态行；终端不会停留在
raw mode，TUI 也永不要求你在备用屏幕上输入原始 token。

HTTP 400 不一定表示名称已存在，还需检查父路径、服务端路径策略和名称。
Mega2 限制新路径必须位于配置的 monorepo 根目录下；选中 `/artifact` 会在该根目录
内部创建，而不是新增顶层路径。为避免泄露秘密，不直接显示服务端错误响应正文。

写入 token 的解析优先级：`--token-file <path>` → 环境变量 `LIBRA_MEGA2_TOKEN` →
`--token`（会留在 shell history，建议用前两者）。非交互写操作（见上文）遵守同一规则。
读操作拒绝 token flag：不带写操作 flag 的 `--json`/`--machine` 与 `--list` 都不接受凭据。

### 删除、移动与改名（`d`、`m`、`e`，仅交互模式）

`d` 在删除选中目录前弹出确认框；`m` 为选中的文件或目录打开目标目录选择器；
`e` 原地重命名选中的目录（即同层移动）。选择器在浏览目标时固定源项：`→` 进入目录，
第一个 `Enter` 选择目标，第二个 `Enter` 执行移动。确认页按 `Esc` 回到选择器，再按
一次才取消，且不发送写入请求。文件仅对删除和改名保持惰性。

Mega2 的 trunk 写入不能落在 `/`，所以不能跨一级 monorepo 根目录移动。例如
`/artifact/rust` 不能移动到 `/data`。选择器只展示源根目录（例如 `/artifact`）之下的
目标，因而不会发送这个必然失败的请求；它也会在 POST 前拒绝把目录移动到自身或子目录。

每次确认的变更执行**一次** `POST /api/v1/delete-entry` 或
`POST /api/v1/move-entry`，成功后重新加载一次父分支列表。
从嵌套分支跨父目录移动时，会额外重载一次根目录并折叠目录树。成功会清除状态行；
失败（401/403、源不存在、目标重名、超时）保留最后一次安全列表并显示不含秘密的
状态行，终端保持完好。没有多选、没有递归：每次操作只针对一个选中条目
（删除/改名为目录，移动可以是文件或目录）。

### Tag 面板（`t`，仅交互模式）

按 `t` 打开 tag 面板，并为第 1 页执行**一次**匿名
`GET /api/v1/tags/list`（三个必填 query 键为 `page`、`per_page`、`path`；本 MVP
固定使用仓库根 `path=/`）。`n`/`p` 显式请求下一页/上一页——每键一次请求，绝不
预取。`a` 先收集 tag 名，再收集可选 message（message 为空 = lightweight tag，
非空 = annotated tag）；`d` 在删除选中 tag 前弹出确认框。`t`、`Esc` 或 `q`
关闭面板但不退出 browser，目录列表原样恢复。

tag 名在发请求前按服务端规则校验（非空、≤255 字节、不含 `..`、`@{`、`//`、不以
`.lock` 结尾，且不含空白/控制字符/禁用字符）。列表匿名；create 与 delete 复用
ADR-MB-03 的会话写入 token。渲染时对 tagger/message 做消毒，敌意服务端字符串
无法控制终端。面板仅操作 root tag。

## 选项

| 选项 | 说明 |
|------|------|
| `--server <BASE-URL>` | Mega2 服务端 base URL（必填）。HTTPS，或 loopback HTTP。 |
| `[PATH]` | 要列出的 rooted 目录，或目录写操作的父目录；默认 `/`。tag 操作只接受 `/`。 |
| `--ref <COMMIT-OR-TAG>` | 可选的 commit 或 tag。 |
| `--list` | 不用终端列出一次 PATH：纯文本行，带 `--json`/`--machine` 时为 JSON payload。 |
| `--create-dir <NAME>` | 不用终端在 PATH 下建立目录 NAME（一次 POST，不重新列出）。 |
| `--delete-dir <NAME>` | 不用终端删除 PATH 下的目录 NAME（一次 POST，无确认行，不重新列出）。 |
| `--move-dir <NAME> <PARENT-PATH>` | 不用终端把 PATH 下的目录 NAME 移到 PARENT-PATH 下，名称不变（一次 POST，不重新列出）。 |
| `--rename-dir <NAME> <NEW-NAME>` | 不用终端把 PATH 下的目录 NAME 改名为 NEW-NAME，父目录不变（一次 POST，不重新列出）。 |
| `--list-tags` | 不用终端列出一页 root tag（一次 GET）。 |
| `--page <N>` | 与 `--list-tags` 同用：要列出的页（1–1000；默认 1）。 |
| `--per-page <N>` | 与 `--list-tags` 同用：每页 tag 数（1–100；默认 20）。 |
| `--create-tag <NAME>` | 不用终端建立 root tag NAME（一次 POST）：lightweight，或带 `--message` 时为 annotated。 |
| `--message <TEXT>` | 与 `--create-tag` 同用：annotated tag 的 message（非空，不超过 1024 字节，不含控制字符）。 |
| `--delete-tag <NAME>` | 不用终端删除 root tag NAME（一次 DELETE，无确认行）。 |
| `--token-file <PATH>` | 写操作（交互或非交互）：从文件读取写入 token（优先级最高）。读操作拒绝此 flag。 |
| `--token <TOKEN>` | 写操作：内联写入 token（优先级最低；会留在 shell history，建议用 `--token-file`）。读操作拒绝此 flag。 |
| `--json[=<FORMAT>]` | 全局标志：单次请求 + JSON envelope（`pretty`/`compact`/`ndjson`）。 |
| `--machine` | 全局标志：严格 NDJSON 机器模式，供自动化使用。 |

## 错误

| 情形 | 稳定行为 |
|------|----------|
| 无效调用（URL 非法、路径非 rooted、缺少 `--server`） | 用法错误，不发请求 |
| 交互模式但非 TTY | 改动终端或发请求之前即拒绝，并提示使用 `--json` |
| 网络不可用 / 超时 | 稳定网络错误，不回显响应体 |
| HTTP 4xx/5xx 或重定向 | 稳定错误并标明状态码；绝不打印响应体 |
| 响应格式错误或含恶意条目 | 稳定协议错误；不渲染、不缓存 |

错误绝不回显服务端响应体、凭据、token 或未校验的路径。

### 机器可读的错误细节

每次失败的 Mega2 请求都会在 JSON 错误信封中加上 `details`（`--json`/`--machine`
时写在 stderr；stderr 不是终端时写在末尾的 JSON 行）。stable code 与 message
保持上表所列不变；`details` 让自动化不必解析 message 就能区分 HTTP 状态：

| 键 | 含义 |
|----|------|
| `method` | `GET`、`POST` 或 `DELETE` |
| `route` | 路由模板，例如 `/api/v1/tree`、`/api/v1/tags/{name}` |
| `http_status` | 收到的状态码；`2xx` 但响应体不合规、或读取响应体时连接中断，也给出该值 |
| `transport` | 没有收到状态码时的失败类别：`timeout`、`connect` 或 `request` |

```json
{"ok":false,"error_code":"LBR-NET-002","category":"network","exit_code":128,"severity":"fatal","message":"mega2 server returned HTTP 500 Internal Server Error","details":{"http_status":500,"method":"GET","route":"/api/v1/tree"}}
```

在发出请求之前就被拒绝的无效输入不带 `details`。这些值从不包含响应体、服务端 URL
或 token；键的完整说明见 [错误码](../../error-codes.md#command-specific-details)。

## 限制与边界

- 每次展开/刷新目录或切换到文件时一个请求；在目录行之间移动与折叠分支不发请求。
  无递归预取、无后台任务、无跨进程缓存。
- 浏览只读且匿名。确认 `a` 后最多一次 `create-entry` POST 加一次重载 GET；
  `d`/`m`/`e` 同样各加一次 `delete-entry`/`move-entry` POST 加一次父分支重载 GET
  （从嵌套分支跨父目录移动时额外重载一次根目录）；本命令
  不含递归或多选操作。
- 不持久化任何配置或凭据；不写磁盘。

## 示例

```bash
# 交互浏览远端根目录
libra mega2 browser --server https://mega2.example.com

# 直接打开某个 rooted 路径
libra mega2 browser --server https://mega2.example.com /src/pkg

# 不用终端输出纯文本列表（脚本、CI）
libra mega2 browser --server https://mega2.example.com --list /src/pkg

# 不用终端建立 /src/pkg（匿名写，适用于 push_auth=none 的服务端）
libra --json mega2 browser --server https://mega2.example.com --create-dir pkg /src

# 同一写操作，从文件读取 token（受 token 保护的服务端）
libra --json mega2 browser --server https://mega2.example.com --create-dir pkg /src --token-file ~/.mega2-token

# 不用终端删除 /src/pkg（没有确认行：flag 写明了目标）
libra --json mega2 browser --server https://mega2.example.com --delete-dir pkg /src --token-file ~/.mega2-token

# 不用终端把 /src/pkg 移到 /src/archive/pkg
libra --json mega2 browser --server https://mega2.example.com --move-dir pkg /src/archive /src --token-file ~/.mega2-token

# 不用终端把 /src/old 改名为 /src/new
libra --json mega2 browser --server https://mega2.example.com --rename-dir old new /src --token-file ~/.mega2-token

# root tag 的第 2 页，每页 50 个，输出 JSON
libra --json mega2 browser --server https://mega2.example.com --list-tags --page 2 --per-page 50

# 不用终端建立 annotated root tag v1.0
libra --json mega2 browser --server https://mega2.example.com --create-tag v1.0 --message "release 1.0" --token-file ~/.mega2-token

# 不用终端删除 root tag v1.0（没有确认行）
libra --json mega2 browser --server https://mega2.example.com --delete-tag v1.0 --token-file ~/.mega2-token

# 列出指定 commit 或 tag
libra mega2 browser --server https://mega2.example.com --ref v1.2

# 交互建/删/移/改名（a、d、m、e）与 tag 面板（t），可配 --token-file
libra mega2 browser --server https://mega2.example.com --token-file ~/.mega2-token

# 单次请求 + JSON envelope（无需 TTY，可在仓库外运行）
libra --json mega2 browser --server https://mega2.example.com

# 供自动化使用的 NDJSON
libra --machine mega2 browser --server http://127.0.0.1:8080
```

## 与 `libra ls-tree` 的对比

| 维度 | `libra mega2 browser` | `libra ls-tree` |
|------|----------------------|-----------------|
| 数据来源 | 远端 Mega2 HTTP API（`/api/v1/tree`） | 本地对象数据库 |
| 是否需要仓库 | 否 | 是 |
| 深度 | 每次请求恰好一层目录 | 任意 tree 路径（`-r` 可递归） |
| 鉴权 | 匿名（不带 token） | 本地仓库访问 |
| Git 兼容性 | 无（Libra 专属扩展） | Git 兼容 plumbing |
