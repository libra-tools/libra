# `libra diff-tree`

显示两个 tree 之间的差异 —— 复用 [`diff`](diff.md) 引擎、采用 plumbing 退出码与 rename 默认值的底层入口。

## 用法

```
libra diff-tree [-r] [-t] [-p] [-s] [--raw] [--name-only] [--name-status] [--exit-code] [--quiet] [-c]
                [--root] [--no-commit-id] <tree-a> [<tree-b>] [-- <path>...]
```

## 说明

`diff-tree` 等价于 `libra diff --old <tree-a> --new <tree-b> --no-renames`：对两个 tree-ish（commit 或 tree id）做 diff。路径限定写在 `--` 之后。作为 Git plumbing，它忽略 porcelain `diff.renames`；`libra --json diff-tree ...` 等全局输出标志仍可用。默认输出是 raw 且**非递归**：双 tree 形式只比较根 tree，变更目录以单条 `040000`
记录出现，需 `-r` 才展开。只给 `<tree-a>` 是 Git 的单提交形式：先输出提交 id 行，
再与其第一父提交做非递归 diff；`--no-commit-id` 抑制该行。`--root` 把初始提交显示为
对空 tree 的全量创建 diff（不给则根提交只输出 id 行）；`-t` 在递归时同时列出 tree 条目。
raw 记录使用完整对象 id（SHA-1 40 位 / SHA-256 64 位），缺失侧为 `000000` 模式与全零 id。
`-p` 输出补丁（隐含递归）。

## 选项

| 选项 | 说明 | 示例 |
|------|------|------|
| `<tree-a> [<tree-b>]` | 要比较的两个 tree-ish，或单个提交（与其第一父提交比较）。 | `libra diff-tree HEAD~1 HEAD` |
| `-r` | 递归子树；默认非递归。 | `libra diff-tree -r a b` |
| `-t` | 递归时同时输出 tree 条目（隐含 `-r`）。 | `libra diff-tree -r -t a b` |
| `--root` | 把初始提交显示为对空 tree 的全量创建 diff。 | `libra diff-tree -r --root <root>` |
| `--no-commit-id` | 单提交形式下抑制提交 id 行。 | `libra diff-tree --no-commit-id HEAD` |
| `-p`, `--patch` | 输出补丁而非 raw 记录。 | `libra diff-tree -p a b` |
| `--raw` | 强制 raw 记录（默认）。 | `libra diff-tree --raw a b` |
| `--name-only` | 只打印变更路径名。 | `libra diff-tree --name-only a b` |
| `--name-status` | 打印路径与状态字母。 | `libra diff-tree --name-status a b` |
| `--exit-code` | 有差异时退出 1（默认 0）。 | `libra diff-tree --exit-code a b` |
| `--quiet` | 不打印任何输出，有差异时退出 1。 | `libra diff-tree --quiet a b` |
| `-s`, `--no-patch` | 完全抑制 diff 输出体。 | `libra diff-tree -s a b` |
| `-c`, `--cc` | 接受 combined-diff 拼写（未合并渲染是已登记的收窄）。 | `libra diff-tree -c a b` |
| `-- <path>...` | 将 diff 限定到路径。 | `libra diff-tree a b -- src/` |
| `--json` / `--machine` | 结构化 diff 输出（与 `diff` 同信封）。 | `libra --json diff-tree a b` |

## 退出码

| 退出码 | 含义 |
|--------|------|
| `0` | 无差异；或有差异但未给 `--exit-code`/`--quiet`。 |
| `1` | 有差异且给了 `--exit-code`/`--quiet`。 |
| `128` | 不在仓库内。 |
| `129` | 参数无效（`LBR-CLI-002`），或修订无法解析（`LBR-CLI-003`）。 |

## 示例

```bash
# 比较某提交与其父提交
libra diff-tree HEAD~1 HEAD

# 限定到目录
libra diff-tree main feature -- src/
```

## 与 Git 对比

| 任务 | Libra | Git |
|------|-------|-----|
| 比较两个 tree | `libra diff-tree a b` | `git diff-tree a b` |

延后：单提交 `diff-tree <commit>`（对比父）、`-t`/`--stdin`。更多选项用 `libra diff`。
