# libra alternates

`libra alternates` 管理 **object alternates**（lore.md 2.3）：从共享/父对象库借用对象，而不是复制对象。这是 Libra 扩展（git 没有 `alternates` 命令；需要手动编辑 `objects/info/alternates`）。

## 兼容性

- 级别：`intentionally-different`。

## 设计

单一所有者模块 `internal::alternates` 读写 `objects/info/` 下的两个 git 标准文件：

- `alternates` — 本对象库从这些对象目录借用对象。读取解析器在本地未命中时会查询可传递链（防循环、有深度上限）；每个借用命中都会在返回前做完整字节 OID 校验。
- `borrowers` — 从本对象库借用对象的对象目录（Libra 扩展）。

`exist` 会查询 alternates，因此借用但存在的对象不会被当成缺失对象。

### 删除安全（严密）

注册 base 也会把本仓库记录为它的 borrower。只要存在任何存活 borrower，base 的 `gc` 和 `cache evict` 就会 **拒绝清理 loose objects** — 共享 base 永远不能删除 borrower 仍然需要的对象。`file obliterate` 会拒绝仅借用的对象（它不会进入父对象库）；`fsck` 会把悬空 alternate 报告为可操作错误。

### 防护

`add` 拒绝自引用、`core.objectformat` 不同的 base（永远不跨哈希类型借用），以及 TIERED（s3/r2）base（本地 alternate 无法访问 base 的远端层）。

## 示例

```bash
libra alternates add /path/to/base/.libra/objects   # 从共享对象库借用
libra alternates list
libra alternates remove /path/to/base/.libra/objects # 停止借用
libra alternates prune --dry-run                     # 列出仓库已消失的借用方登记
libra alternates prune                               # 退役这些登记（解除本库 gc 的封锁）
```

## 退役仓库已消失的借用方

注册 alternate 会在 base 中把本仓库登记为 BORROWER；只要该登记还在，base 的
`gc`、`repack -d`、`cache evict`、`agent clean` 与 `file obliterate` 都会拒绝删除对象。

唯一会被自动清除的，是路径**存在但不是目录**的登记（对象库绝不会是文件）。仅仅
“路径不存在”不算证据，这是有意为之：它与“挂载点未挂载”在外部看来完全相同，一旦
猜错，挂载恢复后借用方所需的对象已经被删掉了。请在 **base 中**运行
`libra alternates prune` 退役路径不存在的登记，`--dry-run` 可先行列出。若某条登记
根本无法检查（挂载不可达、父目录权限不足），请点名退役：
`libra alternates prune /path/to/gone/.libra/objects` 会不问文件系统状态地退役该条
登记——此时用户是唯一可用的证据。该路径与登记逐字匹配，因此指向另一个借用方的
符号链接不会误伤。常规路径仍是在借用方运行 `libra alternates remove`，它会同时注销
两侧。

## 延后项（非 v1）

`git clone --reference`/`--shared` 的免复制能力（需要针对 alternate 做 fetch have-negotiation，目前这些标志仍是已接受的 no-op）；`--dissociate`（复制借用对象并断开链接）；2.11 默认共享对象库。
