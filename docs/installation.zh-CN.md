# 安装 Libra

## 脚本安装器

macOS/Linux 推荐安装方式：

```bash
curl -fsSL https://download.libra.tools/install.sh | sh
```

### 信任模型：签名 stable 通道

两个安装器的默认路径都通过 **Ed25519 签名的 stable manifest**
（`https://download.libra.tools/libra/releases/stable/manifest-v1.json`）解析版本：
先用内嵌于安装器的公钥验证 manifest 签名，再按签名内的 `sha256` 与 `size`
逐字节校验下载的二进制。任何验证失败都会中止安装且不落盘——被攻破的镜像
无法替换二进制。

以下三类路径**不经过**签名 manifest 验证，且全部显式：

- **`-v <version>` / `-Version`** —— 指定历史版本会绕过 stable 通道，安装器会打印告警；
- **自定义镜像**（`LIBRA_BASE_URL` / `-DownloadBaseUrl`）—— 同样告警；
- **过渡态** —— 签名链尚未启用（stable manifest 不存在）或主机无法验签
  （`install.sh` 需要支持 Ed25519 的 OpenSSL ≥ 1.1.1 及 sha256 工具；
  `install.ps1` 自带验签器）时，安装器停下并要求显式确认或
  `LIBRA_ALLOW_FALLBACK=1` 才回退到未验证的旧路径，绝不静默降级。

内嵌公钥**没有任何运行时覆盖入口**：已发布安装器的信任根无法被任何 flag
或环境变量改变。安装脚本本身经同一 CDN 分发，脚本整体被替换不在本检查
覆盖范围内——带外信任锚（公布脚本哈希）在 UP-01 设计稿中单独跟踪。

安装器把 `libra` 放到 `$LIBRA_HOME/bin`（默认 `~/.libra/bin`），写入 shell
环境文件，并创建可选的相对 symlink：

```text
~/.libra/bin/lba -> libra
```

两个名称执行同一个二进制。整个 Libra home 目录移动后，相对目标仍有效。

## Windows

```powershell
irm https://download.libra.tools/install.ps1 | iex
```

`install.ps1` 为**当前用户**安装（无需管理员权限）：把 `libra.exe` 放到
`%LOCALAPPDATA%\libra\bin`，将该目录加入用户 `PATH`，并在第一个可写的 shim
目录（`%LOCALAPPDATA%\Microsoft\WindowsApps`，否则 `%USERPROFILE%\.local\bin`）
写入 `libra.cmd` 与可选的 `lba.cmd` 简写。

Windows 上非特权安装无法使用 symlink，因此别名是 `.cmd` shim 而非符号链接；
其余安全契约与 POSIX 一致：

- 安装器写出的每个 shim 都带 `@rem libra-managed-shim` 标记。该标记既让重装
  幂等（识别自己写的 shim），也让它能识别**不是自己写的**文件。
- 没有该标记的既有 `lba.cmd`，以及任何既有的 `lba.exe`、`lba.bat`、`lba.ps1`，
  一律不覆盖——`lba` 足够短，可能属于别的工具，而 Windows 会优先解析这些扩展名。
- `-NoAlias`（或 `LIBRA_NO_ALIAS=1`）跳过该简写。要传递此开关，请保存并执行
  安装脚本，而非将它管道传给 `iex`：

  ```powershell
  irm https://download.libra.tools/install.ps1 -OutFile install.ps1
  .\install.ps1 -NoAlias
  ```

- 写入 shim 失败只告警，不会导致安装失败。

## Alias 安全性与幂等性

- 全新安装默认创建 `lba`。
- 重复运行已安装的同一版本时，会修复缺失 alias，不下载或替换 `libra`。
- 有效的 `lba -> libra` 或 `lba -> $LIBRA_INSTALL_DIR/libra` symlink 会被接受，
  并刷新为相对形式。
- 名为 `lba` 的普通文件、目录或指向其他目标的 symlink 属于用户，绝不覆盖。
  安装器会警告并继续。
- 平台或文件系统无法创建 symlink 时，`libra` 安装仍成功；警告后使用完整
  `libra` 命令。

安装器不会创建二进制副本、hard link、shell function 或 profile alias。`lba` 只是
`libra` 旁的可选文件系统 symlink。

## 关闭 alias

单次调用使用 flag：

```bash
curl -fsSL https://download.libra.tools/install.sh | sh -s -- --no-alias
```

自动化安装可用环境变量：

```bash
curl -fsSL https://download.libra.tools/install.sh | LIBRA_NO_ALIAS=1 sh
```

opt-out 不会删除已有 alias，只阻止本次安装创建或刷新。如需删除已知属于
Libra 的 symlink，请显式执行：

```bash
test "$(readlink "$HOME/.libra/bin/lba" 2>/dev/null)" = libra &&
    rm "$HOME/.libra/bin/lba"
```

## 相关选项

| 选项 / 变量 | 效果 |
|-------------|------|
| `-v`, `--version <VERSION>` | 安装指定版本 |
| `-d`, `--dir <PATH>` | 覆盖二进制目录 |
| `LIBRA_INSTALL_DIR` | 二进制目录的环境变量形式 |
| `--no-modify-path` | 写 env 文件，但不编辑 shell rc 文件 |
| `--no-alias` | 本次不创建或刷新 `lba` |
| `LIBRA_NO_ALIAS=1` | `lba` 的环境变量 opt-out |

在 checkout 中运行 `sh install.sh --help` 可查看完整选项和环境变量列表。
