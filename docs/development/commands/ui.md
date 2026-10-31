# ui 命令开发设计

## 命令实现目标

`libra ui` 在同一个 `libra` 二进制里打开一个桌面窗口。窗口使用 Ely GPUI
Components（GPUI 快照 `gpui-pre` 0.3.10）。feature `gui` 默认关闭，普通 CLI
构建不链接 GPU UI。

## 对比 Git 与兼容性

- 兼容级别：`intentionally-different`。Git 无对应命令。
- 已支持：不带额外参数的 `libra ui`。带 `--features gui` 时在进程主线程打开一个窗口；否则退出 128 / `LBR-UNSUPPORTED-001`，并提示 `cargo build --features gui`。
- `libra ui --help` 只打印用法，不打开窗口。
- 不需要仓库。

## 设计方案

- GPUI 的 `Application::run` 必须在进程主线程上调用。`src/main.rs` 在 helper 提前返回和 tracing 初始化之后、启动 `libra-cli` 线程之前，用 `cli::ui_window_launch` 识别裸 `libra ui`（子命令前可以有全局 flag，子命令后不能有参数）。
- 窗口实现在 `src/gui/mod.rs`，仅 `feature = "gui"` 编译。入口是 `gui::run`。
- clap 命令始终存在：`Commands::Ui` → `command::ui::execute`。调度器路径不打开窗口。库嵌入（`libra::exec`）走到这里时返回说明，避免在错误线程上启动 GPUI。
- 预检是 `CommandPreflight::none()`，scope 是 `ReadOnly`。不参与自动升级检查。

## 实现历史

- 2026-10-24：第一扇窗口。Ely 的 `FocusScope`、`Heading`、`Paragraph` 和 `Button`。依赖钉在 Ely `6b0d154dc5c3ec3daf3bf059cc77cd8ed11ba2df` 与 `gpui-pre` `=0.3.9`。

## 当前状态

`libra ui` 打开桌面 History 页面。`src/gui/desktop.rs` 管理页面与本地预览状态，
`src/gui/screens/` 放各设计页面，`widgets.rs` 提供原生 GPUI 布局和 Ely 控件。
Sessions 保留演示目录 `src/gui/sessions.json`，不读取仓库。官网 Logo 内嵌在
`src/gui/assets/libra-logo.svg`，通过组合 AssetSource 与 Ely 的字体、图标一起加载。

2026-11-01 桌面设计来自 `libra-desktop-gui.zip`。`.dc.html` 是设计资料，
不是运行时依赖；UI 不使用浏览器或 WebView。依赖更新到 Ely `dd727f4` 与 GPUI 0.3.10。

## 还未实现的功能

- 把 status、log、diff 接进窗口。
- 从窗口调用 Libra 库函数，而不是每次新建一个 Tokio runtime。
- 发布构建是否带 `gui`。当前 release 流水线不启用这个 feature。

## 桌面界面预览（2026-11-01）

26 个设计入口由 `Page::ALL` 注册，并由枚举穷尽匹配分派到原生视图。
History 为默认页，All tools / `⌘K` 是完整页面入口。各页单独存放在
`src/gui/screens/`；Sessions 沿用 `view.rs` 和 `sessions.json` 的可读内容边界。
Ely 提供按钮、输入框、复选框、选择标签、主题、焦点与弹层；GPUI 提供布局、
提交图、代码与归因行。没有 HTML/WebView，也不加载 `.dc.html` 的画布脚本。

所有状态均为演示状态；`widgets::action` 只显示预览提示，不调用命令、数据库、
网络、凭据存储或外部 Agent。表单状态仅在窗口内存中存在。启动图示不会假装
完成真实配置或安全检查，About 的版本、工具包、系统与架构来自构建信息。

官网 Logo 的来源为 `https://libra.tools/libra-logo-without-words.svg`，
获取日期为 2026-11-01。图标、字体、Logo 随二进制内嵌，不依赖在线资源。
本次验收平台为 macOS；Linux、Windows 不在此次测试范围。

## macOS 界面验证

- `cargo build --features gui` 后运行 `./target/debug/libra ui`。从 All tools
  打开全部 26 页，同时检查浅色和深色的 Sessions、字段编辑、复选框及无结果搜索。
- `⌘K` 聚焦命令面板，`Esc` 返回；History 和全局搜索使用独立输入状态。
  `⌘Q`、`⌘W` 与关闭最后一个窗口均结束 UI 进程。
- `cargo test --features gui --lib gui::` 覆盖演示目录及 History 的组合筛选。
- GPUI 调试构建会拒绝重复的无障碍节点 ID。导航栏、页面、搜索分类和重复卡片
  各有独立 element ID；回归检查需包括搜索的 Everything 标签与 Welcome、Settings。
