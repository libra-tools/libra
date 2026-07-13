# `libra ls-remote`

列出远程仓库通告的引用，不下载对象，也不更新本地引用。

```bash
libra ls-remote [OPTIONS] <repository> [patterns...]
```

在 Libra 仓库内运行时，`<repository>` 可以是已配置的远程名称，也可以是 URL，或本地 Git/Libra 仓库路径。

## 选项

| 标志 | 说明 | 示例 |
|------|-------------|---------|
| `--heads` | 只显示 `refs/heads/*` 分支引用 | `libra ls-remote --heads origin` |
| `-t`, `--tags` | 只显示 `refs/tags/*` 标签引用 | `libra ls-remote --tags origin` |
| `--refs` | 省略 `HEAD` 和以 `^{}` 结尾的 peeled 标签引用 | `libra ls-remote --refs origin` |
| `--symref` | 在对应 OID 行之前打印 symbolic-ref 目标（如 `ref: refs/heads/main\tHEAD`）。远端通告的 `symref=` capability 优先；缺失 capability 时（尤其本地 Libra 源），使用与 fetch 相同的 HEAD OID / 分支 tip 解析器合成 `HEAD`。 | `libra ls-remote --symref origin` |
| `patterns...` | 匹配完整引用名或尾部路径组件；`*` 和 `?` 遵循 Git 风格 glob 行为，并且可以匹配 `/` | `libra ls-remote origin main 'refs/heads/*'` |

## 人类可读输出

每个匹配引用按如下格式打印：

```text
<object-id>	<refname>
```

示例：

```text
4f3c2d1a...	HEAD
4f3c2d1a...	refs/heads/main
```

## JSON 输出

使用 `--json` 时，输出使用标准命令信封：

```json
{
  "ok": true,
  "command": "ls-remote",
  "data": {
    "remote": "origin",
    "url": "https://example.com/repo.git",
    "heads_only": false,
    "tags_only": false,
    "refs_only": false,
    "patterns": [],
    "entries": [
      {
        "hash": "4f3c2d1a...",
        "refname": "refs/heads/main"
      }
    ]
  }
}
```

## 示例

```bash
# 列出具名远程的所有引用
libra ls-remote origin

# 直接列出 URL 的所有引用（不需要注册远程）
libra ls-remote https://example.com/repo.git

# 限制为匹配模式的分支
libra ls-remote --heads origin main

# 面向代理的结构化 JSON 信封，仅标签
libra --json ls-remote --tags origin
```

`libra ls-remote --help` 会渲染同一横幅，因此文档和 CLI 表面保持同步（跨命令 `--help` EXAMPLES 推出，见 `docs/development/commands/_general.md` 条目 B）。

## 说明

- `ls-remote` 只执行协议发现（对本地 Git 仓库等价于 `git-upload-pack --advertise-refs`）。
- 它不会写入对象、远程跟踪引用、配置或工作树文件。
- `--heads` 和 `--tags` 可以组合使用，以同时显示分支和标签引用，同时排除 `HEAD`。
