# `libra check-mailmap`

通过仓库 `.mailmap` 解析 `Name <email>` 联系人 —— `git check-mailmap` 的一个聚焦子集。

## 用法

```
libra check-mailmap <contact>...
libra check-mailmap --stdin
```

## 说明

对每个 `Name <email>` 联系人（作为参数，或带 `--stdin` 时每行一个从 stdin 读取），`check-mailmap` 在工作树的 `.mailmap` 中查找并打印规范的 `Name <email>`。无 mailmap 匹配的联系人原样打印。姓名与邮箱按 ASCII 大小写不敏感比较，且 `libra shortlog` 共用同一解析器。

`.mailmap` 行采用常见的 Git 形式：

```
Proper Name <proper@example.com>
<proper@example.com> <commit@example.com>
Proper Name <proper@example.com> <commit@example.com>
Proper Name <proper@example.com> Commit Name <commit@example.com>
```

对同一 email，带 commit 名字的 `(name, email)` 规则优先于仅 email 的规则，与 Git 一致。注释（`#`）与空行被忽略。

## 选项

| 选项 | 说明 | 示例 |
|------|------|------|
| `<contact>...` | 要解析的联系人，每个为 `Name <email>`。 | `libra check-mailmap 'Bob <bob@old>'` |
| `--stdin` | 从 stdin 读取联系人（每行一个）。 | `… \| libra check-mailmap --stdin` |
| `--json` / `--machine` | 结构化输出：`{ contacts: [...] }`。 | `libra --json check-mailmap 'B <b@x>'` |

## 退出码

| 退出码 | 含义 |
|--------|------|
| `0` | 已打印解析后的联系人。 |
| `128` | 不在仓库内、未给联系人，或联系人缺少 `<email>`。 |

## 示例

```bash
echo 'Old Name <old@example.com>' | libra check-mailmap --stdin
libra check-mailmap 'Old Name <old@example.com>' 'Other <other@example.com>'
```

## 与 Git 对比

| 任务 | Libra | Git |
|------|-------|-----|
| 解析联系人 | `libra check-mailmap '<c>'` | `git check-mailmap '<c>'` |
| 从 stdin | `libra check-mailmap --stdin` | `git check-mailmap --stdin` |

差异与延后项：仅读取工作树 `.mailmap`（暂不支持 `mailmap.file` / `mailmap.blob` 配置）；解析器尚未接入 `log` / `blame` 的作者显示 —— 该集成为已记录的后续项。
