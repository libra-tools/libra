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

`libra config` 跨三个 scope 读写配置值：**local**（仓库级，存储在 `.libra/libra.db`）、**global**（用户级，存储在 `<XDG_CONFIG_HOME 或 ~/.config>/libra/config.db`；既有的 legacy `~/.libra/config.db` 在自动迁移版本发布前继续有效）和 **system**（机器级，存储在 `/etc/libra/config.db`；级联优先级最低，仅纯配置——无 vault）。各数据库都使用 SQLite 和 `config_kv` 表。

不同于 Git 的明文 INI 文件或 jj 的 TOML 文件，Libra 将配置存储在事务型数据库中，并集成 vault 加密。敏感值（API keys、tokens、SSH 私钥）会使用 AES-256-GCM 自动静态加密。

该命令支持两种调用风格：

1. **子命令风格**（推荐）：`libra config set key value`、`libra config get key`
2. **Git 兼容标志风格**（隐藏）：`libra config --get key`、`libra config key value`

使用 `get` 读取值时，Libra 会按优先级 local → global → system 级联查找。第一个匹配项胜出；system 库不可读时会被跳过。

### 裸读 `libra config <key>`

只给一个位置参数、不给值时是**读取**，与 `git config <key>` 一致：把已存储的值写 stdout 并以 0 退出；多值 key 返回**最后一个**值；级联顺序与 `get` 完全相同（local → global → system）；加密值渲染为 `<REDACTED>`（要明文用 `config get --reveal`）。key 未设置时以 **exit 1** + `LBR-CLI-002` 失败。`-z`/`--null` 与 `get` 上的行为一致：值以 NUL 而非换行结尾。

**与 Git 的有意差异**：对**受保护 key**——即 Libra 判定为机密的 key（`vault.env.*`、`auth.token.*`、`*.privkey`，或末段包含 `secret`、`token`、`password`、`credential`、`apikey`、`accesskey`、`privatekey`、`secretkey`）——裸读形式保留 Libra 的交互式安全赋值路径：它会**无回显地提示输入新值**，而不是打印已存储的值。没有终端时报 `missing value for protected key '<key>' (non-interactive environment)` 并以 2 退出。要读取受保护 key 请用 `libra config get <key>`，它返回 `<REDACTED>`。该差异已登记在 `COMPATIBILITY.md`。

## 配置 schema 兼容性

GlobalConfig 与 SystemConfig 使用独立的配置 ledger `configuration_schema_versions`。当前 manifest 已知的 Repository-only receipt（包括 `2026090801`）不会使配置库被误判为 future。未知或名称不匹配的 receipt、真正的配置 future schema 仍不受支持；remote/cloud 命令需要该作用域时，以 `LBR-CONFIG-001` fail-closed。

显式的 global/system 配置修改会把 configuration-owned legacy-reader barrier 与配置数据放在同一个事务内。该标记是 legacy ledger 中的保留 receipt，使固定旧版 `0.22.16` 等旧 binary 在写入前拒绝此库；本 build 只有在精确匹配标记且存在有效 configuration base receipt 时才承认它。事务失败时，标记和本次配置修改一起回滚，原有 legacy receipt 不会被覆盖或删除。

scoped get/list、默认值级联与 remote preflight 不写入 barrier；配置级联以只读方式查询，不创建缺失的库。此兼容性迁移只能前滚，旧 binary 必须升级；禁止通过删除 receipt 或手工编辑 SQLite 强行降级。识别受支持的 Repository receipt 不等于允许自动 repair；本版本对未知／不支持的状态仅提供升级路径。

全局路径为 `LIBRA_CONFIG_GLOBAL_DB` 或 XDG 配置目录（`$XDG_CONFIG_HOME/libra/config.db`，默认 `<home>/.config/libra/config.db`，各平台一致）；当只存在 legacy `<home>/.libra/config.db` 时，它仍是活动文件，因此读写不会分裂到两个库。`LIBRA_CONFIG_GLOBAL_DB` 是逐字覆写，同时禁用 XDG 默认与 legacy 回退。系统路径为 `LIBRA_CONFIG_SYSTEM_DB` 或 `/etc/libra/config.db`。完整的进程环境／repo-local 存储配置可以证明无需 GlobalConfig，但不能绕过 remote/cloud 对 SystemConfig 默认值的兼容性检查。只有在明确需要本地对象访问时才使用 `--offline` 或 `LIBRA_READ_POLICY=offline|local`，不能借此绕过远端同步安全检查。

## 只读 global schema doctor

使用 `libra config doctor --global-schema`，或
`libra --json config doctor --global-schema`，只检查全局 schema 元数据。
路径为解析后的 global 配置路径（env 覆写、XDG 默认或 legacy 回退）；无需仓库，不读取配置值、
vault、System/Repository DB，不运行迁移、写 barrier、创建备份或触发自动升级／恢复。
缺失目标保持缺失。冗余 `--global` 可用；`--local`、`--system` 和值／操作参数拒绝。
成对的 `--repair --confirm` 选择下方独立写入流程；单独使用其中任意选项均失败。

JSON envelope 的 `data.report_version=1`；human 与 JSON 使用同一报告，包含
`scope`、`role`、`path_source`、configured/canonical path、`exists`、`size_bytes`、
UTC `modified_at_utc`、`legacy_path`、`legacy_exists`、`migration_pending` 和
`configuration`／`legacy` ledger。`path_source` 取值为 `LIBRA_CONFIG_GLOBAL_DB`
（env 覆写）、`xdg`（绝对 `XDG_CONFIG_HOME`）、`home`（`<home>/.config/libra` 默认）或
`legacy`（旧的 `<home>/.libra/config.db` 仍在使用；此时 `migration_pending` 为 true，
`legacy_path`／`legacy_exists` 描述回退文件）。
每个 ledger 提供 `observed_version`、`latest_version`、`readable`、`verified_name`；
版本使用字符串，避免 `i64::MAX` 的 JSON 数值精度丢失。null 表示缺失或不可用，
不表示健康。仅显示通过 manifest 校验的 receipt 名称，不输出任意 receipt 文本或配置值。

`classification` 包括 `absent`、`compatible`、`upgrade_required`、
`unsupported_future`、`unsupported_receipt`、`unreadable`、`changed_during_inspection`。
成功完成诊断仍以 0 退出，包括不支持的库；自动化必须检查 classification，不能仅看退出码。
`issue` 只报告已证明不支持的 ledger/version；非法参数仍使用现有 CLI usage error。
`producer_disposition` 区分已登记但未归因的 Repository receipt、合法配置 barrier 与未知来源。
当前 manifest 将 `2026090801` 识别为 `operation_v2_branch_convergence`，但 receipt 或 mtime
不能证明历史 writer PID/binary。**默认只读 doctor 的 `repair_eligible` 始终为 `false`**。
不支持状态应升级到 producer-compatible build；禁止手工编辑 SQLite receipt，
doctor 也不是 remote-sync bypass。

使用标准 SQLite 只读 snapshot，不能以 `immutable` 跳过 live DB 的锁和变化检测。
WAL-mode 缺失正常 WAL/SHM 文件时，保守报告 `unreadable`，不打开 SQLite 来创建这些文件。
不要手工创建 sidecar；可在所属应用正常维护这些文件时重试，或诊断另行取得的 SQLite-consistent snapshot。
稳定目标的主 DB/WAL 内容及 mtime 不变；前后检查文件 identity、size、mtime，发现变化时报告
`changed_during_inspection`。检查并非文件系统锁，外部 rotation 可能与它竞态，SQLite 协调文件可能变化；
OS access time 与 SHM 协调状态不保证恒定。即使检查通过也不提供 repair 权限。

## 显式确认的 legacy global schema repair

需要 Libra v0.22.26 或更新版本；v0.22.25 仅提供只读 doctor。

修复与默认只读 doctor 分离，必须显式请求：

```sh
libra --json config doctor --global-schema
libra --json config doctor --global-schema --repair --confirm /absolute/canonical/path/config.db
```

请使用自己报告中的精确 `canonical_path`，不要复制示例路径。确认值必须是逐字节匹配的
绝对规范路径；配置路径中的符号链接／非规范部分、值操作参数及其他 scope 均拒绝。
不运行 Repository preflight、System DB 读取或自动升级。

当前只注册 **v0.22.19 Linux amd64 producer-format cohort**，并非出现 `2026090801`
就能修复。source revision 为 `b94bfe12f2ec2f039b88ddb5c6f8871787d60f17`，producer binary
SHA-256 为 `03447eb983178433425b5afffba351edae4044e7ded5b9e35c956dddb2bb68a6`。
完整 293 个 schema objects（含 SQLite 内部结构）、60 个原始 receipts 和运行时 manifest
都必须通过检查。Repository 数据／存储位置、未知表／trigger／receipt、被改动的 bootstrap
metadata 均不合资格。格式 attestation **不能归因某个真实文件的历史 writer/PID**；未知状态
仍须使用 producer-compatible binary，禁止改 receipt 来伪造匹配。

写入修复目前仅支持 **Unix** 的已验证本地文件系统：Linux ext-family、XFS、Btrfs、tmpfs、
overlayfs，以及 macOS APFS/HFS。Windows、其他／未知文件系统、网络文件系统在任何 repair
副作用之前拒绝。文件及直接父目录须属于当前 effective uid，且不可 group/world-writable；
文件只能有一个 hard link。不安全祖先和 SQLite sidecar 拒绝。私有固定锁
`config.db.schema-repair.lock` 串行化 repair，结束后保留，不自动删除。
这些措施无法抵抗同 uid/root 恶意进程；修复前应停止其他 writer、文件替换和 rotation 工具，
发现替换或并发提交时终止。

先由 SQLite `VACUUM INTO` 在不持有源库写事务的情况下建立一致逻辑备份，flush 后重新只读
打开，检查完整性与格式。目标旁的私有 `.libra-config-repair-<random>/`（0700）保留
`backup.sqlite`（0600）和 `recovery.json`。应用不枚举／解密配置值，由 SQLite 复制其逻辑
内容；备份属于敏感数据。失败或中断的副本保留为 **unverified**，不会自动删除或复用。

备份验证后才取得 SQLite 写锁，重新核对 manifest、attestation 和文件 identity；连接内 TEMP
nonce 配合 `data_version` 拒绝连接替换或其他进程的提交。一个事务初始化
`configuration_schema_versions`，并向 legacy ledger 追加配置拥有的
`configuration_legacy_reader_barrier`。原 receipts、配置行、加密标记及 sequence 高水位均保留；
不运行 Repository migration，不改变 journal mode，不显式 checkpoint。旧版 Repository-only
binary 必须在写入前拒绝 barrier；后续应使用兼容的新 binary，没有自动降级。

成功 JSON 为 `data.action="repair"`、`report_version=1`、`outcome="repaired"`、
`backup_path`、`backup_verified=true`、`committed=true` 及已注册格式／source hashes。
已经保护的配置返回 `outcome="already_protected"`，不创建备份、不写入、不声称 producer
attestation；这也不是完整健康诊断。默认 doctor 的只读报告与 `repair_eligible=false` 不变。

恢复必须显式进行。保留当前 DB 和 recovery directory；只有有效 `recovery.json` 中的
`backup_verified=true` 才能作为考虑恢复 `backup.sqlite` 的前提，缺失／中断／未验证状态
不能证明备份可用。崩溃或 commit 结果不明确时先跑只读 doctor，过时的状态文件不能决定事务
是否提交。停止所有使用该库的进程，核验备份完整性及来源，保存当前 DB 和 sidecars 供分析，
再由维护者将已验证一致备份作为整体恢复并设置正确私有权限。禁止覆盖 live DB、将旧 WAL/SHM
混入恢复后的主库、恢复未验证副本或手工修改 receipt；保留 producer-compatible binary。

非法确认使用 `LBR-CLI-002`，格式／路径／权限资格拒绝使用 `LBR-CONFIG-001`，锁、备份与事务
失败使用 `LBR-IO-002`。错误不包含配置值或原始 SQLite schema 错误。备份存在不代表 repair
已提交；应检查 commit 状态，不确定时重新诊断。

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
# Output: /home/user/.config/libra/config.db
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
| `--global` | 使用全局用户配置（`<XDG_CONFIG_HOME 或 ~/.config>/libra/config.db`）。 |
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
- `--global` 使用 `<XDG_CONFIG_HOME 或 ~/.config>/libra/config.db`（legacy `~/.libra/config.db` 在自动迁移前仍作为回退）
- `--system` 使用 `/etc/libra/config.db`（可经 `LIBRA_CONFIG_SYSTEM_DB` 覆盖）；级联优先级最低，写入通常需要提升权限，且该作用域拒绝 vault 加密密钥（见设计动机）

## `code.defaultProvider` 键

`libra code` 在启动时一次性解析生效 provider；`code.defaultProvider` 是该阶梯中的持久化槽位（显式 `--provider` → `--agent` 绑定 → 被恢复线程记录的 provider（`--resume`）→ **`code.defaultProvider`** → 凭据探测）：

```bash
libra config set --global code.defaultProvider deepseek   # global 默认
libra config set code.defaultProvider zhipu               # repo-local 覆盖
libra config get code.defaultProvider
libra config unset --global code.defaultProvider
```

合法取值即 `libra code --provider` 接受的 provider id：`anthropic`、`codex`、`deepseek`、`gemini`、`kimi`、`ollama`、`openai`、`zhipu`。配置命中时跳过凭据探测；未设置或空值下探到探测；无法识别的 id 使 `libra code` 以 129（`LBR-CLI-002`）退出并列出合法取值，且不回显已存储的值。该键只存放于本 SQLite config 数据库——与 `agents.toml` 的 `[code.*]` profile 段（`[code.multi_agent]`、`[code.goal]` 等）无关，两个载体互不回退。完整解析阶梯见 [code.md](code.md)。

## `core.filemode` 键

`core.filemode`（按大小写不敏感读取）决定 `add`、`update-index <path>` 与 `commit -a` 从工作树暂存时如何记录文件 mode。未设置时 Unix 默认为 `true`、其它平台为 `false`。为 `false` 时：重新暂存已有条目沿用索引中已记录的 mode，新路径记为 `100644`（工作树中的可执行文件不会被记为 `100755`）；`add --chmod=+x` 与 `update-index --cacheinfo` 携带显式 mode，不受影响。为 `true`（Unix 默认）时，仅 mode 变化（已跟踪普通文件的 owner-execute 位与索引不同、内容未变）会被 `status` 报告、被 `diff` 渲染、被 `add`/`commit -a`/`update-index <path>` 暂存，并被 `stash push` 视为本地修改；为 `false` 时这些命令忽略仅 mode 的差异，但条目类型变化（如普通文件被替换为符号链接）仍会显示。非法布尔值在任何索引写入前以 `bad boolean config value '<value>' for 'core.filemode'` 使 `add`/`status` fail-closed（与 `commit.verbose` 同一映射）。

```bash
libra config set core.filemode false
libra config get core.filemode
```

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
