# `libra mega2 browser`

以 HTTP 浏览远端 Mega2 仓库的**单层目录**：既可在终端中交互浏览，也可为脚本与
Agent 输出一次性的机器可读列表。

`mega2` 是 Libra 专属扩展，**没有 Git 等价契约**：它不 clone、不 fetch、不 push
Git 对象，浏览的是远端元数据而非本地 tree 对象。需要检查本地对象时请使用
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
  的 URL 一律拒绝；客户端固定追加 `/api/v1/tree`。
- `PATH` 必须为 rooted 路径（默认 `/`），不得包含 `.`/`..` 组件、NUL、控制字符
  或平台分隔符。
- `--ref` 可选，用于指定 commit 或 tag；省略时使用服务端默认 revision。

每次导航或刷新只发送一个匿名 `GET /api/v1/tree`：不附带 `Authorization` 头，
不打开本地仓库（可在仓库外直接使用），也不读写任何本地状态（index、对象库、
数据库、配置）。

客户端有界：禁用 redirect 与 proxy，10 秒超时，响应体上限 1 MiB、条目上限
2000，且 `content_type` 只接受 `directory` 或 `file`。名称含 `..`、路径分隔符
或终端控制字符的条目一律 fail-closed；服务端返回的条目 `path` 不作为导航依据。

### 交互模式（默认）

交互模式要求 **stdin 与 stdout 都是终端**。任一不是 TTY 时立即以稳定错误拒绝，
并提示改用 `--json`；拒绝发生在改动终端之前。

浏览期间终端进入 raw mode，由有界状态机驱动：无递归、无后台预取。

| 按键 | 动作 |
|------|------|
| `↑`/`↓` 或 `k`/`j` | 移动选择 |
| `Enter` | 进入选中的目录（对子路径发一次请求） |
| `Backspace` 或 `h` | 返回上级目录（不会高于 `/`） |
| `+` | 在当前目录建立子目录（见下） |
| `d` | 删除选中目录（需要额外确认行；文件为惰性） |
| `m` | 将选中目录移动到编辑后的目标父路径 |
| `R` | 原地重命名选中目录（同层移动；`r` 仍是重新加载） |
| `t` | 开关 tag 面板（见下） |
| `r` | 重新加载当前列表 |
| `q`（或 `Ctrl-C`、`Esc`） | 退出 |

所有退出路径（含错误与被处理的信号）都会还原终端状态（raw mode 与备用屏幕）。

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
| 启动、`Enter`、`Backspace`/`h`、`r` | `--list`（PATH 即目标目录；不带 flag 的 `--json` 与之相同） | `GET /api/v1/tree` |
| `+` | `--create-dir <NAME>`（PATH 为父目录） | `POST /api/v1/create-entry` |
| `d` | `--delete-dir <NAME>`（PATH 为父目录） | `POST /api/v1/delete-entry` |
| `m` | `--move-dir <NAME> <PARENT-PATH>`（PATH 为 NAME 当前的父目录；名称不变） | `POST /api/v1/move-entry` |
| `R` | `--rename-dir <NAME> <NEW-NAME>`（PATH 为父目录，保持不变） | `POST /api/v1/move-entry` |
| `t`，再按 `n`/`p` | `--list-tags [--page <N>] [--per-page <N>]`（PATH 只能为 `/`） | `GET /api/v1/tags/list` |
| `t`，再按 `+` | `--create-tag <NAME> [--message <TEXT>]`（PATH 只能为 `/`） | `POST /api/v1/tags` |

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

### 建目录（`+`，仅交互模式）

按 `+` 打开单行名称编辑器，在当前路径下建立子目录（位于根目录时发送的
parent 即 `/`）。若当前选中项是**文件**，编辑器拒绝打开（选择项必须是目录或空白
区域）。按 `Enter` 会先用与 wire 相同的规则校验名称（拒绝 `/`、`\`、`.`、`..`、
NUL 与控制字符），通过后才发送**一次** `POST /api/v1/create-entry`
（`is_directory=true`、`skip_build=true`、无 `content`）；按 `Esc` 直接取消，完全不发
请求。

确认建立后恰好重新加载一次当前列表。失败情形（401/403、重名、超时、响应格式
错误）会保留屏幕上最后一次安全列表，并显示**不含秘密**的状态行；终端不会停留在
raw mode，TUI 也永不要求你在备用屏幕上输入原始 token。

写入 token 的解析优先级：`--token-file <path>` → 环境变量 `LIBRA_MEGA2_TOKEN` →
`--token`（会留在 shell history，建议用前两者）。非交互写操作（见上文）遵守同一规则。
读操作拒绝 token flag：不带写操作 flag 的 `--json`/`--machine` 与 `--list` 都不接受凭据。

### 删除、移动与改名（`d`、`m`、`R`，仅交互模式）

`d` 在删除选中目录前要求额外的确认行；`m` 把选中目录移动到编辑后的目标父路径；
`R` 原地重命名（即同层移动）。这三个键对文件一律惰性；`Esc` 取消任何编辑器且不
发请求；敌意目标（`..`、分隔符、非 rooted 路径）在 POST 前即被拒绝。

每次确认的变更执行**一次** `POST /api/v1/delete-entry` 或
`POST /api/v1/move-entry`，成功后恰好重新加载一次列表。成功会清除状态行；
失败（401/403、源不存在、目标重名、超时）保留最后一次安全列表并显示不含秘密的
状态行，终端保持完好。没有多选、没有递归：每次操作只针对一个选中目录。

### Tag 面板（`t`，仅交互模式）

按 `t` 打开 tag 面板，并为第 1 页执行**一次**匿名
`GET /api/v1/tags/list`（三个必填 query 键为 `page`、`per_page`、`path`；本 MVP
固定使用仓库根 `path=/`）。`n`/`p` 显式请求下一页/上一页——每键一次请求，绝不
预取。`+` 先收集 tag 名，再收集可选 message（message 为空 = lightweight tag，
非空 = annotated tag）；`d` 在删除选中 tag 前要求额外确认行。`t`、`Esc` 或 `q`
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

- 每次导航/刷新一个请求；无递归、无预取、无后台任务、无跨进程缓存。
- 浏览只读且匿名。确认 `+` 后最多一次 `create-entry` POST 加一次重载 GET；
  `d`/`m`/`R` 同样各加一次 `delete-entry`/`move-entry` POST 加一次重载 GET；本命令
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

# 不用终端把 /src/pkg 移到 /lib/pkg
libra --json mega2 browser --server https://mega2.example.com --move-dir pkg /lib /src --token-file ~/.mega2-token

# 不用终端把 /src/old 改名为 /src/new
libra --json mega2 browser --server https://mega2.example.com --rename-dir old new /src --token-file ~/.mega2-token

# root tag 的第 2 页，每页 50 个，输出 JSON
libra --json mega2 browser --server https://mega2.example.com --list-tags --page 2 --per-page 50

# 不用终端建立 annotated root tag v1.0
libra --json mega2 browser --server https://mega2.example.com --create-tag v1.0 --message "release 1.0" --token-file ~/.mega2-token

# 列出指定 commit 或 tag
libra mega2 browser --server https://mega2.example.com --ref v1.2

# 交互建/删/移/改名（+、d、m、R）与 tag 面板（t），可配 --token-file
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
