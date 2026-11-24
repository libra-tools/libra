# `libra replace`

在读取对象时用另一个对象替换它 —— `git replace` 的一个聚焦子集。

## 用法

```
libra replace [-f] <object> <replacement>
libra replace -d <object>...
libra replace [-l] [<pattern>]
```

## 说明

一条替换记录表示：当要读取 `<object>` 时，改为返回 `<replacement>`。替换发生在对象加载层（`load_object`），因此经由它的所有读取者 —— `log`、`show`、`rev-parse` 的 peel 等 —— 都透明地遵守，而不只是某一个调用点。

- **创建**（`libra replace <object> <replacement>`）—— 记录替换。两个对象都必须存在；类型必须一致，除非用 `-f`。已存在的替换只有 `-f` 才覆盖。对象不能替换自己。
- **删除**（`-d <object>...`）—— 删除替换；删除不存在的替换是错误。
- **列出**（`-l [<pattern>]`）—— 每行打印一个被替换对象的 id（Git 的默认短格式），可按 `wildmatch` 模式过滤，模式对整个对象 id 做整名匹配（`*`、`?`、`[...]` 为通配符，默认模式为 `*`）。`--format=medium/long` 同时显示替换 oid 与类型；`-l` 最多接受一个模式。

替换以松散 ref 存于 `.libra/refs/replace/<oid>`（Git 的 `refs/replace/` 命名空间）。

## 选项

| 选项 | 说明 |
|------|------|
| `-f`, `--force` | 覆盖已存在的替换，并允许类型不一致。 |
| `-d`, `--delete` | 删除给定对象的替换。 |
| `-l`, `--list` | 列出被替换对象 id（可按 `<pattern>` 过滤）。 |

## 退出码

| 退出码 | 含义 |
|--------|------|
| `0` | 成功。 |
| `128` | 不在仓库内、对象非法、删除不存在的替换、类型不一致而未加 `-f`、已存在替换而未加 `-f`，或 IO 错误。 |

## 示例

```bash
# 让历史在原位读到一个修订后的提交
libra replace <old-commit> <new-commit>
libra log         # 现在显示替换

libra replace -l                 # 列出被替换对象
libra replace -d <old-commit>    # 停止替换
```

## 与 Git 对比

| 任务 | Libra | Git |
|------|-------|-----|
| 创建 | `libra replace <o> <r>` | `git replace <o> <r>` |
| 删除 | `libra replace -d <o>` | `git replace -d <o>` |
| 列出 | `libra replace -l` | `git replace -l` |

差异与延后项：替换以松散 ref 存于 `.libra/refs/replace/` 而非 SQLite reference 表，故 `show-ref` / `for-each-ref` 暂不列出；`-l` 仅打印对象 id（Git 默认短格式）且按整名 `wildmatch` 过滤；`--format`、`--edit`、`--graft`、`--convert-graft-file` 未实现。
