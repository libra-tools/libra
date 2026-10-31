# `libra ui`

打开使用内置演示数据的 Libra 桌面设计预览。这是 Libra 扩展，Git 没有对应命令。

窗口只在使用 `gui` feature 构建时编进二进制。默认 `cargo build` 仍然是 CLI；
在那个二进制上执行 `libra ui` 会说明如何重新构建。

## 用法

```
libra ui
```

## 说明

不带其它参数的 `libra ui` 在进程主线程打开 Libra 桌面界面，默认显示 History。
使用 [Ely GPUI Components](https://github.com/ELY-Open-Source/Ely-GPUI-Components)
仓库版本 `dd727f4` 和 `gpui-pre` 0.3.10，实现提交图、作者筛选和创作历史检查器。
顶栏和 **All tools**（`⌘K`）可进入全部 26 个设计页面：

- History、Agent lanes、Sessions（Overview / Conversation / Changes）、能力地图、
  搜索、操作日志、Memory。
- Agent 信任与诊断、评审运行、调查、合并评审、自动化。
- 欢迎／导入、冲突解决、Blame、Bisect、Stashes／Tags／Worktrees、交互式 Rebase。
- Cloud／Remotes、Sparse view／Layers、依赖图、Mega2、设置、启动页、启动错误页、About。

启动页和启动错误页是在 All tools 中查看的设计状态，不执行真实启动检查。
Sessions 支持浅色／深色、可读内容搜索和本地筛选。表单输入与复选框只影响预览，
不会保存配置。

这是桌面设计的界面预览，所有记录和状态均为内置演示数据。导航、选择、标签和
主题切换仅改变界面状态；仓库、Agent、信任、恢复、网络和账户操作不会执行。
Logo 来自官网，字体和图片内置，离线可用。当前以 macOS 为验证平台。
窗口显示 **Design preview** 标记，未接入的操作会显示预览提示。
关闭窗口或按 `⌘Q` 退出；`Esc` 关闭命令面板。

此命令不读取、不写入仓库。

`libra ui --help` 只打印用法，不打开窗口。其它额外参数是用法错误，同样不打开窗口。

没有 `--features gui` 的构建以 `LBR-UNSUPPORTED-001` 退出 128，并给出
`cargo build --features gui`。

通过库调度器（`libra::exec`）调用不会打开窗口。界面必须从 `libra` 进程入口启动，
因为 UI 工具包占用主线程。

## 选项

`ui` 自己没有标志。子命令前的全局标志（`libra --no-pager ui`）会被窗口忽略。

## 退出码

| 退出码 | 含义 |
|--------|------|
| `0` | 窗口已打开并随后关闭。需要 `--features gui`。 |
| `128` | 此二进制没有桌面界面（`LBR-UNSUPPORTED-001`），或窗口打开失败。 |
| `129` | 额外参数。`libra ui --help` 退出 0。 |

## Examples

```bash
# 默认 CLI 构建：说明缺少 feature，退出 128。
libra ui

# 把窗口编进同一个 libra 二进制，然后打开。
cargo build --features gui
./target/debug/libra ui

# 用法文本。不打开窗口。
libra ui --help
```

## 与 Git 的比较

Git 没有桌面子命令。`ui` 在 [`COMPATIBILITY.md`](../../../COMPATIBILITY.md) 中是
`intentionally-different`。
