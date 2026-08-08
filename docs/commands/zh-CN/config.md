# `libra config`

`libra config` 管理存储在 SQLite-backed `config_kv` 中的仓库本地和用户全局配置，包括由 vault 支撑的 secrets 和密钥管理。

**别名：** `cfg`

## 概要

```
libra config <subcommand> [options]
libra config set [--global | --system] [--add] [--encrypt] [--plaintext] [--stdin] <key> [<value>]
libra config get [--global | --system] [--all] [--reveal] [--regexp] [-d <default>] <key>
libra config list [--global | --system] [--name-only] [--show-origin] [--vault] [--ssh-keys] [--gpg-keys]
libra config unset [--global | --system] [--all] <key>
libra config import [--global]
libra config path [--global | --system]
libra config generate-ssh-key --remote <name>
libra config generate-gpg-key [--name <name>] [--email <email>] [--usage <usage>]
```

也支持 Git 兼容的标志风格（从帮助中隐藏）：

```
libra config [--get | --get-all | --unset | --unset-all | -l | --add | --import | --get-regexp | --show-origin] [--local | --global | --system] [-z | --null] [--type <t> | --bool | --int | --path] [key] [value] [-d <default>]
libra config --remove-section <name>
libra config --rename-section <old-name> <new-name>
```

## 说明

`libra config` 跨三个 scope 读写配置值：**local**（仓库级，存储在 `.libra/libra.db`）、**global**（用户级，存储在 `~/.libra/config.db`）和 **system**（机器级，存储在 `/etc/libra/config.db`；级联优先级最低，仅纯配置——无 vault）。各数据库都使用 SQLite 和 `config_kv` 表。

不同于 Git 的明文 INI 文件或 jj 的 TOML 文件，Libra 将配置存储在事务型数据库中，并集成 vault 加密。敏感值（API keys、tokens、SSH 私钥）会使用 AES-256-GCM 自动静态加密。

该命令支持两种调用风格：

1. **子命令风格**（推荐）：`libra config set key value`、`libra config get key`
2. **Git 兼容标志风格**（隐藏）：`libra config --get key`、`libra config key value`

使用 `get` 读取值时，Libra 会按优先级 local → global → system 级联查找。第一个匹配项胜出；system 库不可读时会被跳过。

### 裸读 `libra config <key>`

只给一个位置参数、不给值时是**读取**，与 `git config <key>` 一致：把已存储的值写 stdout 并以 0 退出；多值 key 返回**最后一个**值；级联顺序与 `get` 完全相同（local → global → system）；加密值渲染为 `<REDACTED>`（要明文用 `config get --reveal`）。key 未设置时以 **exit 1** + `LBR-CLI-002` 失败。`-z`/`--null` 与 `get` 上的行为一致：值以 NUL 而非换行结尾。

**与 Git 的有意差异**：对**受保护 key**——即 Libra 判定为机密的 key（`vault.env.*`、`auth.token.*`、`*.privkey`，或末段包含 `secret`、`token`、`password`、`credential`、`apikey`、`accesskey`、`privatekey`、`secretkey`）——裸读形式保留 Libra 的交互式安全赋值路径：它会**无回显地提示输入新值**，而不是打印已存储的值。没有终端时报 `missing value for protected key '<key>' (non-interactive environment)` 并以 2 退出。要读取受保护 key 请用 `libra config get <key>`，它返回 `<REDACTED>`。该差异已登记在 `COMPATIBILITY.md`。

## 选项

### 子命令

#### `set <key> [<value>]`

设置配置值。如果省略 `<value>` 且 key 是敏感 key，Libra 会交互式提示输入（隐藏回显）。在非交互上下文（CI/CD）中，使用 `--stdin` 管道传入值。

| 标志 | 说明 |
|------|------|
| `--add` | 将该 key 作为额外值添加，允许重复（类似 Git 的多值 key，如 `remote.origin.fetch`） |
| `--encrypt` | 即使 key 不匹配敏感 key 启发式，也强制 vault 加密 |
| `--plaintext` | 强制明文存储，即使看起来像敏感 key 也跳过自动加密 |
| `--stdin` | 从 stdin 读取值，而不是位置参数（适合在 CI/CD 中管道传 secrets） |

```bash
# 基本设置
libra config set user.name "Jane Doe"

# 设置全局配置
libra config set --global user.email "jane@example.com"

# 强制加密
libra config set --encrypt custom.api_token "sk-abc123"

# 从 stdin 设置（CI/CD）
echo "$SECRET" | libra config set --stdin vault.env.GEMINI_API_KEY

# 添加多值 key
libra config set --add remote.origin.fetch "+refs/heads/*:refs/remotes/origin/*"

# 省略敏感 key 的值时交互提示
libra config set vault.env.GEMINI_API_KEY
```

#### `get <key>`

获取配置值。按 local → global → system scope 级联，返回第一个匹配项（system 库不可读时跳过）。

| 标志 | 说明 |
|------|------|
| `--all` | 返回该 key 的所有值（多值 key） |
| `--reveal` | 对加密条目显示实际解密值（会阻止内部 vault 凭据，如 `vault.roottoken_enc`） |
| `--regexp` | 将 `<key>` 视为正则表达式，并返回所有匹配条目 |
| `-d`, `--default <value>` | key 未找到时返回此值（而不是报错） |

```bash
# 简单 get
libra config get user.name

# 带默认 fallback
libra config get -d "unknown" user.name

# 获取多值 key 的所有值
libra config get --all remote.origin.fetch

# 显示加密值
libra config get --reveal vault.env.GEMINI_API_KEY

# 正则搜索
libra config get --regexp "user\\..*"
```

#### `list`

列出活动 scope 中的所有配置条目。

| 标志 | 说明 |
|------|------|
| `--name-only` | 只显示 key 名，不显示值 |
| `--show-origin` | 为每个条目加上 scope 前缀（`local`、`global` 或 `system`） |
| `--vault` | 只显示 `vault.env.*` 条目 |
| `--ssh-keys` | 显示 SSH key 条目 |
| `--gpg-keys` | 显示 GPG key 条目 |

```bash
# 列出所有本地条目
libra config list

# 列出并显示 scope 标签
libra config list --show-origin

# 只列出 vault 环境条目
libra config list --vault

# 只列出 key 名
libra config list --name-only

# 列出 SSH keys
libra config list --ssh-keys
```

#### `unset <key>`

移除配置条目。

| 标志 | 说明 |
|------|------|
| `--all` | 移除该 key 的所有值（用于多值 key） |

```bash
# 移除一个 key
libra config unset user.signingkey

# 移除多值 key 的所有值
libra config unset --all remote.origin.fetch
```

#### `import`

从用户的 Git config（`.gitconfig`）导入配置。将相关条目复制到 Libra 的配置数据库。

```bash
# 从 Git 全局配置导入到 Libra 全局配置
libra config import --global

# 导入到本地配置
libra config import
```

#### `path`

打印活动 scope 的配置数据库文件系统路径。

```bash
# 显示本地配置路径
libra config path
# Output: /path/to/repo/.libra/libra.db

# 显示全局配置路径
libra config path --global
# Output: /home/user/.libra/config.db
```

#### `edit`

不支持。Libra 使用 SQLite 存储，无法安全地通过文本编辑器 round-trip。详情见[设计动机](#设计动机为何不同于-gitjj)。

#### `generate-ssh-key --remote <name>`

为命名远程生成 SSH 密钥对。私钥加密存储在 vault（`vault.ssh.<remote>.privkey`）中；公钥存储在 `vault.ssh.<remote>.pubkey`。

```bash
libra config generate-ssh-key --remote origin
libra config get vault.ssh.origin.pubkey
```

#### `generate-gpg-key`

生成用于提交签名或加密的 GPG 密钥对。

| 标志 | 说明 |
|------|------|
| `--name <name>` | key 使用的用户名（默认使用 `user.name` 配置） |
| `--email <email>` | key 使用的用户邮箱（默认使用 `user.email` 配置） |
| `--usage <usage>` | Key 用途：`signing`（默认）或 `encrypt` |

```bash
# 生成签名 key
libra config generate-gpg-key

# 使用显式身份生成加密 key
libra config generate-gpg-key --name "Jane Doe" --email "jane@example.com" --usage encrypt

# 获取公钥
libra config get vault.gpg.pubkey
```

### Scope 标志

这些标志是全局的（适用于任意子命令）：

| 标志 | 说明 |
|------|------|
| `--local` | 使用仓库配置（`.libra/libra.db`）。这是写入的默认值。 |
| `--global` | 使用全局用户配置（`~/.libra/config.db`）。 |
| `--system` | 使用系统级配置（`/etc/libra/config.db`，可经 `LIBRA_CONFIG_SYSTEM_DB` 覆盖）。级联优先级最低；写入通常需要提升权限。该作用域**不**支持 vault 加密密钥（见设计动机）。 |

### 隐藏的 Git 兼容标志

这些标志为 `git config` 调用模式提供向后兼容。它们从 `--help` 中隐藏。多数会翻译为等价子命令；`--remove-section` / `--rename-section` 是仅 flag 的 section 操作，没有 subcommand 形式。

| 标志 | 等价子命令 / 行为 |
|------|-------------------|
| `--get` | `get <key>` |
| `--get-all` | `get --all <key>` |
| `--unset` | `unset <key>` |
| `--unset-all` | `unset --all <key>` |
| `-l`, `--list` | `list` |
| `--add` | `set --add <key> <value>` |
| `--import` | `import` |
| `--get-regexp` | `get --regexp <key>` |
| `--show-origin` | `list --show-origin` |
| `--type=<bool\|int\|path>`、`--bool`、`--int`、`--path` | 读取（`--get`/`--get-all`/`--get-regexp`）**与设置**时规范化值：bool 变体 → `true`/`false`；int 支持可选 k/m/g（1024 倍率）；path 展开开头的 `~`/`~/`。设置时在存储前校验+规范化（与 git `config --type` 一致：`yes` → `true`、`1k` → `1024`），非法值报错且不写入。非 get/set 模式会被拒绝（exit 129）。 |
| `--remove-section <name>` | 在一个事务内删除 section `<name>` 的 key，采用 Git 的 section/subsection 身份（`--remove-section branch` 删 `branch.<key>` 但不动 `branch.feature.*` 子节）。section 不存在 → exit 128。 |
| `--rename-section <old> <new>` | 把 section `<old>` 的 key 搬到 `<new>`，保留每个值及其加密标志。源不存在 → exit 128；新旧同名 → exit 2；目标 section 已存在则拒绝 → exit 128。 |

### 其他标志

| 标志 | 说明 |
|------|------|
| `-d`, `--default <value>` | key 未找到时使用的默认值（Git 兼容位置模式） |
| `-z`, `--null` | NUL 分隔输出记录（`git config -z`）：`--get`/`--get-all` 输出 `value\0`；`--get-regexp`/`--list` 输出 `key\nvalue\0`；`--name-only` 输出 `key\0`；`--show-origin` 前缀 `origin\0`。`--json` 优先。仅作用于标准 config 输出；与 `--ssh-keys`/`--gpg-keys`/`--vault` 组合会被拒绝（exit 129）。 |
| `--json` | 输出结构化 JSON |
| `--quiet` | 抑制人类可读输出 |

## 常用命令

```bash
libra config set user.name "Jane Doe"
libra config get user.name
libra config list
libra config list --show-origin
libra config unset user.signingkey
libra config import
libra config path
```

## 人工输出

**`get`** 在单行打印值：

```
Jane Doe
```

**`list`** 打印 key-value 对：

```
user.name=Jane Doe
user.email=jane@example.com
core.editor=vim
```

带 `--show-origin`：

```
local   user.name=Jane Doe
global  user.email=jane@example.com
```

带 `--name-only`：

```
user.name
user.email
core.editor
```

**`set`** 成功时不打印任何内容（退出码 0）。

**`path`** 打印数据库路径：

```
/home/user/repo/.libra/libra.db
```

## 结构化输出（JSON 示例）

**`get`：**

```json
{
  "command": "config",
  "data": {
    "key": "user.name",
    "value": "Jane Doe",
    "origin": "local"
  }
}
```

**`list`：**

```json
{
  "command": "config",
  "data": {
    "entries": [
      { "key": "user.name", "value": "Jane Doe", "origin": "local" },
      { "key": "user.email", "value": "jane@example.com", "origin": "global", "encrypted": false }
    ]
  }
}
```

## Secrets 与 Vault 条目

当 key 匹配 Libra 的敏感 key 规则时，敏感 key 会加密存储，包括：

- `vault.env.*`
- `*.privkey`
- API keys、tokens、passwords 以及类似 secret 的 key

示例：

```bash
libra config set vault.env.GEMINI_API_KEY
echo "$SECRET" | libra config set --stdin vault.env.GEMINI_API_KEY
libra config set --encrypt custom.api_token "secret"
libra config get vault.env.GEMINI_API_KEY
libra config get --reveal vault.env.GEMINI_API_KEY
libra config list --vault
```

`--reveal` 对内部 vault 凭据（如 `vault.roottoken_enc` 和 `vault.ssh.<remote>.privkey`）会被阻止。

## 密钥管理

SSH keys 按远程生成并存储在 config 中：

```bash
libra config generate-ssh-key --remote origin
libra config get vault.ssh.origin.pubkey
libra config list --ssh-keys
```

GPG 公钥通过 config 暴露，而私有签名材料保留在 `vault.db` 内：

```bash
libra config generate-gpg-key
libra config generate-gpg-key --usage encrypt
libra config get vault.gpg.pubkey
libra config list --gpg-keys
```

支持的 `--usage` 值是 `signing` 和 `encrypt`。

## Scope

- 默认 scope 是 local（`.libra/libra.db`）
- `--global` 使用 `~/.libra/config.db`
- `--system` 使用 `/etc/libra/config.db`（可经 `LIBRA_CONFIG_SYSTEM_DB` 覆盖）；级联优先级最低，写入通常需要提升权限，且该作用域拒绝 vault 加密密钥（见设计动机）

## 保留命名空间 `upgrade.*`

自动升级配置是保留命名空间，存储在
`{LIBRA_HOME}/upgrade/settings.json`（默认 `~/.libra/upgrade/settings.json`；
可用 `LIBRA_HOME` 环境变量覆盖基目录；当 `LIBRA_CONFIG_GLOBAL_DB` 隔离全局
配置数据库时，settings 也随之落到该数据库所在目录），绝不落入 SQLite 存储。
仅支持以下单值、`--global` 操作：

| 操作 | 行为 |
| --- | --- |
| `set --global upgrade.mode <v>` | 仅接受 `auto`/`manual`/`off`（大小写不敏感）；其它值为用法错误。原子写入。 |
| `get --global upgrade.mode` | 读取存储的模式；文件缺失读作 `off`；文件损坏为硬错误（`LBR-UPGRADE-001`）。 |
| `unset --global upgrade.mode` | 将 `mode` 重置为 `off` 并**保留**文件。 |
| `list --global [--show-origin]` | 渲染文件承载的条目，origin 为 `file:{path}`。 |

所有其它可到达该命名空间的拼写均以用法错误 fail-closed（`LBR-CLI-002`，
exit 129）：local/system 作用域、`--add`、`--get-all`、`--unset-all`、
`--type` 类型转换、`--encrypt`/`--plaintext`/`--stdin`、
`--remove-section`/`--rename-section`、`--default`、多个 action 拼写组合、
带空白的 key/value 拼写（不做空白归一化），以及能匹配 `upgrade.mode` 的
`--get-regexp` 模式。`config import` 会跳过 `upgrade.*` 条目并给出警告；
`list` 与不匹配的 `--get-regexp` 模式会抑制 SQLite 中任何陈旧的
`upgrade.*` 行，确保 settings 文件是唯一事实来源。settings 文件损坏为
`LBR-UPGRADE-001`。

运行时由配置支撑的环境变量解析顺序是：

1. CLI 参数
2. 本地配置（`vault.env.<NAME>`）
3. 全局配置（`vault.env.<NAME>`）
4. 进程环境变量

如果必需 API key 没有由 Vault 条目或进程环境变量提供，Libra 会报告缺失 key，并要求你设置 `vault.env.<NAME>` 或导出 `<NAME>`。

## 设计动机（为何不同于 Git/jj）

### 为什么使用 SQLite 而不是文本文件？

Git 使用 INI 格式文本文件；jj 使用 TOML。Libra 使用 SQLite，因为：

1. **事务写入。** SQLite 提供 ACID 保证。不同于写到一半的文本文件，写入中崩溃不会损坏配置。当多个 AI agent 可能并发写配置时，这很关键。
2. **结构化查询。** 多值 key、前缀搜索和正则匹配都是 SQL 查询，而不是文本解析。这消除了一整类转义和解析 bug。
3. **集成加密。** Vault 加密值以加密 blob 形式与明文值一起存储在同一张表中。文本文件格式需要独立加密层或内联编码方案。

### 为什么使用 vault 加密？

Git 将配置存储在明文 INI 文件中，用来保存 API keys、access tokens 和 SSH/GPG 私钥本质上不安全。Libra 原生集成 Vault-backed 加密存储。敏感 key（如 `vault.env.*`、`*.privkey`，或包含 `secret`/`token` 等子串的 key）会在 local 和 global scope 中使用 AES-256-GCM 自动静态加密。这消除了“CLI 中已脱敏但磁盘上是明文”的虚假安全感，让开发者可以安全地把环境覆盖值直接存储在配置中。

### 为什么 `--system` 拒绝 vault 加密密钥？

`--system` 读写系统级纯配置 `/etc/libra/config.db`（可经 `LIBRA_CONFIG_SYSTEM_DB` 覆盖），级联优先级最低——类似 Git 的 `/etc/gitconfig`。写入通常需要提升权限；存在但不可读的系统 DB 在级联读取时被跳过，而非使其他用户的命令崩溃。

它有意**不**支持的是 vault：在系统作用域存储加密密钥（`vault.*` 键或 `--encrypt` 值）会以用法错误被拒绝。在多用户 OS 环境中，root 拥有的 `/etc/libra` 下的系统级 unseal key 要么对普通用户不可读（破坏解密），要么全局可读（破坏加密）。系统范围的*密钥*应在 OS/环境层处理；Libra 把 vault 限定在 `--global`（用户级）与 `--local`（仓库）作用域。

### 为什么没有 `config edit`？

Libra 使用 SQLite 数据库（`config_kv` 表），而不是明文文件。将数据库行导出到文本编辑器，再把 unified diff 解析回 SQL `UPDATE`/`DELETE` 语句是危险的。具体而言，对于多值 key（如 `remote.origin.fetch`），明文表示缺少行级主键。重新排序、部分修改或删除行会阻止 Libra 准确地将文本更改映射回数据库行，最终不可避免地导致数据丢失或损坏。为保证数据一致性，必须使用稳健的 `set`、`--add`、`unset` 和 `list` 命令。

### 为什么内置 SSH/GPG 密钥管理？

Libra 不把 SSH 私钥作为明文文件分散到文件系统，而是将它们加密存储在 config vault 中（`vault.ssh.<remote>.privkey`）。调用 SSH 传输时，key 会动态解密到临时文件（`chmod 600`），传给 SSH client，然后立即删除。GPG 私钥完全由 vault 内部 PKI engine 管理，绝不会导出到文件系统。

### 为什么将子命令风格作为主接口？

Git 使用 `git config key value`（隐式 set）和 `git config key`（隐式 get），这存在歧义：`git config foo` 可能是 get，也可能是不完整的 set。Libra 参考 jj，要求显式子命令（`set`、`get`、`list`、`unset`）。Git 兼容标志风格（`--get`、`-l` 等）作为迁移用隐藏别名保留，但文档化接口是子命令风格，因为它无歧义、可通过 `--help` 发现，也更容易让 AI agent 正确生成。

### 为什么使用 `--default` 而不是区分退出码？

Git 在 key 未找到时以代码 1 退出，这在脚本中与其他错误难以区分。Libra 的 `--default` 标志提供显式 fallback 值，让脚本和 agent 无需解析退出码就能处理缺失 key。

## 参数对比：Libra vs Git vs jj

| 功能 | Git | jj | Libra |
|------|-----|----|-------|
| 隐式 set | `git config key val` | 无（要求 `set`） | `libra config set key val` 加兼容的 `libra config key val` |
| 子命令风格 | 无 | 有（`set/get/list/edit/path`） | 有（`set/get/list/unset/import/path`） |
| 获取值 | `git config key` | `jj config get key` | `libra config get key` |
| 列表 | `git config -l` | `jj config list` | `libra config list` |
| 在编辑器中编辑 | `git config -e` | `jj config edit` | 不支持（SQLite 存储） |
| 正则搜索 | `git config --get-regexp` | 无 | `libra config get --regexp` |
| 显示来源 | `git config --show-origin` | 无 | `libra config list --show-origin` |
| 类型转换 | `--type=bool\|int\|path` | 无（TOML 类型） | `--type=bool\|int\|path` + `--bool`/`--int`/`--path`（读取与设置时均规范化） |
| 默认 fallback | `--default value` | 无 | `--default value` |
| Null 分隔 | `-z` | 无 | `-z` / `--null`（get/get-all 输出 `value\0`；`--get-regexp`/`--list` 输出 `key\nvalue\0`；`--name-only` 输出 `key\0`） |
| 重命名/移除 section | 有 | 无 | `--remove-section` / `--rename-section`（Git section/subsection 语义；rename 拒绝已存在的目标 section） |
| JSON 输出 | 无 | 无 | **`--json`** |
| Secret 脱敏 | 无 | 无 | **自动检测** |
| 从 Git 导入 | N/A | N/A | **`libra config import`** |
| Vault 加密 | 无 | 无 | **AES-256-GCM（仅 local/global；system 作用域拒绝）** |
| Env var vault | 无 | 无 | **`vault.env.*`** |
| 每个远程 SSH key | 无 | 无 | **`generate-ssh-key --remote`** |
| GPG key 生成 | 无 | 无 | **`generate-gpg-key`** |
| Env var 解析 | 无 fallback | 无 fallback | **CLI -> env -> repo -> global** |
| Config 文件路径 | N/A | `jj config path` | **`libra config path`** |
| 条件配置 | `includeIf` | `[[when]]` blocks | 不支持 |
| Worktree scope | `--worktree` | `--workspace` | 不支持 |
| 任意文件 | `--file <path>` | 无 | 不支持 |
| 存储格式 | INI 文本文件 | TOML 文本文件 | **SQLite + vault** |
| Scopes | system/global/local/worktree | user/repo/workspace | **system/global/local**（system：仅纯配置，无 vault；无 worktree 作用域） |
| 只列 key 名 | `--name-only` | 无 | **`--name-only`** |
| 多值 add | `--add` | 无 | **`set --add`** |
| Stdin 输入 | 无 | 无 | **`set --stdin`** |
| 强制加密 | 无 | 无 | **`set --encrypt`** |
| 强制明文 | 无 | 无 | **`set --plaintext`** |

## 错误处理

| 代码 | 条件 | 提示 |
|------|------|------|
| `LBR-REPO-001` | 不在 libra 仓库内（local scope） | 使用 `libra init` 初始化，或使用 `--global` |
| `LBR-CLI-002` | 在 `--system` 作用域使用 vault 加密密钥（`vault.*`/`--encrypt`） | vault 密钥改用 `--global` 或 `--local` |
| `LBR-CLI-003` | key 未找到且未提供 `--default` | 用 `libra config list` 检查 key 名称 |
| `LBR-CLI-002` | 使用了 `edit` 子命令（不支持） | 使用 `set`、`get`、`unset`、`list` 子命令 |
| `LBR-IO-001` | 读取配置数据库失败 | 检查 `.libra/libra.db` 的文件权限 |
| `LBR-IO-002` | 写入配置数据库失败 | 检查文件权限和磁盘空间 |

## 兼容性说明

- `libra vault` 已移除。请改用 `libra config generate-ssh-key`、`libra config generate-gpg-key` 和 `libra config get vault.*`。
- 不支持 `libra config edit`（见上方设计动机）。
- 旧仓库可能仍包含遗留的 `vault.gpg_pubkey` 条目；新写入使用 `vault.gpg.pubkey`。
