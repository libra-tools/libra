# `libra init`

创建空 Libra 仓库，或重新初始化已有仓库。

## 概要

```
libra init [OPTIONS] [DIRECTORY]
```

## 说明

`libra init` 创建新的 Libra 仓库，在 `.libra/libra.db` 中初始化 SQLite-backed 元数据，配置 `HEAD`，并可选导入已有本地 Git 仓库。

在已有目录中运行 `libra init` 会创建 `.libra` 子目录，其中包含对象存储、SQLite 数据库、默认配置、指向初始分支的 HEAD，以及（默认）vault-backed PGP 签名密钥。非 bare 仓库还会获得一个可见的根 `.libraignore` 文件用于 ignore 规则。如果给出 `DIRECTORY` 且该目录不存在，会先创建该目录。

提供 `--from-git-repository` 时，会从源 Git 仓库导入对象和 refs，并配置 `origin` 指向源分支布局。转换后的仓库会把源仓库实际 `HEAD` 分支报告为 `initial_branch`；不会使用 `init.defaultBranch` 重命名或错误报告导入分支。在源工作树或已 checkout 导入中发现的任何 `.gitignore` 文件都会复制为匹配的 `.libraignore` 文件。

在已初始化的仓库中再次运行 `libra init` 是安全的：与 `git init` 一致，它会就地重新初始化，打印 `Reinitialized existing Libra repository in <path>`，补齐缺失的标准布局（模板、目录）并重新应用 `--shared`，同时保留现有仓库数据——配置、`HEAD`、refs、对象、vault 与仓库 id 均不受影响。打开数据库仍可能应用 schema 迁移；保留数据不等于保留旧数据库结构。当 `--initial-branch`/`--object-format` 与现有仓库不一致时会被忽略（并给出警告）；`--from-git-repository` 在已初始化的仓库上会被拒绝。

不能把用户级 Libra home（`LIBRA_HOME`，默认 `~/.libra`）作为仓库存储根。该目录保存用户状态；全局配置保存在 `~/.libra/config.db`（或 `LIBRA_CONFIG_GLOBAL_DB` 指定的位置）。当这两个目录不同时，两者都受保护。检查同样适用于尚不存在的目录、路径别名和 linked worktree 的 `commondir` 目标。

仓库发现会忽略 home 中遗留的 `libra.db`，因此从用户主目录初始化项目时会读取真正的全局配置。打开旧仓库或全局配置数据库时，迁移会自动补建缺失的旧版 `config` 表，保留已有配置；被忽略的 home `libra.db` 文件保持原样。

### Operation-v2 收敛（当前分支，未发布）

`2026090801` 收敛迁移已在当前未发布分支实现，并通过 focused 迁移验证，包括真实旧 binary 创建的受控仓库升级。整树集成与发布验收仍未完成。

- 单向迁移 `2026090801` 必须保留原始 `2026090101`（`operation_v2`）与 `2026090601`（`legacy_config_table`，#472）迁移收据，包括已有收据时间戳。已应用 #472、仍使用 v1 operation 的仓库需要补齐缺失的 operation-v2 迁移，而不只是提高最大版本号。
- 已有 v1 operation 数据必须保留在 `legacy_operation*` 命名空间；已有 `config`、`config_kv` 配置值与 #472 缺表修复必须保留。分支收敛不授权删除 legacy 数据。
- `2026090801` 成功提交后，最高仅支持 `2026090601` schema 的旧 binary 必须在使用不兼容的 operation 表之前拒绝打开较新的仓库 schema。不能仅凭软件包版本字符串判断 binary 支持的 schema。
- 升级真实数据库前，先保留经过验证的一致性升级前备份：使用 SQLite online-backup 机制，不要只复制运行中数据库的主文件而遗漏 WAL。保护好备份并保留旧 binary。Linked worktree 共享仓库数据库，不能充当隔离的升级测试环境。
- 此迁移没有 down 路径。运行旧 binary、再次执行 `init` 或删除迁移收据都不是降级流程。Binary 安装回滚不会恢复数据库；恢复需要匹配的升级前备份与 binary。

另见 [operation 捕获边界](op.md#当前分支的捕获契约未发布)与[收敛原理](../../development/commands/op.md#current-branch-convergence-contract-unreleased)。

## 选项

### `[DIRECTORY]`

指定要初始化的目录的位置参数。省略时默认为 `.`（当前工作目录）。

```bash
libra init my-project          # creates ./my-project/.libra
libra init                     # creates ./.libra
```

### `--bare`

创建 bare 仓库。Bare 仓库没有工作树，用作中央远程目标。仓库目录本身会成为对象存储。

```bash
libra init --bare my-repo.git
```

### `-b, --initial-branch <NAME>`

覆盖初始分支名称。对新仓库省略该标志时，Libra 会按 local、global、system 顺序读取
`init.defaultBranch`（变量名大小写不敏感），未设置时回退到 `main`。本地和全局的加密值会先解密再校验；本地/全局配置数据库不可读时以 `LBR-IO-001` 失败，不可读或不支持的 system 配置 scope 会跳过。例外：schema 比二进制新的全局配置库会在一次性警告后被跳过而不失败（见 `LBR-CONFIG-001`）。对于 `--from-git-repository`，源仓库的 `HEAD` 分支优先，避免导入结果被配置默认值误报。分支名会按与
`git check-ref-format` 相同的规则验证：无空格、无 `..`、无 ASCII 控制字符，最大 255
字符。空值或非法配置在写入仓库布局前以 `LBR-CLI-002` 失败。

```bash
libra init -b develop
libra init --initial-branch trunk
```

### `--object-format <FORMAT>`

设置对象哈希算法。可接受值为 `sha1`（默认）和 `sha256`。

```bash
libra init --object-format sha256
```

### `--from-git-repository <PATH>`

从已有本地 Git 仓库导入对象和 refs。源仓库必须包含有效的 `HEAD`、`config` 和 `objects` 结构。会配置一个指向导入分支布局的 `origin` 远程，结果中的 `initial_branch` 与人类输出报告源仓库的 `HEAD` 分支；此路径不读取 `init.defaultBranch`。空 Git 仓库（无 refs）会产生错误。

对于非 bare 导入，Libra 会把能看到的每个 `.gitignore` 转换为同级 `.libraignore`。已有用户拥有的 `.libraignore` 文件会被保留，并在结构化输出中作为 warnings 报告。

```bash
libra init --from-git-repository ../old-project
```

### `--vault <BOOL>`

启用或禁用 vault-backed PGP 签名。默认为 `true`。启用时，Libra 会在初始化期间生成 PGP 签名密钥并将其存储在 vault 中。设为 `false` 可完全跳过 vault 设置。

```bash
libra init --vault false
```

### `--template <PATH>`

模板目录路径，其内容会复制到新的 `.libra` 目录。

```bash
libra init --template /path/to/template
```

### `--shared <MODE>`

指定仓库将在多个用户之间共享（镜像 Git 的 `--shared` 标志，用于组权限）。支持
`false`、`umask`、`true`、`group`、`all`、`world`、`everybody`，或 `0770`
这类 4 位八进制模式。

`true` 会规范化为 `group`；`world` 与 `everybody` 会规范化为 `all`。只要显式
传入 `--shared`，Libra 就会把规范化后的值写入 `core.sharedRepository`，因此
fresh init 与 reinit 之后都可以用 `libra config get core.sharedRepository` 观察当前
shared 模式。

Numeric mode 会在任何仓库布局写入前预校验。当前 Libra 会把 numeric mode 直接应用到仓库目录和文件，因此 owner 必须保留 `rwx`，且 group/other 任一类只要获得 read 或 write 权限，就必须同时具备对应的 execute bit 以保证目录可遍历。`0660` 这类不可遍历模式会以 `LBR-CLI-002` 拒绝，并且不会留下半初始化的 `.libra` 目录。

```bash
libra init --shared group shared-repo
libra init --shared 0770 shared-repo
```

### `--ref-format <FORMAT>`

设置引用存储格式。可接受值：`strict`、`filesystem`。

### `-q, --quiet`

抑制进度和成功输出。只打印错误。

```bash
libra init -q my-project
```

## 常用命令

```bash
libra init
libra init my-project
libra init --bare my-repo.git
libra init -b develop
libra init --object-format sha256
libra init --from-git-repository ../old-project
libra init --vault false
libra init --shared group shared-repo
```

## 人类可读输出

默认人类模式将分阶段进度写到 `stderr`，最终确认写到 `stdout`。

阶段包括：

- `Creating repository layout ...`
- `Initializing database ...`
- `Setting up refs ...`
- 使用 `--from-git-repository` 时的 `Converting from Git repository at ...`
- 启用 vault signing 时的 `Generating PGP signing key ...`

成功输出使用过去时：

```text
Initialized empty Libra repository in /path/to/repo/.libra
  branch: main
  signing: enabled
```

`--quiet` 会抑制进度和最终成功摘要。

## 结构化输出

`libra init` 支持全局 `--json` 和 `--machine` 标志。

- `--json` 向 `stdout` 写入一个成功信封
- `--machine` 以紧凑单行 JSON 写入相同 schema
- 两者都会抑制进度输出
- 成功时 `stderr` 保持干净，包括 `--from-git-repository`

示例：

```json
{
  "ok": true,
  "command": "init",
  "data": {
    "path": "/path/to/repo/.libra",
    "bare": false,
    "initial_branch": "main",
    "object_format": "sha1",
    "ref_format": "strict",
    "repo_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
    "vault_signing": true,
    "converted_from": null,
    "ssh_key_detected": "/Users/alice/.ssh/id_ed25519",
    "warnings": [],
    "reinitialized": false
  }
}
```

## 设计理由

### 使用 SQLite 而不是扁平文件存储元数据

Git 将配置存储在扁平 `.git/config`（INI 格式）中，将 refs 作为 `.git/refs/` 下的单独文件，将 reflogs 作为追加文本文件。这种方式在并发写入时会遭遇竞态，需要目录级锁（`*.lock` 文件），并且在没有 `packed-refs` 机制时无法进行原子多引用更新。

Libra 将所有元数据（config、refs、reflogs、rebase state）存储在 `.libra/libra.db` 的单个 SQLite 数据库中。SQLite 提供 ACID 事务、通过 WAL 模式实现并发读/单写语义，以及无需扫描文件系统的高效查询。这种设计消除了困扰网络文件系统（NFS、CIFS）上 Git 的一整类损坏 bug，并让“查找所有匹配模式的分支”这类操作从目录遍历变为 O(log n)。

### 默认启用 Vault 签名

现代开发工作流越来越需要提交来源证明（供应链安全的签名提交、CI 中的验证合并）。Git 将签名留作手动 opt-in，并需要外部 GPG/SSH 密钥管理。Libra 采取相反立场：在 `init` 时启用 vault-backed PGP 签名，并自动生成密钥。不需要签名的开发者可以用 `--vault false` 退出，但 secure-by-default 路径意味着新仓库无需额外设置即可立即用于验证工作流。

### 没有 `--separate-git-dir` / `--separate-libra-dir`

Git 支持通过 `--separate-git-dir` 将 `.git` 目录与工作树解耦，创建一个 `gitdir:` 指针文件。该功能很少使用，却为每个路径解析例程增加复杂度，并在指针文件或目标目录被独立移动时造成微妙破坏。Libra 移除此功能，始终将 `.libra/` 与工作树根共址，简化仓库发现算法并消除一种用户困惑来源。

### `--from-git-repository` 而不是 Git 缺失的导入

Git 没有在 init 时从另一种 VCS 格式导入到自身的内置概念；最接近的等价操作是 `git clone --local`。jj 提供 `jj git init --git-repo` 以通过 Git 后端共址操作。Libra 的 `--from-git-repository` 提供一次性、单向导入，将对象和 refs 从本地 Git 仓库复制到新的独立 Libra 仓库。这是有意的设计选择：Libra 不是像 jj 那样包装 Git，而是创建完全独立的 `.libra` 存储，使其成为独立 VCS，而不是 Git 前端。

### 默认分支遵循 `init.defaultBranch`

Libra 按照 Git 风格先读取当前仓库的 `init.defaultBranch`，再读取全局和系统配置；未配置时回退到 `main`。本地/全局加密值会先解密；本地/全局读取失败以 `LBR-IO-001` 失败，不可读或不支持的 system scope 会跳过。例外：schema 比二进制新的全局配置库会在一次性警告后被跳过而不失败（见 `LBR-CONFIG-001`）。使用 `-b`（`--initial-branch`）会覆盖新仓库的配置值。空值或无效分支名会在创建仓库前以 `LBR-CLI-002` 失败。`--from-git-repository` 例外地使用源 `HEAD` 分支并报告它，不使用这个默认值。

### jj 对比

jj（`jj git init`）包装 Git 后端，不创建自己的对象存储；它将 jj 特定元数据（operation log、view）与 `.git` 目录并存。Libra 创建完全独立的 `.libra` 存储，拥有自己的对象格式，使其成为独立 VCS，而不是 Git 前端。`--from-git-repository` 标志提供一次性导入路径，而不是持续共居。

## 参数对比：Libra vs Git vs jj

| 参数 / 标志 | Git | jj | Libra |
|---|---|---|---|
| 在当前目录初始化 | `git init` | `jj git init` | `libra init` |
| 在具名目录初始化 | `git init <dir>` | `jj git init <dir>` | `libra init <dir>` |
| Bare 仓库 | `git init --bare` | 无直接等价 | `libra init --bare` |
| 初始分支名 | `git init -b <name>` / `--initial-branch` | 无直接标志（使用 `trunk()` revset config） | `libra init -b <name>` / `--initial-branch` |
| 对象哈希格式 | `git init --object-format=sha256` | 从 Git 后端继承 | `libra init --object-format sha256` |
| 模板目录 | `git init --template=<dir>` | N/A | `libra init --template <dir>` |
| 共享权限 | `git init --shared[=<mode>]` | N/A | `libra init --shared <mode>` |
| 独立存储目录 | `git init --separate-git-dir=<dir>` | `jj git init --colocate` | 已移除 |
| 从 Git 仓库导入 | N/A（使用 `git clone --local`） | `jj git init --git-repo <path>` | `libra init --from-git-repository <path>` |
| Vault / 签名 bootstrap | N/A（手动 GPG/SSH 设置） | N/A | `libra init --vault <bool>`（默认：true） |
| Ref 存储格式 | `git init --ref-format=<format>`（Git 2.45+） | N/A | `libra init --ref-format <format>` |
| Quiet 模式 | `git init -q` / `--quiet` | N/A | `libra init -q` / `--quiet` |
| 结构化 JSON 输出 | N/A | N/A | `libra init --json` / `--machine` |
| 递归 submodules | `git init` + `git submodule init` | N/A | N/A（不支持 submodules） |

## 错误处理

每个 `InitError` 变体都会映射到显式 `StableErrorCode`。

| 场景 | 错误码 | 退出码 | 提示 |
|----------|-----------|------|------|
| 无效参数（错误分支名、错误格式） | `LBR-CLI-002` | 129 | 因参数而异 |
| `init.defaultBranch` 为空或无效 | `LBR-CLI-002` | 129 | 修复 local/global 值或使用 `--initial-branch <name>` |
| local/global 默认配置不可读 | `LBR-IO-001` | 128 | 修复错误中列出的数据库，或使用 `--initial-branch <name>`；`libra config --global` 无法修复仓库的本地数据库 |
| 仓库存储根为用户级 Libra home | `LBR-CLI-002` | 129 | 选择独立项目目录，例如 `libra init <project>` |
| 在已初始化仓库上使用 `--from-git-repository` | `LBR-CLI-002` | 129 | "convert into a fresh directory instead" |
| 找不到源 Git 仓库 | `LBR-IO-001` | 128 | -- |
| 源不是有效 Git 仓库 | `LBR-CLI-003` | 129 | "a valid Git repository must contain HEAD, config, and objects" |
| 找不到模板目录 | `LBR-IO-001` | 128 | -- |
| 路径不是有效 UTF-8 | `LBR-IO-001` | 128 | -- |
| 从 Git 转换失败 | `LBR-REPO-003` | 128 | -- |
| Vault 初始化失败 | `LBR-INTERNAL-001` | 128 | Issues URL |
| I/O 错误（权限、磁盘） | `LBR-IO-001` | 128 | -- |
| 数据库初始化失败 | `LBR-INTERNAL-001` | 128 | Issues URL |

## Vault 和身份

- 默认启用 vault-backed signing
- `--vault false` 跳过 vault 设置并写入 `vault.signing=false`
- 启用 vault signing 时，Libra 按以下顺序解析身份：
  1. 目标仓库本地配置
  2. 全局配置
  3. `GIT_COMMITTER_*`、`GIT_AUTHOR_*`、`EMAIL`、`LIBRA_COMMITTER_*`
  4. 内置回退：`Libra User <user@libra.local>`

这有意比 `libra commit` 更宽松：缺失身份不会阻止仓库创建。

## Git 导入

`--from-git-repository <path>` 从本地 Git 仓库获取对象和 refs，并配置 `origin` 以及导入的分支布局。

- 源路径必须指向有效本地 Git 仓库
- JSON 输出中的 `converted_from` 报告规范源 Git 目录
- 空 Git 仓库会以 repo-state 错误失败，因为没有 refs 可导入

## 兼容性说明

- `--separate-libra-dir` 和 `--separate-git-dir` 已移除
- 非 bare 仓库始终在工作树内使用标准 `.libra/` 布局
- 不再检测曾使用 `gitdir:` `.libra` 链接文件的历史仓库

旧 separate-layout 仓库迁移：

```bash
rm .libra
mv /path/to/separate/storage .libra
```
