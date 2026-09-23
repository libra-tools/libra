# `libra bundle`

创建、校验、检查与解包 Git v2 bundle。bundle 由文本头和 pack 组成，可由系统 Git 或 Libra 消费。

## 用法

```text
libra bundle create <file> [--all] [--branches] [--tags] [<rev>...]
libra bundle verify <file>
libra bundle list-heads <file>
libra bundle unbundle <file>
```

## 说明

- `create` 写完整、非 thin bundle。显式修订可与 `--all`、`--branches`、`--tags` 组合，且至少需要一种选择。`--all` 与显式 `HEAD` 会广告一行 `HEAD`（分离 HEAD 指向该提交；附着 HEAD 仍写 `HEAD`，不改写成 `refs/heads/<branch>`）。annotated tag head 保留 tag 对象 OID，pack 包含其目标闭包。输出先写入私有临时文件并同步，再 rename 到目标。
- `verify` 校验 v2 头、本地 prerequisite、pack 版本与完整 pack checksum。
- `list-heads` 只打印 `<oid> <ref>` advertised heads，不导入对象。
- `unbundle` 校验 prerequisite/checksum，构建正确的 SHA-1 或 SHA-256 pack index，并把 pack/index 对装入对象库。它打印 heads，但按 `git bundle unbundle` 语义**不更新 refs**。重复导入会先核对已安装 pair，再报告成功。

bundle 输入、收集的原始对象数据和最终输出各自以 1 GiB 为上限。这也会约束 pack 压缩前的内存，因此原始数据超过上限、即使高度可压缩的对象图也会被拒绝。创建端只支持完整历史；prerequisite/thin/增量范围 bundle 仍延后。

## 选项

| 选项 | 说明 |
|---|---|
| `<rev>...` | 把显式修订作为 advertised heads 包含。`HEAD` 广告为 `HEAD`。 |
| `--all` | 包含全部本地分支和 tag，并写入一行 `HEAD`。 |
| `--branches` | 包含全部本地分支。 |
| `--tags` | 包含全部本地 tag，并保留 annotated 对象。 |

## 退出码

| 退出码 | 含义 |
|---|---|
| `0` | 成功。 |
| `1` | `verify`/`list-heads` 遇到不可读/非法 bundle 或缺失 prerequisite。 |
| `128` | 仓库/用法/写入失败，或 `unbundle` 校验、建索引、安装失败。 |

## 示例

```bash
libra bundle create repository.bundle --all
libra bundle create releases.bundle --tags main
libra bundle verify repository.bundle
libra bundle list-heads repository.bundle

# 导入对象，检查输出的 heads，再只更新需要的 ref
libra bundle unbundle repository.bundle
libra update-ref refs/heads/restored <printed-commit-oid>

git clone repository.bundle restored
```

## 与 Git 对比

| 任务 | Libra | Git |
|---|---|---|
| 创建 | `libra bundle create <f> --all` | `git bundle create <f> --all` |
| 校验 | `libra bundle verify <f>` | `git bundle verify <f>` |
| 列出 heads | `libra bundle list-heads <f>` | `git bundle list-heads <f>` |
| 导入对象 | `libra bundle unbundle <f>` | `git bundle unbundle <f>` |

仍延后的 surface：prerequisite/thin/增量 bundle 创建。`libra clone <bundle>` 已支持读取 Git v2 bundle（先仓库目录，再 `<path>.bundle`，再 `<path>`）。`verify` 会校验 checksum，但不会构建临时 index 来穷尽解码每个 pack entry。
