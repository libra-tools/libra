# `libra metadata`

分支/仓库 metadata key-value store（Libra 扩展，lore.md §1.5 — branch protect/archive/lineage 的基础）。最接近的 Git 对应物是 `git config branch.<name>.*`。

## 概要

```
libra metadata get   <key>         (--branch <name> | --repo)
libra metadata set   <key> <value> (--branch <name> | --repo)
libra metadata unset <key>         (--branch <name> | --repo)   # alias: clear
libra metadata list                (--branch <name> | --repo) [--prefix <p>]
```

## 说明

Metadata 有作用域：必须且只能指定 `--branch <name>` / `--repo` / `--revision <rev>` 之一。

- **Branch scope**（`--branch <name>`）：附着到本地分支的 key-values，存储在 `metadata_kv` SQLite 表中。Metadata 会跟随分支生命周期：`branch -m` 移动它，`branch -c`/`-C` 复制它（forced copy 会替换目标 metadata，匹配 ref overwrite），删除分支会删除它。每个动词都要求分支存在。Remote-tracking branches 不携带 metadata。
- **Repo scope**（`--repo`）：仓库级 key-values，存储在 `config` store 的 `metadata.*` namespace 下 — 因此 `libra config --get metadata.<key>` 能看到相同值（有意的双表面；`libra metadata --repo` 是普通值的推荐入口）。看似敏感的 keys（例如 `metadata.apitoken`）以及现有值已加密的 keys，会被 `set --repo` **拒绝**，并提示改用 config 入口（`libra config metadata.<key> <value>`），由它负责 vault-encryption 决策 — 在这里写入要么会未加密存储 secret，要么会破坏加密行。`get`/`list` 将加密值渲染为 `<REDACTED>`（使用 `libra config --get --reveal metadata.<key>` 解密）；`unset` 可作用于任何 key。若某个 key 通过 `config --add` 给了多个值，`set`/`unset` 会拒绝并提示先用 `config unset-all`；`get` 返回最新值。

- **Revision scope**（`--revision <rev>`）：commit 上的 metadata。Commit 是不可变的，因此该作用域合并两层：commit message 的 **trailer block**（只读，按 Git 规则解析 — 与 `log --trailer` 使用同一引擎）和可变 **notes layer**（每个 commit 一个 JSON 文档，位于 `refs/notes/metadata` 下；`libra notes --ref metadata` 是有意的双表面）。读取优先 notes layer；`get`/`list` 在 JSON 中报告 `source`（`note`/`trailer`）。写入（`set`/`unset`）只触碰 notes layer — unset 仅存在于 trailer 的 key 会退出 1 并提示 amend/reword，移除 shadowed trailer 的 note entry 会打印 notice，说明 trailer 值重新可见。该作用域下 key 匹配是 ASCII **大小写不敏感**（trailer 惯例；branch/repo 保持精确）。Note-layer 值是 **local-only**（notes 永远不会 push）— 另一个 clone 能看到 commit trailers，但看不到你的 overrides。JSON `target` 始终是解析后的完整 commit OID。每个 commit 文档上限 1 MiB。

众所周知的 branch keys — `protect`、`archive` 和 `lineage.*` prefix — 已对 `branch reset` 和 `update-ref` **强制执行**（lore.md 1.13；delete/push/merge enforcement 待定）：设置它们会打印 notice。后续 branch-policy layer（lore.md 1.13）会一次性落地更多 enforcement，并以 fail-closed 方式读取这些 keys（损坏值视为 protected，绝不静默 unprotected）。

值默认是文本；`set --branch` 还接受 **typed values**（lore.md 1.10）：`--numeric`（整数或有限小数 — 不允许前后空白；设置时验证，按原样存储）和 `--binary`（VALUE 参数是标准 base64 — 存储编码文本，因此 raw payload 上限约为 1 MiB value limit 的 3/4；用 `| base64 -d` 解码）。`get`/`list`/JSON 会报告存储的 `value_type`。`--repo` 拒绝 typed flags（config store 仅文本；已记录后续项）。空字符串是合法值，且不同于 key 缺失。Key 精确且大小写敏感（最大 256 bytes，无 whitespace）；value 上限 1 MiB。

## 选项

| 选项 | 说明 |
|------|------|
| `get <key>` | 打印值。key 缺失时退出 1（类似 `config` key miss）。 |
| `set <key> <value>` | 创建或覆盖。`--json` 会在覆盖时报告 `previous` 值和 `value_type`。 |
| `--numeric` / `--binary` |（仅 `set --branch`）声明值类型；互斥。验证失败退出 129。 |
| `unset <key>` | 删除 key（别名：`clear`）。未删除任何内容时退出 1。 |
| `list` | 打印按 key 排序的 `key=value` 行。 |
| `--branch <NAME>` | 操作本地分支的 metadata。 |
| `--repo` | 操作仓库级 metadata（`config` `metadata.*`）。 |
| `--revision <REV>` | 操作 commit metadata（不可变 trailers + 可变本地 notes layer；见上文）。 |
| `--prefix <P>` |（仅 `list`）只显示以该 prefix 开头的 keys，例如 `lineage.`。 |
| `--json` / `--machine` | 结构化信封：`{ action, scope, target, key, value, ... }`。 |

## 退出码

| 代码 | 含义 |
|------|------|
| `0` | 成功。 |
| `1` | 对缺失 key 执行 `get`/`unset`。 |
| `128` | 不在仓库中。 |
| `129` | 用法错误：缺失/重复 scope、无效 key、value 过大、未知分支（`LBR-CLI-002`/`LBR-CLI-003`）。 |

## 示例

```bash
# 保护一个分支（对 branch reset/update-ref 强制执行）并读回。
libra metadata set protect true --branch main
libra metadata get protect --branch main

# key prefix 下的 lineage records。
libra metadata set lineage.parent dev --branch feature
libra metadata list --branch feature --prefix lineage.

# Repo-level metadata，也可通过 config 表面看到。
libra metadata set owner platform-team --repo
libra config --get metadata.owner

# 面向 agents 的结构化输出。
libra --json metadata list --branch main
```

## 与 Git 对比

Git 没有 first-class metadata store；最接近的是 `git config branch.<name>.*`（per-branch config）和 `git notes`（per-object annotations）。`libra metadata` 在 [`COMPATIBILITY.md`](../../../COMPATIBILITY.md) 中分类为 `intentionally-different`。Metadata 是 local-only：永远不会被 push、pull 或 publish。
