# replace 命令开发设计

## 命令实现目标

`libra replace` —— 在对象读取时用另一个对象替换它（`refs/replace/*`）。GGT-13 互操作池命令之一（独立增量）。核心是「全链路 peel」：替换在 `load_object` 生效，故 `log`/`show`/`rev-parse` peel 等所有经 load_object 的读取者都遵守（acceptance「不只改一个调用点」）。

## 对比 Git 与兼容性

- 兼容级别：`partial`。
- 已支持：`replace [-f] <object> <replacement>`（创建，类型须一致除非 `-f`，已存在须 `-f`，禁止自替换）、`-d <object>...`（删除）、`-l [<pattern>]`（列出，默认）。peel 经 `load_object` 覆盖 log/show/rev-parse-peel。
- **有意差异/延后**：替换存为 `.libra/refs/replace/<oid>` 松散 ref（非 SQLite reference 表），故 `show-ref`/`for-each-ref` 暂不列出；`--edit`/`--graft`/`--convert-graft-file` 未实现。

## 设计方案

- 入口与分发：`src/cli.rs::Commands::Replace` → `command::replace::execute_safe`（require_repo）。
- 源码分层：`src/command/replace.rs`：`ReplaceArgs`（force/delete/list/args）、`resolve`（peel 钩子，pub）、`load_replace_map`、`create`/`delete`/`list`/`resolve_any`。
- **peel 钩子（关键）**：`command::load_object`（`src/command/mod.rs`）在读取前调用 `replace::resolve(*hash)`。`resolve` 读进程级 `OnceLock<HashMap<oid,oid>>`（首次用 `load_replace_map` 扫描 `.libra/refs/replace/` 填充——纯 fs，无 async/DB，可在 sync 的 load_object 内安全调用），按链解析（`MAX_REPLACE_DEPTH=8` 防环）。**无替换时为近零成本 no-op**（map 空直接返回），故对现有全部读取路径零行为变化。
  - ⚠️ 进程级缓存：每个 CLI 调用是新进程→新缓存（正确）。同进程内创建替换后再读不会刷新缓存——CLI 不这么用；集成测试用子进程（每命令新进程）故正确。
- 存储：`refs/replace/<obj-oid>` 文件，内容为 `<repl-oid>\n`。选择松散 ref 而非 reference 表，因 `reference.kind` 有 CHECK `IN ('Branch','Tag','Head')`，新增 'Replace' 需 SQLite 表重建迁移（高风险）；松散 ref 是 Git-faithful 且让 sync peel 钩子免于 async DB。
- create：`resolve_any`（全长 hex oid 且存在→任意类型；否则 get_commit_base 解析 ref/commit-ish）解析两端；类型须一致除非 `-f`；禁止自替换；已存在须 `-f`；写 `<repl>\n`。
- delete：解析→删除文件；NotFound→128。list：扫描目录，每行打印一个 oid（Git 默认短格式），按**子串**过滤（非 glob），排序打印；`--format`（含 `<old> -> <new>`）与 glob 延后。
- 底层操作对象：读对象库（类型/存在性）；写/删 `.libra/refs/replace/` 文件。

## 实现历史

- 2026-06-30（GGT-13 / 4，`grit-gap.md` 阶段 6）：互操作池第四个命令；独立增量。

## 当前状态

- 公开状态：已公开（`Commands::Replace`）。
- 测试：`tests/command/replace_test.rs`（**核心 peel**：replace HEAD HEAD~1 后 `log -1` 显示 "base" 而非 "second"；list 含被替换 oid；delete 恢复原始；已存在须 `-f`（128/0）；删除不存在 128；坏对象 128；非仓库 128）。
- 用户文档：`docs/commands/replace.md`（EN + zh-CN）。

## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| 存储 | 接入 SQLite reference 表（show-ref/for-each-ref 可见） | 延后；当前松散 ref（避免 CHECK 迁移）。 |
| 选项 | `--edit`/`--graft`/`--convert-graft-file` | 延后。 |

## 维护要求

- 改进本命令前先阅读 [docs/development/commands/_general.md](_general.md)。
- ⚠️ `resolve` 在 **hot** 的 load_object 路径上——任何改动必须保持「无替换时近零成本」且 sync（不得引入 async/DB 调用）。

- 2026-11-23（PM-04 / #758）：`-l` 的 `<pattern>` 改用 `command::log::decorate_filter::wildmatch` 整名匹配（默认 `*`），取代子串过滤；`validate_modes` 在任何读写前拒绝 Git 的全部误用——`-d`+`-l`、非列表的 `--format`、非写入的 `-f`、多个 `-l` 模式、以及创建模式位置参数≠2（移除「无参数/单参数即列表」的隐式行为）；各错误均 `LBR-CLI-002`。回归：`replace_list_uses_wildmatch`（AC1–AC2）、`replace_rejects_invalid_mode_combinations`（AC3–AC8）。
