# `libra update-ref`

安全地更新、创建或删除一个分支 ref，支持可选的比较并交换（compare-and-swap）—— `git update-ref` 的一个聚焦子集。ref 读取、ref 写入/删除与 reflog 写入全部发生在**同一个 SQLite 事务**内，因此比较失败会原子回滚。

## 用法

```
libra update-ref [-m <reason>] refs/heads/<branch> <newvalue> [<oldvalue>]
libra update-ref -d [-m <reason>] refs/heads/<branch> [<oldvalue>]
```

## 说明

`update-ref` 把 `refs/heads/<branch>` 指向 `<newvalue>`（不存在则创建），或用 `-d` 删除它。可选的 `<oldvalue>` 是**比较并交换**守卫：

- 一个完整对象 id —— ref 当前必须指向它，否则命令失败；
- `0000…0000`（全零 id）—— ref 必须**尚不存在**（仅创建）。

省略 `<oldvalue>` 时，ref 会被无条件创建或覆盖。

**范围（v1）：** 仅支持 `refs/heads/<branch>` —— 即 Libra 的 SQLite `reference` 表能直接建模的分支 tip 情形。`HEAD`、`refs/tags/*`、`refs/remotes/*` 以及任意 ref 命名空间均被拒绝；`HEAD` 请用 [`symbolic-ref`](symbolic-ref.md) / [`switch`](switch.md)，标签请用 [`tag`](tag.md)。

每次成功更新都会为该 ref 写入一条 `update-ref` 的 reflog 记录。你为比较并交换传入的 `<oldvalue>` **绝不会**写入 reflog 消息；只记录真实的前后对象 id。

## 选项

| 选项 | 说明 | 示例 |
|------|------|------|
| `-d`, `--delete` | 删除 ref 而非更新。 | `libra update-ref -d refs/heads/old` |
| `-m <reason>` | 随更新记录的 reflog 原因。 | `libra update-ref -m "重置 tip" refs/heads/main <oid>` |
| `<newvalue>` | 新提交，可写对象 id 或任意 revision 表达式（`-d` 时省略）。 | `libra update-ref refs/heads/main HEAD~1` |
| `<oldvalue>` | 比较并交换的期望当前值，可写 id 或 revision（`0{40}` = 必须不存在）。 | `libra update-ref -d refs/heads/topic HEAD` |
| `--json` / `--machine` | 结构化输出：`{ ref, old, new, deleted }`。 | `libra --json update-ref refs/heads/main <oid>` |

符号值（`ref:refs/heads/…`）以及把全零对象 id 作为 `<newvalue>` 都会被拒绝 —— 请用 `symbolic-ref`，或用 `-d` 删除。

### `<newvalue>` 中的 revision 表达式

`<newvalue>` 走 Libra 通用的 revision 解析器，因此分支名、tag、`HEAD`、父/祖先导航（`HEAD^`、`HEAD~2`）与缩写对象 id 都可用。

**不做隐式 peel**：表达式本身指向的对象必须就是 commit。lightweight tag 存的就是 commit id，故被接受；bare annotated tag 指向的是 tag 对象，会被拒绝（`LBR-CLI-003`，错误信息里点名解析到的类型）。需要它时请显式 peel：

```bash
libra update-ref refs/heads/release v1.0^{commit}
```

这与 Git 一致——Git 同样拒绝把 annotated tag 对象写进 `refs/heads/*`。无法解析的 revision 是 `LBR-CLI-003`；上面两条语法层拒绝仍是 `LBR-CLI-002`。两者退出码都是 `128`。对象库自身的失败（对象不可读或损坏）保留其仓库/IO 错误码，绝不降级为输入错误。

`<oldvalue>` 接受同样的表达式，但有一处有意差异：**不做类型校验**。它断言的是「ref 现在指向什么」，因此解析结果按原样逐字比较——在这里写 annotated tag 得到的是普通的比较并交换不匹配，而不是「不是 commit」的拒绝，这也正是 Git 的行为。全零 id 的「必须不存在」语义不变。

## 退出码

| 退出码 | 含义 |
|--------|------|
| `0` | ref 已更新、创建或删除。 |
| `128` | 不在仓库内、不支持/非法的 ref、`<newvalue>` 无法解析或解析结果不是 commit、非法对象 id、比较并交换不匹配，或删除不存在的 ref。 |

## 示例

```bash
# 把分支指向某个提交
libra update-ref refs/heads/main <oid>

# 比较并交换：仅当 main 仍为 <oldoid> 时才移动
libra update-ref refs/heads/main <newoid> <oldoid>

# 仅当分支不存在时创建
libra update-ref refs/heads/topic <oid> 0000000000000000000000000000000000000000

# 删除分支 ref，可选地用当前值守卫
libra update-ref -d refs/heads/old
libra update-ref -d refs/heads/old <oldoid>
```

## 与 Git 对比

| 任务 | Libra | Git |
|------|-------|-----|
| 更新分支 ref | `libra update-ref refs/heads/b <oid>` | `git update-ref refs/heads/b <oid>` |
| 比较并交换 | `libra update-ref refs/heads/b <new> <old>` | `git update-ref refs/heads/b <new> <old>` |
| 删除 ref | `libra update-ref -d refs/heads/b` | `git update-ref -d refs/heads/b` |

延后（未公开）：非 `refs/heads/*` 命名空间、`HEAD`、`--stdin` 批量更新、`--create-reflog`、`--no-deref`。
