# libra hydrate

`libra hydrate <path>...` **按需** 物化工作树内容（lore.md 3.3）。这是 Lore “hydrating VFS”的诚实、平台可移植 v1：一个显式命令，而不是透明的 FUSE-on-access 文件系统（后者仍是 `worktree-fuse` 后续项）。只处理 whole-object — 没有 FastCDC range。

## 兼容性

- 级别：`intentionally-different`（Git 没有 hydrate/VFS 表面）。

## 设计

对每个请求路径（默认还包含通过 3.1 依赖图得到的传递 forward dependencies），hydrate 按 local → alternate（2.3）→ remote durable tier 解析 blob，并写入工作树。读取策略自然生效（`--offline`/`--local` 拒绝远端 fetch）。

### 失败恢复契约

每个 blob 在 borrowed/remote 命中时会做 OID 校验（并且在 `--verify` 下对本地路径重新哈希 — 若不匹配则从 durable tier 治愈），然后通过原子临时文件 + rename 发布。因 **任何** 原因失败的 hydration — 对象到处缺失、远端不可达、传输错误、verify mismatch、中断 — 都会保持既有工作树文件 **不变**，永远不会留下截断或半写入文件。

### Sparse gating

启用 sparse view（2.2）时会 gate 完整 hydration 集合 — 包括请求 roots 和被拉入的 dependencies — 因此依赖边永远不能绕过一个为了避免物化大型 out-of-view assets 而设置的 view。`--ignore-sparse` 可覆盖。

## 示例

```bash
libra hydrate scene.usd                 # 物化 scene.usd 及其依赖
libra hydrate scene.usd --no-deps       # 只物化这个文件
libra hydrate assets/ --depth-limit 2   # 限制依赖闭包
libra hydrate big.bin --verify          # 落地前重新哈希 payload
libra hydrate a b --dry-run             # 报告会 hydrate 什么
libra hydrate x --ignore-sparse         # hydrate out-of-view 路径
```

## 延后项（非 v1）

LFS-pointer blobs（其下载路径尚非原子 — 会干净跳过）、symlink/gitlink 条目、透明 FUSE on-access hydration、FastCDC 和字节范围 hydration。通过 3.2 的 `libra fetch --notes` / `libra pull --notes` 从本地 Libra source 拉取图之后，跨机器依赖展开现在可工作（网络 / foreign-Git 传输延后，D17）。
