//! About, build diagnostics and license tabs.
use ely_gpui_component::theme::Palette;
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, img, prelude::FluentBuilder, px,
};

use super::super::{
    desktop::Desktop,
    widgets::{action, badge, card, column, mono, muted, row, table, tabs, title},
};
pub fn render(s: &Desktop, cx: &Context<Desktop>, p: &Palette) -> AnyElement {
    column()
        .id("about-page")
        .size_full()
        .overflow_y_scroll()
        .items_center()
        .p_6()
        .child(
            card(p)
                .w(px(600.0))
                .p_7()
                .gap_5()
                .child(
                    row().child(img("libra/logo.svg").size(px(80.0))).child(
                        column()
                            .gap_2()
                            .child(title("Libra").text_size(px(32.0)))
                            .child(muted("AI agent-native, Git-compatible version control", p))
                            .child(mono(env!("CARGO_PKG_VERSION"))),
                    ),
                )
                .child(tabs(&["About", "Diagnostics", "Licenses"], s.tab, cx))
                .when(s.tab == 0, |d| {
                    d.child(badge(
                        "Interface preview · update status not checked",
                        0x334155,
                        0xf1f5f9,
                    ))
                    .child(action("about-update", "Check for updates", false, cx))
                    .child(
                        "Libra keeps the creation history behind your code: the prompts, agent sessions, tool events \
                         and checkpoints that led to each commit, stored locally in an open format and pushed only \
                         when you choose.",
                    )
                    .child(table(
                        &["Resource", "Location"],
                        &[
                            vec!["Website", "libra.tools"],
                            vec!["Documentation", "libra.tools/docs"],
                            vec!["Release notes", "GitHub · libra-tools/libra"],
                            vec!["Report an issue", "GitHub · libra-tools/libra/issues"],
                            vec!["Community", "Discord"],
                            vec!["Updates", "@git_mono_AI"],
                        ],
                        p,
                    ))
                })
                .when(s.tab == 1, |d| {
                    d.child(table(
                        &["Field", "Value"],
                        &[
                            vec!["version", env!("CARGO_PKG_VERSION")],
                            vec!["desktop window", "always built"],
                            vec!["UI toolkit", "gpui-pre 0.3.10 · Ely dd727f4"],
                            vec!["platform", std::env::consts::OS],
                            vec!["architecture", std::env::consts::ARCH],
                            vec!["data source", "bundled demonstration records"],
                            vec!["repository / agents / cloud", "not connected"],
                        ],
                        p,
                    ))
                    .child(muted(
                        "Paths, hostnames, tokens and session content are never included.",
                        p,
                    ))
                    .child(action("diagnostics-copy", "Copy diagnostics", false, cx))
                })
                .when(s.tab == 2, |d| {
                    d.child(title("Libra is MIT licensed"))
                        .child("Copyright © 2025–2026 Web3 Infrastructure Foundation")
                        .child(table(
                            &["Built with", "Version", "License"],
                            &[
                                vec!["GPUI", "gpui-pre 0.3.10", "Apache-2.0"],
                                vec!["Ely GPUI Components", "dd727f4", "MIT"],
                                vec!["git-internal", "0.10.4", "MIT"],
                            ],
                            p,
                        ))
                        .child(action("notices", "All third-party notices…", false, cx))
                })
                .child(
                    div()
                        .border_t_1()
                        .border_color(p.border)
                        .pt_4()
                        .child(muted("Made by the web3infra-foundation community", p)),
                )
                .child(mono("libra --version")),
        )
        .into_any_element()
}
