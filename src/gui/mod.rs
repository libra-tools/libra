//! Desktop window for `libra ui`.
//!
//! GPUI owns the process main thread. [`run`] is called from `main` before
//! the CLI worker thread starts. It is not safe to call from `cli::parse`.

use std::sync::{Arc, Mutex};

use anyhow::Context;
mod assets;
mod desktop;
mod model;
mod screens;
mod view;
mod widgets;

use desktop::Desktop;
use gpui::{
    App, AppContext, Bounds, KeyBinding, Menu, MenuItem, TitlebarOptions, WindowBounds,
    WindowOptions, px, size,
};

gpui::actions!(libra_application, [Quit]);

/// Open one window and run until the user closes it.
///
/// # Errors
///
/// Returns an error when Ely cannot load its fonts or the window fails to
/// open. The message names that step.
pub fn run() -> anyhow::Result<()> {
    let startup_error = Arc::new(Mutex::new(None));
    let reported = Arc::clone(&startup_error);
    gpui_platform::application()
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            if let Err(error) = open_window(cx) {
                match reported.lock() {
                    Ok(mut slot) => *slot = Some(error),
                    Err(poisoned) => *poisoned.into_inner() = Some(error),
                }
                cx.quit();
            }
        });
    match startup_error.lock() {
        Ok(mut slot) => slot.take().map_or(Ok(()), Err),
        Err(poisoned) => poisoned.into_inner().take().map_or(Ok(()), Err),
    }
}

fn open_window(cx: &mut App) -> anyhow::Result<()> {
    ely_gpui_component::init(cx).context("Ely failed to start")?;
    use ely_gpui_component::theme::{Mode, Palette, Theme};
    let mut light = Palette::light(false);
    light.bg = gpui::rgb(0xf4f5f7).into();
    light.surface = gpui::rgb(0xffffff).into();
    light.border = gpui::rgb(0xe3e6eb).into();
    light.fg = gpui::rgb(0x111827).into();
    light.fg_muted = gpui::rgb(0x5b6474).into();
    light.selection = gpui::rgb(0xeaf1ff).into();
    light.accent = gpui::rgb(0x2563eb).into();
    light.accent_hover = gpui::rgb(0x1d4ed8).into();
    light.on_accent = gpui::rgb(0xffffff).into();
    light.focus = light.accent;
    light.link = light.accent;
    light.hover = gpui::rgb(0xeef2f6).into();
    Theme::set_palette(Mode::Light, Some(light), cx);
    let mut dark = Palette::dark(false);
    dark.bg = gpui::rgb(0x0d1117).into();
    dark.surface = gpui::rgb(0x151b23).into();
    dark.sunken = gpui::rgb(0x1b222c).into();
    dark.overlay = dark.surface;
    dark.hover = gpui::rgb(0x222b38).into();
    dark.active = gpui::rgb(0x1b2433).into();
    dark.border = gpui::rgb(0x2a313c).into();
    dark.fg = gpui::rgb(0xe6eaf0).into();
    dark.fg_muted = gpui::rgb(0x9aa4b2).into();
    dark.selection = gpui::rgb(0x17264a).into();
    dark.accent = gpui::rgb(0x3b82f6).into();
    dark.accent_hover = gpui::rgb(0x2563eb).into();
    dark.on_accent = gpui::rgb(0xffffff).into();
    dark.focus = dark.accent;
    dark.link = dark.accent;
    Theme::set_palette(Mode::Dark, Some(dark), cx);
    Theme::set_mode_now(Mode::Light, cx);
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-w", Quit, None),
    ]);
    cx.set_menus([Menu::new("Libra").items([MenuItem::action("Quit Libra", Quit)])]);
    cx.on_window_closed(|cx, _| {
        if cx.windows().is_empty() {
            cx.quit();
        }
    })
    .detach();
    let bounds = Bounds::centered(None, size(px(1440.0), px(900.0)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(1120.0), px(700.0))),
        titlebar: Some(TitlebarOptions {
            title: Some("Libra".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    cx.open_window(options, |window, cx| cx.new(|cx| Desktop::new(window, cx)))
        .context("failed to open the Libra window")?;
    cx.activate(true);
    Ok(())
}
