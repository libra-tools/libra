# `libra dirty`

咨询式 dirty-set 标记（Libra 扩展，lore.md §1.1）。Git 没有等价物；该缓存为 agent 和工具加速 status。

## 概要

```
libra dirty <paths>...
libra dirty --list
```

## 说明

`libra dirty <paths>` 在 `working_dirty` SQLite 缓存中把路径标记为 dirty — 不读取文件内容，也绝不触碰索引。标记是咨询式的，只能让缓存视图 *过度* 报告（安全方向）。不存在的路径是合法的：删除也属于 dirty。路径必须保持在仓库内（逃逸路径会让整个调用失败，退出 129）。

缓存生命周期：

- **`libra status --scan`** — 唯一权威重建：运行普通完整 status，并原子替换快照（unstaged dirty set + staged set），用索引指纹和 HEAD 打戳。
- **`libra status --cached`** — 消费快照，而不是遍历工作树。任何新鲜度疑问（scan 后索引或 HEAD 变化；尚未 scan）都会退化为完整 status 并给出提示 — 缓存永远不会说谎。**快照语义**：scan 后发生的仅工作树编辑不会改变索引，并且在 rescan 或 `libra dirty` 标记记录它们之前对 `--cached` 不可见 — 这些标记就是为此存在。
- **`libra status --check-dirty`** — 只重新验证缓存集合（O(dirty paths)）：验证为 clean 的行会被修剪；不会发现新路径。
- **`libra dirty --list`** — 显示缓存行（`kind`、`source`、path）以及缓存新鲜度。

默认 `libra status` 永远不读写该缓存。

## 退出码

| 代码 | 含义 |
|------|------|
| `0` | 成功。 |
| `128` | 不在仓库中。 |
| `129` | 用法错误（逃逸路径、缺少参数）。 |

## 示例

```bash
libra status --scan               # 构建快照
libra dirty src/main.rs           # 不重新扫描，记录一个编辑
libra status --cached             # 从缓存获取 O(dirty) status
libra status --check-dirty        # 修剪陈旧标记
libra --json dirty --list         # 结构化缓存检查
```

## 与 Git 对比

Git 没有 dirty-set 缓存表面（最接近的是 index stat cache 和 `fsmonitor`，但它们都是内部机制）。`libra dirty` 以及 `status` 的 `--scan`/`--cached`/`--check-dirty` 标志在 [`COMPATIBILITY.md`](../../../COMPATIBILITY.md) 中分类为 `intentionally-different`。注意 `status --cached` 与 Git 的 `--cached`（= index）无关。
