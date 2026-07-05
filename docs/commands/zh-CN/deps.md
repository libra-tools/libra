# libra deps

`libra deps` 管理 **文件依赖图**（lore.md 3.1）：带类型、按文件、按版本记录的依赖边。这是 Libra 扩展 — Git 没有文件依赖概念。

## 兼容性

- 级别：`intentionally-different`。

## 设计

一条边 `(from -> to, kind)` 声明一个文件依赖另一个文件。边按提交 VERSIONED：权威存储是保留 notes ref `refs/notes/deps` 下每个提交一个邻接文档，由 `internal::deps::DependencyStore` 独占管理（镜像 `refs/notes/metadata` 模式 — 不新增 SQLite 表，遵守 §3.6 “no per-kind table” 规则）。每次查询都会加载该 revision 的（有大小上限）文档并在内存中计算，因此没有可能失同步的 projection cache。

查询是防循环的（带 visited set 的迭代 BFS — 深/宽图不会让栈溢出）且容忍缺失（缺少 note → 空图）。路径是 repo-relative 并规范化（去掉 `./`、`\`→`/`、折叠尾随 `/`）；绝对路径、`..` 逃逸和空字符串都会被拒绝。

`transitive_closure` API 是 3.2（dependency-filtered clone/sync）和 3.3（hydrating VFS）扩展根文件集时调用的可复用接缝。

## 线缆传输（lore.md 3.2 — 本地 side-channel）

Libra deps note 是一个 loose blob（JSON 邻接文档）加 SQLite `notes` 表中的一行；`refs/notes/deps` 不是真正的 ref 表 ref，因此无法随 pack/ref want set 传输。lore.md 3.2 通过专用本地协议 side-channel 传输边：`libra fetch --notes` / `libra pull --notes` 从 **本地 Libra source** 导入 `refs/notes/deps`（与任何本地边做 union merge 并重新验证每个 endpoint），默认关闭（Git parity）。可用 `remote.<name>.fetchNotesDeps=true` 持久化 opt-in；`libra clone --deps-of` 会隐含启用。网络 / foreign-Git / push-side 传输延后（D17），所以 fresh clone 在用 `--notes` 获取 notes 前仍读取空图。

## 示例

```bash
libra deps add scene.usd tex/wood.png     # 声明一个依赖
libra deps list scene.usd                 # 直接依赖
libra deps list tex/wood.png --reverse    # 反向依赖方
libra deps tree scene.usd                 # 传递闭包
libra deps tree scene.usd --depth-limit 2 # 有界闭包
libra deps why scene.usd tex/wood.png     # 最短依赖路径
libra deps rm scene.usd tex/wood.png      # 删除一条边
libra deps add a b --revision <commit>    # 指向特定提交
```

## 延后项（非 v1）

网络 / foreign-Git / push-side 边传输（D17）、把边 carry-forward 到新提交、跟随 rename（基于路径的边不会自动迁移），以及自动依赖推断（v1 边由作者声明）。
