# `libra merge-file`

执行文件级三路合并 —— `git merge-file` 的一个聚焦子集。以共同祖先 `<base>` 为基准，把 `<current>` 与 `<other>` 合并，复用 `libra merge` 对 blob 内容所用的同一个 `diffy` 三路合并，因此冲突标记完全一致。

## 用法

```
libra merge-file [-p|--stdout] [--diff3] [-q|--quiet] <current> <base> <other>
```

## 说明

`merge-file` 把从 `<base>` 到 `<other>` 的更改并入 `<current>`。两侧改了同一区域时记录冲突标记：

```
<<<<<<< ours
...<current> 的行...
=======
...<other> 的行...
>>>>>>> theirs
```

带 `--diff3` 时，base 段落出现在 `|||||||` 与 `=======` 之间。

默认把结果写回 `<current>`；带 `-p` 则打印到 stdout、不改文件。在仓库内就地写入时，原 `<current>` 会先复制到本 worktree local gitdir 的 `merge-file-backup/`（每个 worktree 各自持有备份，互不干扰）：干净合并后删除备份，仍有冲突时保留并提示。

三个参数按原始字节读取；不要求它们被跟踪或对应已存 blob。

## 选项

| 选项 | 说明 | 示例 |
|------|------|------|
| `-p`, `--stdout` | 把合并结果打印到 stdout；不修改 `<current>`。 | `libra merge-file -p a b c` |
| `--diff3` | 在冲突标记中包含 `<base>` 段落。 | `libra merge-file --diff3 -p a b c` |
| `-q`, `--quiet` | 不在 stderr 上提示冲突。 | `libra merge-file -q a b c` |
| `--json` / `--machine` | 结构化输出：`{ conflict, written, merged? }`（`merged` 仅 `-p` 时有）。 | `libra --json merge-file -p a b c` |

## 退出码

| 退出码 | 含义 |
|--------|------|
| `0` | 干净合并（无冲突）。 |
| `1` | 合并产生冲突（仍输出标记）。无论冲突数量，固定为 `1`。 |
| `128` | 错误：输入缺失/不可读，或为二进制文件（检测到 NUL 字节）。 |

## 示例

```bash
# 打印合并结果，不动任何文件
libra merge-file -p ours.txt base.txt theirs.txt

# 就地合并进 ours.txt（备份在本 worktree 的 merge-file-backup/）
libra merge-file ours.txt base.txt theirs.txt

# diff3 风格标记，同时展示共同祖先
libra merge-file --diff3 -p ours.txt base.txt theirs.txt
```

## 与 Git 对比

| 任务 | Libra | Git |
|------|-------|-----|
| 三路合并到 stdout | `libra merge-file -p a b c` | `git merge-file -p a b c` |
| 就地合并 | `libra merge-file a b c` | `git merge-file a b c` |
| diff3 标记 | `libra merge-file --diff3 …` | `git merge-file --diff3 …` |

差异与延后项：冲突标记标签为 `ours` / `theirs`（与 `libra merge` 一致），不是文件名；冲突退出码固定为 `1`（Git 报告冲突数量）；`-L <label>`、`--ours` / `--theirs` / `--union`、`--marker-size` 暂未公开。
