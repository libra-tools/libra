# `libra completions`

为 `libra` CLI 生成 shell completion 脚本。这对应 Lore 的 `completions` 命令和 `git completion` contrib 脚本的人体工学，让 shell 可以 tab 补全 Libra 子命令和标志。

## 概要

```
libra completions <shell>
```

`<shell>` 是 `bash`、`zsh`、`fish`、`powershell` 或 `elvish` 之一。

## 说明

`completions` 将请求 shell 的 completion 脚本打印到 stdout。脚本由 Libra 当前 clap 命令树生成，因此总是反映当前子命令和标志表面，不需要手动维护表格。

该命令不读取仓库状态，也可在仓库外工作。

安装方式是把脚本重定向到 shell 会加载 completions 的位置，或用 `eval` 将其加载到当前 shell 做一次性会话使用。

## 选项

| 选项 | 说明 | 示例 |
|------|------|------|
| `<shell>` | 目标 shell：`bash`、`zsh`、`fish`、`powershell` 或 `elvish`。 | `libra completions zsh` |
| `--json` / `--machine` | 结构化输出：`{ shell, script }`。 | `libra --json completions bash` |

## 退出码

| 代码 | 含义 |
|------|------|
| `0` | Completion 脚本已写出。 |
| `129` | 未知或缺失 shell 参数（Git 风格 clap 用法错误）。 |

## 示例

```bash
# bash：系统级安装
libra completions bash | sudo tee /etc/bash_completion.d/libra >/dev/null

# zsh：放入 $fpath 中的某个目录
libra completions zsh > ~/.zsh/completions/_libra

# fish
libra completions fish > ~/.config/fish/completions/libra.fish

# 不安装，直接加载到当前 shell
eval "$(libra completions bash)"

# 面向工具的结构化输出
libra --json completions bash
```

## 与 Git 对比

Git 没有 `git completions` 子命令；completion 脚本位于 `contrib/completion` 目录并需要手动 source。Libra 将生成器直接暴露为 `libra completions <shell>`，因此该命令在 [`COMPATIBILITY.md`](../../../COMPATIBILITY.md) 中分类为 `intentionally-different`。
