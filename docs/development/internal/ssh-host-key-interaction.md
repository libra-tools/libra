# SSH Host-Key Interaction Design（issue #582, HKT-00）

> 本文是 [`issues/582.md`](../plan/issues/582.md) HKT-00 的產物：固定外部參照、定義不回放遠端輸出的
> host-key interaction state machine，並把既有重疊所有權正式移交給 #582。設計於 2026-09-28 定稿（GO）；
> 實作與驗證由 HKT-01／HKT-02 依此 seam 交付，於 2026-11-01 以 Closed 收口，GitHub Issue #582 已關閉。
>
> **核對日期：** 2026-09-28（設計與實作 seam 固定於此日；實作落點與測試錨點於 HKT-01/HKT-02 開工日更新）。

## 固定參照

本節把比較用的 Git 與 OpenSSH 來源固定為具體版本、URL、檔案路徑、核對日期與可重現觀察命令
（DEP-HKT-03）。任何「Git 相容」主張都必須以這些固定 revision 為準，不使用浮動 branch。

| 來源 | 固定 revision | 來源 URL | 檔案路徑 | 核對日期 |
|---|---|---|---|---|
| Git（Apple fork，本機） | `git version 2.54.0 (Apple Git-157)` | https://github.com/Apple-Open-Source/git ／ 上游 https://github.com/git/git | `builtin/clone.c`（`transport` host-key 流程）、`connect.c`（`ssh` host key 處理）、`submodule.c` | 2026-09-28 |
| OpenSSH（本機） | `OpenSSH_10.3p1, LibreSSL 3.3.6` | https://github.com/openssh/openssh-portable（tag `V_10_3` 附近） | `readconf.c`（`UserKnownHostsFile`/`GlobalKnownHostsFile`）、`sshconnect.c`（host-key 確認提示、`known_hosts` 比對） | 2026-09-28 |
| `ssh-keyscan` | 隨 OpenSSH `10.3p1` 排程（`/usr/bin/ssh-keyscan`） | 同上 | `ssh-keyscan.c`（輸出 `<host> <type> <base64>`） | 2026-09-28 |

**可重現觀察命令**

```sh
# 本機版本
git --version
ssh -V 2>&1
ssh-keygen -lf - <<< "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI...  # 驗證 SHA256: 指紋格式
ssh-keyscan -t ed25519,ecdsa,rsa -p 22 localhost   # 觀察 keyscan 輸出行格式

# 觀察 OpenSSH 對未知主機的提示（人工，需 TTY）
ssh -o BatchMode=no -o StrictHostKeyChecking=ask localhost
# → "The authenticity of host 'localhost (127.0.0.1)' can't be established.\n
#    ED25519 key fingerprint is SHA256:....\n
#    Are you sure you want to continue connecting (yes/no/[fingerprint])?"
```

### keyscan → 指紋的確定性計算

OpenSSH 的 `ssh-keygen -lf` 對主機金鑰輸出 `SHA256:<base64>`，其中 `<base64>` 是對金鑰 blob
（含 key type 前的 wire-format 位元組）取 SHA-256 後以 base64（無 padding）編碼。`ssh-keyscan`
輸出的一行 `<host> <keytype> <base64>`，其 `<base64>` 即為金鑰 blob 的 base64。因此下列計算可
確定性得到與 OpenSSH 一致的指紋（GC-HKT-02：每個可見字元都來自已驗證 metadata）：

```
blob = base64_decode(keyscan_line 的第三欄)
fingerprint = "SHA256:" + base64_no_padding(sha256(blob))
```

對比測試（HKT-02 fixture）以 `ssh-keygen -E sha256 -lf` 的輸出作為期望值，證明本實作與 OpenSSH 一致。

## 安全邊界（ADR-HKT-01 落實）

- **無 raw replay：** 遠端 stdout、stderr、banner、ANSI 控制序列、protocol bytes 都不會進入 prompt、
  終端、log 或 JSON/machine 輸出。目前 capture 契約（`SSH_STDERR_LIMIT` 64 KiB、有界 digest、遇
  `Untrusted` 不得回顯）保持不退化。
- **無盲目 keyscan 寫入：** `ssh-keyscan` 只作為「取得 host key 並計算指紋」的來源；未經使用者明確
  接受前不寫入 `known_hosts`。接受後才以 `ssh -G` 解析出的有效 host token（`HostKeyAlias`，否則
  `host` 或 `[host]:port`）把 keyscan 的 `<type> <blob>` 寫入 OpenSSH 實際使用的 known-hosts 目的地。
- **changed key 不接受：** 只對 typed `Untrusted` 進入互動流程；`Changed` 維持既有 `LBR-NET-001`
  固定指引，沒有 accept/fingerprint/retry prompt（ADR-HKT-03）。

## 狀態機

下列矩陣為唯一權威定義；HKT-02 的 fixture 測試逐格回歸。`Eligible = 有效政策為 ask 且 human 輸出
且 stdin/stdout/stderr 皆為 TTY`。`Untrusted`/`Changed` 由既有 typed classifier 判定（exit 255 +
保留 stderr 內固定 pattern）。

| 分類 | 政策 | TTY / 輸出 | 行為 | 信任寫入 | 錯誤結果 |
|---|---|---|---|---|---|
| Known | 任意 | 任意 | 正常 batch transport，成功 | 無 | 成功 |
| Unknown（Untrusted） | `ask` | human + TTY | 進入互動確認：`ssh -G` 解析有效 host/port → keyscan → 指紋 prompt → 接受後以有效 host token 寫 `known_hosts` → 以 batch transport 重試 | 接受後一條 | 成功 |
| Unknown（Untrusted） | `ask` | human + 無 TTY 或 JSON/machine | 不進入互動，維持既有固定 `LBR-NET-001` unknown 指引 | 無 | 既有 unknown 錯誤 |
| Unknown（Untrusted） | `yes`/`accept-new`/`no` | 任意 | 完全轉交 OpenSSH（HKT-01），不顯示 Libra prompt | 由 OpenSSH | 依 OpenSSH 政策 |
| Unknown（Untrusted） | `ask` | human + TTY，但使用者拒絕/EOF/取消/timeout | 不寫入信任，不開始 packet-line transfer | 無 | 既有穩定 network error 契約（unknown 指引） |
| Changed | `ask` | human + TTY | 不進入互動；維持既有 `LBR-NET-001` changed 指引 | 無 | 既有 changed 錯誤 |
| Changed | 其他政策 | 任意 | 同左，OpenSSH 政策 | 無 | 依政策 |

### 互動確認的子狀態機（僅 `Untrusted` + Eligible）

```
Untrusted + Eligible
  → ssh -G <user@host>（本地 config dump）解析 HostName/Port/HostKeyAlias/UserKnownHostsFile
      ├─ 無 hostname（不支援 -G）→ 回退 URL host/port 與既有 known-hosts 目的地解析
      └─ 有 hostname → 以有效 host/port 續行
  → ssh-keyscan -t ed25519,ecdsa,rsa [-p 有效 port] 有效 host
      ├─ 無輸出（keyscan 失敗/無 key）→ 回傳既有 unknown 指引（不寫、不阻塞）
      └─ 有輸出 → 對每個 key 計算「algorithm / SHA256:…」指紋
          → 寫入 prompt: 「The authenticity of host '<有效 host[:port]>' can't be established.」
              每行: 「<alg> key fingerprint is SHA256:….」  「Are you sure you want to
              continue connecting (yes/no/[fingerprint])?」
              → 讀取回答
                  ├─ `yes`（或完整指紋，大小寫不敏感）→ 接受：以有效 host token（`HostKeyAlias`，
                  │    否則 `host`／`[host]:port`）把 keyscan 的 `<type> <blob>` 寫入
                  │    known_hosts 目的地 → 回傳 Accepted → 重試 batch transport
                  ├─ `no` / 空 / EOF / 取消 → 回傳 Rejected → 維持既有 unknown 錯誤
                  └─ 其它輸入（不辨識）→ 重新提示（有界次數）
```

## Go/No-Go 結論

**結論：GO。** 選定「`ssh-keyscan` 取 key + 本地計算指紋 + 本地 prompt + 接受後寫入 `known_hosts`
+ 既有 batch transport 重試」的 seam。它滿足 ADR-HKT-01 所有約束：

1. 不回放遠端輸出：只消費 keyscan 的 `<host> <type> <base64>`（GC-HKT-02），其餘全部由 Libra 本地
   固定字串構成。
2. 不盲寫 keyscan：只有使用者明確輸入 `yes`/完整指紋後才寫入 `known_hosts`。
3. 不改 changed-key 語義：只對 typed `Untrusted` 進入互動，`Changed` 維持 fail-closed 指引。
4. 只在「unknown + TTY + ask」分支增加一次有界握手（keyscan + 重試 transport），符合 GC-HKT-01 與
   性能預算。

**觸發點：** 以既有 SSH transport 的 typed `Untrusted`（`SSH_HOST_KEY_UNCONFIRMED_SIGNAL`）作為進入
互動的判準，而非在每個操作前都做 keyscan；因此已知主機零額外 round-trip。

**已知限制 / 殘餘風險（HKT-02 須覆蓋）：**
- 有效 host token 與 known-hosts 目的地以 `ssh -G` 取得 OpenSSH 解析後的 `HostName`/`Port`/
  `HostKeyAlias`/`UserKnownHostsFile`，因此 ssh_config 的 host 覆寫（例如 `Host github.com` →
  `HostName ssh.github.com`＋`Port 443`）會以 `[ssh.github.com]:443` 而非 URL 的 `github.com`
  寫入信任；這是修復 HKT-02 逃逸缺陷（接受後 retry 因 token 不符仍失敗）的權威來源。
- `ssh -G` 不可用或未輸出 `hostname` 時，回退到 URL host/port 與既有 `~/.ssh/known_hosts`（或
  ssh_config 的第一個 `UserKnownHostsFile`）解析。`GlobalKnownHostsFile`、`KnownHostsCommand` 與
  多路徑 `UserKnownHostsFile` 的實際寫入目的地仍依 OpenSSH 版本；本卡以第一個可寫路徑為準。
- 若 HKT-02 證明 keyscan 在既有限制下不足以安全實作，則以 DEFER-HKT-03 停止 HKT-02（見 issues/582
  ADR-HKT-01 Revisit when）。

## 所有權移交（DEP-HKT-01 / DEP-HKT-02）

- `issues/480.md` HP-17（issue #560）：host-key 互動範圍由 #582 正式承接，HP-17 標註為「移交 #582
  （HKT-01/HKT-02）」，不再作為獨立實作卡（見下方註記與 issues/480 卡的移交記錄）。
- `plan-20260901.md` ADR-PKT-03 / DEFER-07：零 raw-stderr 契約由 #582 繼承並以本設計的
  GC-HKT-01..04 落實；DEFER-07 的「受限終端中介」由本設計的本地 prompt seam 取代，並在該表記錄
  「移交 #582」。

> **移交判定：** 移交後不允許同時存在兩份平行 SSH implementation 卡。HKT-01（政策 cascade）與
> HKT-02（human confirmation）是唯一持有該行為軸的卡；HP-17 的 `done/complete` 屬性保留為歷史
> 證據，但其「互動確認」承諾由 HKT-02 交付。
