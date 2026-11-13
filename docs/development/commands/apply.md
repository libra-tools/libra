# apply 命令开发设计

## 命令实现目标

`libra apply --check` 校验一个 unified-diff 补丁能否干净应用到当前工作树，**不写入**。MVP：解析 + 路径安全 + 试应用 + 退出码；真正写入（临时文件 + 原子 rename）为后续扩展。

## 对比 Git 与兼容性

- 兼容级别：`partial`。
- 已支持：`apply --check [-p<n>] [<patch>...]`（无文件时读 stdin）、单/多文件 unified diff、新增（`--- /dev/null`）/修改/删除（`+++ /dev/null`）、`--json`/`--machine`。
- 退出码：0 可应用 / 1 不可应用（上下文冲突或目标缺失）/ 128 错误（非仓库、未带 `--check`、格式错误/超大(>64 MiB)/非 UTF-8、目标路径不安全）。
- 未公开（延后）：真正写入（无 `--check`）、`--index`/`--cached`、`--3way`、`--reverse`、`--unidiff-zero`、二进制补丁、rename/mode hunk。

## 设计方案

- 入口与分发：`src/cli.rs::Commands::Apply` → `command::apply::execute_safe`。
- 源码分层：`src/command/apply.rs`：`ApplyArgs`（`check`/`strip`(`-p<n>` 默认 1)/`patches`）、`execute`/`execute_safe`、`ApplyOutput`（`--json`：`applies`/`files`）、`read_patch`/`split_file_patches`/`patch_target`/`strip_path`/`resolve_safe`。
- 合并/补丁核心：`diffy::Patch::from_str`（跳过 `diff --git`/`index` 前导）+ `diffy::apply(base, &patch)`（Ok=可应用 / Err=不可应用）。与 `merge`/`merge-file` 同一 `diffy` 引擎。
- 多文件拆分（`split_file_patches`）：含 `diff --git ` 则按其行拆；否则按「`--- ` 行且下一行 `+++ `」拆（避免把内容里的 `--- ...` 删除行误判为文件头）。
- 路径解析与安全（`patch_target` + `strip_path` + `resolve_safe`）：目标取 modified 侧（删除取 original 侧），`-p<n>` 剥离前导组件；`resolve_safe` 拒绝绝对路径、空/`.`/`..` component、NUL、`.libra/` 内部，防止 path alias 绕过 collision check，并用 `util::is_sub_path` 守卫越出工作树；`reject_symlink_components` 拒绝通过已有 symlink 逃逸 → 128。
- 内部复用 seam：`prepare_patch` 先在 memory 中算出所有 target 结果，`PreparedPatch::write` 才以 atomic rename 写入并恢复已有 permissions；仅供 `am` 使用，公开 `apply` 仍严格 check-only。
- 资源边界：补丁 > 64 MiB → 128（`MAX_PATCH_BYTES`，stdin 用 `take(cap+1)`）。
- 输入：positional 补丁文件（拼接）或 stdin（无文件时）。
- 退出语义：任一文件不可应用 → `silent_exit(1)`（工作树未触碰）；解析/路径/大小错误 → 128。
- 底层操作对象：补丁输入 + 只读工作树文件。**无写入**（--check）。

## 实现历史

- 2026-06-30（GGT-10，`grit-gap.md` 阶段 3）：新增 `apply --check` MVP。

## 当前状态

- 公开状态：已公开（`Commands::Apply`）。
- 测试：`tests/command/apply_test.rs`（干净修改 exit 0、上下文不符 exit 1、新文件、多文件、`-p0`、stdin、路径越界/非规范 alias/symlink component/`.libra/` 128、缺 `--check` 128、格式错误 128、`--json`、非仓库 128）。
- 用户文档：`docs/commands/apply.md`（EN + zh-CN）。

## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| 写入模式 | 真正应用（无 `--check`）+ 临时文件 + 原子 rename + 回滚 | **有意延后**（计划「写入模式（未来扩展）」）；当前必须带 `--check`，否则 128。 |
| 兼容差异项 | `--index`/`--cached`/`--3way`/`--reverse`/`--unidiff-zero`/二进制/rename/mode | 延后。 |
| hunk 上限 | 1 MiB hunk 行数上限 | 暂仅补丁总大小上限（64 MiB）；hunk 级上限后续补。 |

## 维护要求

- 改进本命令前先阅读 [docs/development/commands/_general.md](_general.md)。
- 补丁解析/应用必须继续走 `diffy`（与 `merge`/`merge-file` 一致）；写入模式落地时必须实现临时文件 + 原子 rename + 失败清理，绝不留部分写入。

- 2026-11-13（#478 LG-11，ADR-LG-07 第 1–3 条）：默认应用在全部准备/检查通过后原子写出工作树（多文件补丁任一失败零写入）；新增 `--numstat`（`ins\tdel\tpath`，二进制 `-\t-`）、`--summary`（create/delete/mode change）与 `--apply`（与报告同用时先报告后应用）；`--stat` 对齐 Git 的名称/计数列宽并省略零插入/删除子句；`-R/--reverse` 回归。测试：`test_apply_worktree_and_reports_matrix`（t4106:15、t4102:38 移植）、`apply_test` 18/18、`compat_mail_am_basic` 41/41。

