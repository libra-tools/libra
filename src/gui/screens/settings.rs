//! Account, appearance and capture privacy settings preview.
use ely_gpui_component::{
    buttons::{Button, ButtonVariant},
    theme::{ControlSize, Mode, Palette, Theme},
};
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, px,
};

use super::super::{
    desktop::{Desktop, Page},
    widgets::{
        action, agent, badge, card, check, column, field, label, muted, nav, note, row, table,
        title,
    },
};
pub fn render(s: &Desktop, cx: &Context<Desktop>, p: &Palette) -> AnyElement {
    row()
        .size_full()
        .items_stretch()
        .gap_0()
        .child(
            column()
                .w(px(240.0))
                .flex_none()
                .p_5()
                .child(title("Settings"))
                .children(
                    ["Account & tokens", "Appearance", "Capture & privacy"]
                        .iter()
                        .enumerate()
                        .map(|(i, t)| {
                            Button::new(("setting-tab", i), *t)
                                .variant(if s.tab == i {
                                    ButtonVariant::Subtle
                                } else {
                                    ButtonVariant::Ghost
                                })
                                .on_click(cx.listener(move |s, _, _, cx| {
                                    s.tab = i;
                                    cx.notify();
                                }))
                        }),
                )
                .child(nav(Page::Agents, cx))
                .child(nav(Page::Cloud, cx))
                .child(nav(Page::Automation, cx))
                .child(nav(Page::About, cx))
                .child(nav(Page::History, cx))
                .child(div().flex_1())
                .child(label("LIBRA", p))
                .child(muted(env!("CARGO_PKG_VERSION"), p)),
        )
        .child(
            column()
                .id("settings-content")
                .overflow_y_scroll()
                .flex_1()
                .p_6()
                .bg(p.surface)
                .gap_5()
                .when(s.tab == 0, |d| {
                    d.child(title("Account & tokens"))
                        .child(
                            card(p).child(
                                row()
                                    .child(agent("You"))
                                    .child(
                                        column()
                                            .gap_1()
                                            .child("Signed in to libra.tools")
                                            .child(muted("via GitHub · token expires in 27 days", p)),
                                    )
                                    .child(div().flex_1())
                                    .child(action("refresh-token", "Refresh", false, cx))
                                    .child(action("sign-out", "Sign out", false, cx)),
                            ),
                        )
                        .child(
                            row()
                                .child(title("Host tokens"))
                                .child(action("add-token", "Add token…", false, cx)),
                        )
                        .child(table(
                            &["Host", "User", "Stored in", "Status"],
                            &[
                                vec!["git.acme.internal:443", "eli", "keyring", "valid"],
                                vec!["mega.acme.internal:443", "eli", "file", "expired"],
                            ],
                            p,
                        ))
                        .child(field("TOKEN STORAGE", "Encrypted file (default) ⌄", p))
                        .child(note(
                            "Switching migrates existing tokens and verifies them before removing the old copy.",
                            p,
                        ))
                        .child(field(
                            "AFTER A SUCCESSFUL PASSWORD PROMPT",
                            "Ask to save (default No) ⌄",
                            p,
                        ))
                        .child(muted(
                            "Tokens are only sent to the exact host and port they were saved for.",
                            p,
                        ))
                })
                .when(s.tab <= 1, |d| {
                    d.child(
                        card(p).child(title("Appearance")).child(label("THEME", p)).child(
                            row()
                                .child(
                                    Button::new("theme-light", "Light")
                                        .size(ControlSize::Sm)
                                        .on_click(|_, _, cx| Theme::set_mode_now(Mode::Light, cx)),
                                )
                                .child(
                                    Button::new("theme-dark", "Dark")
                                        .size(ControlSize::Sm)
                                        .on_click(|_, _, cx| Theme::set_mode_now(Mode::Dark, cx)),
                                )
                                .child(
                                    Button::new("theme-system", "Match system")
                                        .size(ControlSize::Sm)
                                        .on_click(|_, window, cx| Theme::set_mode_now(window.appearance().into(), cx)),
                                ),
                        ),
                    )
                })
                .child(
                    card(p)
                        .child(title("Capture & privacy"))
                        .child(badge(
                            "✓ Redact credentials in transcripts and tool output · always on",
                            0x166534,
                            0xdcfce7,
                        ))
                        .child(check(
                            "semantic-summaries",
                            "Show AI semantic summaries (readable content only)",
                            true,
                            s,
                            cx,
                        ))
                        .child(check(
                            "raw-export-confirm",
                            "Require confirmation for raw export",
                            true,
                            s,
                            cx,
                        ))
                        .child(field("CLEAN UP CHECKPOINTS OF STOPPED SESSIONS AFTER", "Never ⌄", p)),
                ),
        )
        .into_any_element()
}
