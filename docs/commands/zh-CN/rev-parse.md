# `libra rev-parse`

解析修订名，并打印规范化的对象 ID、符号引用或仓库路径。

## 概要

```bash
libra rev-parse [OPTIONS] [SPEC]...
```

## 说明

`libra rev-parse` 会将类似修订的输入解析为以下三种形式之一：

- 完整对象 ID（默认）
- 使用 `--short` 得到的短对象 ID
- 使用 `--abbrev-ref` 得到的符号分支名

它还支持 `--show-toplevel`，用于打印工作树的绝对仓库根目录。未提供 `<SPEC>` 时默认为 `HEAD`；提供多个 `<SPEC>` 时各自单独成行解析。输出过滤标志（`--flags`/`--no-flags`/`--revs-only`/`--no-revs`）则把每个参数分类为 flag、revision 或 path，并打印过滤后的子集。

共享的严格对象 spec 解析器支持：`@` 作为 `HEAD`、完整 tag ref、`^N`/`~N`、`^{commit|tree|blob|tag|object}` 与递归 `^{}` peel、数字 reflog selector（`main@{1}`、`HEAD@{0}`，以及表示当前分支的 `@{1}`）和 `<tree-ish>:<path>`。解析必须消费完整输入，非法后缀不会静默退化到另一个对象。reflog 日期 selector、`@{-N}`、`@{upstream}`/`@{push}` 与相对 `./`/`../` tree path 仍不支持，并会明确失败。

Index path spec 解析当前 worktree 的 index：`:<path>` 读取 stage-0 条目，`:<n>:<path>`（n = 0–3）读取对应 merge stage（`:0:p` ≡ `:p`）；以 `./` 或 `../` 开头时相对当前目录解析，其它路径按仓库相对路径处理。路径不在 index、所请求 stage 缺失但其它 stage 存在、以及 `./`/`../` 越出工作树时均以 `LBR-CLI-003`（退出 129）失败。

提交消息搜索 spec 按提交消息正文（头区空行之后的全部内容）查找提交：`:/<regex>` 以 HEAD 加上所有本地分支、远程跟踪分支与 tag 为起点，`<rev>^{/<regex>}` 以 `<rev>` 为起点，均按提交时间从新到旧遍历祖先并返回第一个匹配。前缀 `!-` 取反匹配，`!!` 保留字面 `!`，其它 `!` 开头为错误；无效正则、正则超限或无匹配均为 `LBR-CLI-003`（退出 129）。正则使用 Rust `regex` crate（线性时间引擎，不支持反向引用与 lookaround），部分仅 POSIX ERE 可编译的模式无法使用；匹配区分大小写，遍历期间只读提交对象。

`AUTO_MERGE` 是本命令唯一接受的伪引用：非 squash 合并因冲突暂停时，它解析为包含冲突标记的自动结果 tree。`merge --continue`、`--abort` 或 `--quit` 后会消失；干净合并和 squash 合并时不可用。六个历史伪引用（`ORIG_HEAD`、`MERGE_HEAD`、`CHERRY_PICK_HEAD`、`REVERT_HEAD`、`REBASE_HEAD`、`FETCH_HEAD`）仍会作为 revision 被拒绝。

## 选项

Git describe 名称（`<ref>-<count>-g<hex>`）也可解析：在 tag、本地分支、远程分支的字面查找都未命中后，末尾的对象 ID 即目标提交（如 `v1-2-gabc1234`）；若存在恰为该名字的引用，则字面引用优先。此后缀按 Git 的 `GET_OID_COMMIT` 规则解析：缩写必须无歧义，除非候选中恰有一个提交（该提交胜出）；全长 OID 必须真实存在；含非十六进制字符或少于 4 位的后缀以 `LBR-CLI-003` 失败（不输出歧义候选列表）；后缀指向附注 tag 对象时返回该 tag 对象本身（不剥皮）；blob 与 tree 后缀同样返回该对象——根命名空间与 Git 一样接受全部对象类型；`tags/` 命名空间在本命名空间字面 tag 与「完整名字」的本地/远程跟踪分支查找之后才解析 describe 后缀，因此恰好同名的真实引用始终优先；`heads/` 与 `remotes/` 命名空间同样按「字面分支 → 完整名字的其它命名空间查找 → describe 后缀」顺序解析；`refs/` 限定形式（`refs/tags/`、`refs/heads/`、`refs/remotes/`）沿用同一顺序。
### `--show-ref-storage-format`

输出仓库的引用存储后端。Libra 的引用存于 SQLite，因此始终输出 `sqlite`（Git 的 `files`/`reftable` 后端未实现，见 `COMPATIBILITY.md` 的 intentionally-different）。`--json` 返回 `{"ref_storage_format": "sqlite"}`。

```bash
libra rev-parse --show-ref-storage-format
```


| 标志 | 说明 |
|------|-------------|
| `--verify` | 断言 `<SPEC>` 恰好解析为一个实际存在的对象；成功时打印对象 ID，失败退出 128（全局 `--quiet` / `-q` 下静默退出 1）。 |
| `--show-object-format[=<mode>]` | 打印仓库的对象格式（`sha1`、`sha256`，或 Libra 的 `blake3`）。`storage`（默认）、`input`、`output` 均报告 `core.objectformat`；`compat` 输出一个空行（Libra 没有兼容哈希）；其它取值以 `LBR-CLI-002` 拒绝。与 SPEC 互斥。 |
| `--short[=<n>]` | 打印无歧义的缩写对象 ID：从 `<n>`（默认 7，最小 4）开始逐位加长，直到没有**其它**对象共享该前缀。当前哈希长度的全长对象 ID，即使对象不在库中也接受（Git 同样会缩写）；`--verify --short` 保持该行为，而 `--verify` 不带 `--short` 仍要求对象存在。 |
| `--sq` | 对解析出的对象名做单引号 shell 引用，便于安全地交给 shell 消费。仅影响解析出的修订输出，不影响 `--show-toplevel` 等查询模式。 |
| `--abbrev-ref` | 打印符号分支名，而不是提交哈希。 |
| `--symbolic-full-name` | 将 spec 解析为完整 ref 名（`refs/heads/<分支>`、`refs/tags/<标签>`、`refs/remotes/<远程>/<分支>`，分离 HEAD 时为 `HEAD`）。有效但非 ref 的对象不输出（退出码 0）；不可解析名以退出码 128 失败。 |
| `--symbolic` | 按“尽量接近原始输入”的形式打印 spec：可解析的 ref、revision 表达式或对象 id 一律**原样回显**（如 `main` 保持 `main`，而非 `refs/heads/main`）。不可解析名以退出码 128 失败。与 `--symbolic-full-name`/`--short`/`--abbrev-ref` 互斥。 |
| `--flags` | 输出过滤模式：把每个 `<SPEC>` 参数分类，仅打印 flag（以 `-` 开头、在 `--` 之前）与解析出的 revision，丢弃非 flag 的 path。 |
| `--no-flags` | 输出过滤模式：丢弃 flag 参数，保留解析出的 revision 与非 flag 的 path。 |
| `--revs-only` | 输出过滤模式：仅打印解析为 revision 的参数（以对象名输出），丢弃 flag 与非 revision 的 path。 |
| `--no-revs` | 输出过滤模式：丢弃 revision 参数，保留 flag 与非 revision 的 path。 |
| `--show-toplevel` | 打印顶层工作树的绝对路径。 |
| `--is-inside-git-dir` | 当前目录位于 `.libra`（Libra 的 `$GIT_DIR`）内时打印 `true`，否则打印 `false`。 |
| `--is-shallow-repository` | 当 `.libra/shallow` 至少包含一个 shallow boundary 时打印 `true`，否则打印 `false`。 |
| `--git-dir` | 打印 `.libra` 目录路径（Libra 的 `$GIT_DIR`）；在 Libra 中始终为绝对路径。 |
| `--absolute-git-dir` | 同 `--git-dir`，但始终为规范化后的绝对路径。（Libra 中 `--git-dir` 已是绝对路径，故两者一致。） |
| `<SPEC>...` | 要解析的对象/修订 spec（可多个），支持 typed peel、数字 reflog selector、`REV:path`、index path（`:<path>`、`:<n>:<path>`）与提交消息搜索（`:/<regex>`、`<rev>^{/<regex>}`）。省略时默认为 `HEAD`；多个时各自解析。 |

## 常用命令

```bash
libra rev-parse
libra rev-parse HEAD~1
libra rev-parse 'HEAD^{tree}'
libra rev-parse 'HEAD:src/main.rs'
libra rev-parse :src/main.rs
libra rev-parse :2:src/main.rs
libra rev-parse ':/fix: crash'
libra rev-parse 'HEAD^{/fix}'
libra rev-parse 'main@{1}'
libra rev-parse --short HEAD
libra rev-parse --abbrev-ref HEAD
libra rev-parse --show-toplevel
libra rev-parse --is-shallow-repository
libra rev-parse --absolute-git-dir
libra --json rev-parse --short HEAD
```

## 人类可读输出

默认输出为包含已解析值的单行。

```text
abc1234def5678901234567890abcdef12345678
```

使用 `--short`：

```text
abc1234
```

使用 `--abbrev-ref`：

```text
main
```

使用 `--show-toplevel`：

```text
/home/alice/project
```

## 结构化输出

```json
{
  "ok": true,
  "command": "rev-parse",
  "data": {
    "mode": "short",
    "input": "HEAD",
    "value": "abc1234"
  }
}
```

`mode` 是 `resolve`、`short`、`abbrev_ref`、`symbolic_full_name`、`symbolic`、`show_toplevel`、`show_prefix`、`show_cdup`、`is_inside_work_tree`、`is_inside_git_dir`、`is_bare_repository`、`is_shallow_repository`、`git_dir` 或 `absolute_git_dir` 之一。

单个 `<SPEC>` 时，`data` 是上述单个对象；**多个** `<SPEC>` 时 `data` 是这些对象按顺序组成的 JSON **数组**（每个 spec 一项）。在**输出过滤**模式（`--flags`/`--no-flags`/`--revs-only`/`--no-revs`）下，`data` 是过滤后 token 的 JSON **字符串数组**（revision 为解析出的对象名，保留的 flag/path 原样）。

## 参数对比：Libra vs Git vs jj

| 功能 | Libra | Git | jj |
|---------|-------|-----|----|
| 解析完整对象 ID | `rev-parse <spec>` | `git rev-parse <spec>` | `jj log -r <rev> --no-graph -T commit_id` |
| typed/递归 peel | `^{commit/tree/blob/tag/object}` / `^{}` | 同 | N/A |
| tree path | `REV:path` | 同 | `jj file show -r REV path` |
| reflog selector | 数字 `ref@{N}` | 数字/日期/upstream/push/checkout 形式 | N/A |
| 缩写对象 ID | `--short` | `--short` | `jj log -r <rev> -T change_id.short()` |
| 符号分支名 | `--abbrev-ref` | `--abbrev-ref` | N/A |
| 完整 ref 名 | `--symbolic-full-name` | `--symbolic-full-name` | N/A |
| 符号（原样）名 | `--symbolic` | `--symbolic` | N/A |
| 输出过滤 | `--flags`/`--no-flags`/`--revs-only`/`--no-revs` | 同 | N/A |
| Shell 引用输出 | `--sq` | `--sq` | N/A |
| 工作树根目录 | `--show-toplevel` | `--show-toplevel` | `jj root` |
| JSON 输出 | `--json` | 无 | 无 |

`--` 分隔符在所有模式下都把 revision 与 path 分开（`rev-parse <rev> -- <path>`）：`--` 之后的参数一律是 path 而非 revision，且在有 path 输出的模式下会回显 `--`。单 revision 模式 `--verify`、`--short` 只打印解析出的那一个对象（不打印 path）。

匹配到多个**不同**对象的短对象名会以 `error: short object ID <p> is ambiguous` 失败，并给出 `The candidates are:` 提示，每个候选一行 `<abbrev> <type>`（按完整对象 id 排序），退出码 129（`LBR-CLI-003`；git 为 128）。`--json` 时错误 envelope 的 `details.candidates` 携带每个候选的完整 `oid`、`type` 与最短唯一 `abbrev`。同一对象的重复命中（如松散对象同时存在于 pack）会先合并，不计为歧义。

> **与 git 的有意差异**：`--verify` 或 `--short` 与任一输出过滤标志（`--flags`/`--no-flags`/`--revs-only`/`--no-revs`）组合会以用法错误（`LBR-CLI-002`，退出码 129）被拒绝。git 在该组合下行为不确定，Libra 选择拒绝而非猜测。

## 错误处理

| 场景 | StableErrorCode | 退出码 |
|----------|-----------------|------|
| 无效目标引用 | `LBR-CLI-003` | 129 |
| 歧义短对象名 | `LBR-CLI-003` | 129 |
| `--verify`/`--short` 与输出过滤标志组合 | `LBR-CLI-002` | 129 |
| 无效工作树状态 | `LBR-REPO-003` | 128 |
| 无法读取仓库元数据 | `LBR-IO-001` | 128 |
| 存储的引用/配置损坏 | `LBR-REPO-002` | 128 |

rev-parse resolves MERGE_AUTOSTASH while held.

对称差 `A...B`（省略一侧时取 `HEAD`）依次输出 `<B_oid>`、`<A_oid>`，再为每个最佳共同祖先输出 `^<merge-base>`；无共同祖先时只输出两个提交 id。
