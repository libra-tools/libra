### `cli.config-key-generation`

目的：覆盖 `generate-ssh-key --remote <NAME>` 和 `generate-gpg-key --name <NAME> --email <EMAIL> --usage <KIND>`。

最小步骤：

```bash
SCENARIO="cli.config-key-generation"
RUN_DIR="$RUN_ROOT/repos/$SCENARIO"
mkdir -p "$RUN_DIR"
cd "$RUN_DIR"
# (prelude provides libra() -- converged short form, long wrapper removed)

mkdir -p keygen-home
libra init keygen-repo
cd keygen-repo
libra config set user.name "Keygen User"
libra config set user.email "keygen@example.invalid"
libra remote add origin git@example.invalid:owner/repo.git

libra config generate-ssh-key --remote origin
libra config get vault.ssh.origin.pubkey

libra config generate-gpg-key --name "Signing User" --email "signing@example.invalid" --usage signing
libra config get vault.gpg.pubkey
libra config get vault.signing

libra config generate-gpg-key --name "Encrypt User" --email "encrypt@example.invalid" --usage encrypt
libra config get vault.gpg.encrypt.pubkey
```

负向步骤：

```bash
cd "$RUN_DIR/keygen-repo"
! libra config --global generate-ssh-key --remote origin
! libra config generate-ssh-key --remote bad.name
! libra config generate-ssh-key --remote no-such-remote
! libra config --global generate-gpg-key --name Bad --email bad@example.invalid
! libra config generate-gpg-key --usage archive
```

断言：SSH key 生成要求 remote 存在且 remote 名只含 `[a-zA-Z0-9_-]`；生成后 public key 可通过 config 读取，private key 只以 vault-encrypted config key 存在且不得出现在日志中；GPG signing usage 写入 `vault.gpg.pubkey` 并启用 `vault.signing`，encrypt usage 写入 `vault.gpg.encrypt.pubkey`；global key generation 和非法 usage 必须失败且无本地副作用。

补充可执行断言（安全关键场景）：
- 生成 SSH key 后，`libra --json config get vault.ssh.origin.pubkey` 必须 `ok:true` 且包含公钥内容。
- 生成 GPG signing key 后，`libra --json config get vault.signing` 必须显示启用状态。
- 验证 private key 绝不泄露：`libra config list --vault` 输出中不得出现私钥材料（仅 pubkey 或 key 名称）。
- 负向 `--global generate-ssh-key` 必须非 0，且错误提示隔离要求。
- 非法 usage（如 archive）必须失败，stderr 包含 "usage" 相关错误或 LBR- 码。
- 操作全程使用隔离 HOME + global DB，结束后验证真实用户 vault 未被触碰（通过检查隔离环境外无新 key）。

