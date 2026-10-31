//! The launch artboard is an inspectable preview, never a real startup check.
use ely_gpui_component::theme::Palette;
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, div, img, px, rgb};

use super::super::{
    desktop::{Desktop, Page},
    widgets::{column, mono, nav, row, title},
};
pub fn render(_: &Desktop, cx: &Context<Desktop>, _: &Palette) -> AnyElement {
    column()
        .size_full()
        .items_center()
        .justify_center()
        .gap_5()
        .child(
            row()
                .w(px(720.0))
                .h(px(440.0))
                .gap_0()
                .items_stretch()
                .rounded(px(14.0))
                .overflow_hidden()
                .bg(rgb(0x0b1530))
                .text_color(rgb(0xffffff))
                .child(
                    column()
                        .w(px(300.0))
                        .flex_none()
                        .justify_center()
                        .p_8()
                        .gap_4()
                        .border_r_1()
                        .border_color(rgb(0x1c2a4d))
                        .child(
                            div()
                                .w(px(116.0))
                                .p_3()
                                .rounded(px(22.0))
                                .bg(rgb(0xffffff))
                                .child(img("libra/logo.svg").size(px(92.0))),
                        )
                        .child(title("Libra").text_size(px(38.0)))
                        .child(
                            div()
                                .text_color(rgb(0xb4c3e0))
                                .child("Version control that remembers how the work was done."),
                        )
                        .child(
                            mono(format!("v{}", env!("CARGO_PKG_VERSION")))
                                .text_color(rgb(0x9fb0cf)),
                        ),
                )
                .child(
                    column()
                        .flex_1()
                        .p_8()
                        .gap_4()
                        .child(title("Starting").text_size(px(15.0)))
                        .child(
                            div()
                                .text_color(rgb(0x9fb0cf))
                                .child("Opening your last repository"),
                        )
                        .children(
                            [
                                ("✓", "Configuration", "Global and repository schema checked"),
                                ("✓", "Repository", "acme-libra · main · sha1 objects"),
                                (
                                    "○",
                                    "Agent adapters",
                                    "Claude Code, Codex trusted · OpenCode closed",
                                ),
                                ("○", "Memory projection", "Checking freshness"),
                                ("○", "Sandbox", "Detecting backend"),
                            ]
                            .iter()
                            .map(|(mark, name, detail)| {
                                row()
                                    .items_start()
                                    .child(
                                        div()
                                            .size(px(18.0))
                                            .rounded_full()
                                            .text_color(rgb(0x29b1ff))
                                            .child(*mark),
                                    )
                                    .child(
                                        column().gap_1().child(*name).child(
                                            div()
                                                .text_size(px(12.0))
                                                .text_color(rgb(0x9fb0cf))
                                                .child(*detail),
                                        ),
                                    )
                            }),
                        )
                        .child(div().flex_1())
                        .child(
                            div()
                                .h(px(4.0))
                                .rounded_full()
                                .bg(rgb(0x1c2a4d))
                                .child(div().w(px(185.0)).h_full().bg(rgb(0x29b1ff))),
                        )
                        .child(
                            div()
                                .text_size(px(10.0))
                                .text_color(rgb(0x9fb0cf))
                                .child("© 2025–2026 Web3 Infrastructure Foundation · MIT"),
                        ),
                ),
        )
        .child(nav(Page::History, cx))
        .into_any_element()
}
