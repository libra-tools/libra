# `libra hash-object`

为原始文件内容或标准输入计算与 Git 兼容的对象 ID。

```bash
libra hash-object [OPTIONS] <PATH>...
libra hash-object --stdin [OPTIONS]
libra hash-object --stdin-paths [OPTIONS]
```

支持 `blob`（默认）、`commit`、`tree`、`tag` 四种 Git 对象类型；对象 id 由 `<type> <size>\0<content>` 头部计算，与 `git hash-object -t <type>` 逐字节一致。默认会校验内容是否为良构对象（blob 接受任意字节），`--literally` 跳过校验。它不会应用 clean 过滤器、attributes 或 LFS 指针转换。`--path` 作为 Git 兼容路径上下文和 stdin JSON source label 接受；在实现路径过滤前，它不会改变被哈希的字节。

只读哈希不需要 Libra 仓库，并且在没有可用仓库对象格式时默认为 SHA-1。`-w` / `--write` 需要仓库，因为它会将对象存入仓库对象数据库。

## 选项

| 选项 | 短选项 | 说明 |
|--------|-------|-------------|
| `<PATH>...` | | 要哈希的文件路径 |
| `--stdin` | | 从标准输入读取字节，而不是读取文件路径 |
| `--stdin-paths` | | 从标准输入读取文件路径（每行一个）并逐个哈希 |
| `--write` | `-w` | 将计算出的对象存入仓库对象数据库 |
| `--type <TYPE>` | `-t` | 要哈希的对象类型：`blob`（默认）、`commit`、`tree`、`tag` |
| `--literally` | | 按给定类型哈希字节，但不校验内容是否为该类型的良构对象 |
| `--path <PATH>` | | Git hash-object 兼容路径上下文标签 |
| `--no-filters` | | 显式按原始字节哈希，不使用路径过滤器 |
| `--json` | | 输出结构化 JSON 信封 |
| `--machine` | | 以一行紧凑 JSON 输出同一信封 |

## 示例

只哈希文件，不写入对象：

```bash
libra hash-object README.md
```

将文件作为 blob 对象哈希并写入：

```bash
libra hash-object -w src/main.rs
```

将文件内容作为类型化对象哈希（id 与 `git hash-object -t <type>` 一致）；除非加 `--literally`，否则会校验内容是否为该类型的良构对象：

```bash
libra hash-object -t commit commit-payload
libra hash-object -t tag --literally arbitrary-bytes
```

从标准输入哈希字节：

```bash
printf 'hello' | libra hash-object --stdin
```

使用 Git 兼容路径上下文标签哈希 stdin：

```bash
printf 'hello' | libra hash-object --stdin --path README.md
```

## 输出

人类可读输出会为每个输入打印一个对象 ID：

```text
b6fc4c620b67d95f953a5c1c1230aaab5db5a1b0
```

结构化输出：

```json
{
  "ok": true,
  "command": "hash-object",
  "data": {
    "object_type": "blob",
    "write": false,
    "objects": [
      {
        "source": "-",
        "oid": "b6fc4c620b67d95f953a5c1c1230aaab5db5a1b0",
        "size": 5,
        "written": false
      }
    ]
  }
}
```

使用 `-w` 时，本地对象持久化与云 catalog 索引具有独立的耐久边界。如果对象已经
写入，但后台 `object_index` 更新遇到终止性数据库错误，命令仍保留正常成功输出
（JSON 只输出一个 `ok: true` 信封），同时向 stderr 写出可操作警告，并在仓库存储
目录中保留原子 repair marker。下一条会执行 schema 检查的仓库命令会自动重试该
精确索引行；只要 marker 无法修复，`libra cloud sync` 就会 fail-closed。使用
`--exit-code-on-warning` 时，这次本地成功写入会返回退出码 9 / `LBR-WARN-001`，但
不会撤销已经持久化的对象。
队列 drain 采用异步且最多等待 60 秒；预算用尽时命令会告警并把 marker 留给下一次
preflight，而不会阻塞嵌入式 Tokio executor。若 marker 无法创建，命令会在报告索引
写入完成前返回错误；索引行成功但 marker 无法删除时，也会进入相同告警契约。
如果多输入写入已持久化前面的对象、随后因后面的输入失败，原始读取/校验错误及退出码仍是
主结果，同时 stderr 也会报告前面已完成写入留下的耐久修复任务。命令拥有的后台持久化任务
会在启动前登记，迟到失败仍归属于排队它的调用。repair 采用有界分页，因此超大队列会跨后续命令持续推进；
在队列清空前，cloud 与实际执行删除的 agent cleanup 都保持 fail-closed。重放与排队 writer
在索引行更新及 marker 退役期间共享来自固定 65,536 个 OID 分片命名空间的进程崩溃可恢复
ownership lock，因此迟到的排队
写入不会在重放和实际清理消费 marker 后重新插回索引行。

## 兼容性

| 功能 | Libra | Git | Jujutsu |
|---------|-------|-----|---------|
| 将文件作为 blob 哈希 | `libra hash-object <path>` | `git hash-object <path>` | N/A |
| 从 stdin 读取 | `--stdin` | `--stdin` | N/A |
| 从 stdin 读取路径 | `--stdin-paths` | `--stdin-paths` | N/A |
| 写入对象 | `-w` / `--write` | `-w` | N/A |
| 选择对象类型 | `-t blob/commit/tree/tag` | `-t <type>` | N/A |
| 路径上下文 | 接受 `--path <path>`，不应用 filters | `--path <path>` | N/A |
| 禁用 filters | 接受 `--no-filters` | `--no-filters` | N/A |
| 路径过滤器 / attributes | 不支持 | filters / attributes | N/A |
| 按字面哈希无效对象 | `--literally`（仅限已知类型） | `--literally`（任意类型字符串） | N/A |

## 错误

| 条件 | 稳定代码 | 退出码 | 提示 |
|-----------|-------------|------|------|
| 对象类型不在 blob/commit/tree/tag 之内 | `LBR-CLI-002` | 129 | `hash-object supports blob, commit, tree, and tag` |
| 内容不是 `-t <type>` 的良构对象（且未加 `--literally`） | `LBR-CLI-002` | 129 | `pass --literally to hash malformed content without validation` |
| 无法读取输入文件 | `LBR-IO-001` | 128 | 确认路径存在且可读 |
| 无法写入对象 | `LBR-IO-002` | 128 | 检查对象存储权限和磁盘空间；若原因为云索引 repair marker 注册失败，负载已安全写入，直接重试即可复用（hash-object 不暂存任何路径，也无需锁文件清理） |
| 对象已写入但云索引修复仍待处理，且使用 `--exit-code-on-warning` | `LBR-WARN-001` | 9 | 修复警告中的仓库数据库/marker 问题；下一条仓库命令会自动重试 |
