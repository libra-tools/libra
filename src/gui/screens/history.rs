//! History graph and captured creation-history inspector.

use ely_gpui_component::{
    buttons::{Button, ButtonVariant},
    theme::{ActiveTheme, ControlSize, Mode, Palette, Theme},
};
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, img, prelude::FluentBuilder, px, rgb,
};

use super::super::{
    desktop::{Desktop, Page},
    widgets::{
        action, agent, badge, card, code, column, label, mono, muted, nav, note, row, table, tabs,
        title,
    },
};

const COMMITS: [(&str, &str, &str, &str, &str); 9] = [
    (
        "Add token-bucket rate limiter to API gateway",
        "Claude Code",
        "9f2c4e1",
        "12m",
        "main",
    ),
    (
        "Tighten limiter config validation",
        "eli",
        "41be07a",
        "38m",
        "",
    ),
    (
        "feat(search): FTS5 index over checkpoints",
        "Codex",
        "c07d5aa",
        "1h",
        "feat/search",
    ),
    (
        "Refactor request context propagation",
        "eli",
        "2a9e1f3",
        "2h",
        "",
    ),
    (
        "Scaffold search module and CLI flag",
        "Codex",
        "7d13b20",
        "3h",
        "",
    ),
    (
        "Merge branch feat/auth-session",
        "eli",
        "e5f0c88",
        "1d",
        "v0.4.0",
    ),
    (
        "Rotate session cookies on privilege change",
        "OpenCode",
        "0b6a2d4",
        "1d",
        "",
    ),
    ("Bump tokio and axum", "eli", "f31c9e7", "2d", ""),
    (
        "Import from Git repository ./acme",
        "eli",
        "18ad40b",
        "3d",
        "",
    ),
];

pub fn render(s: &Desktop, cx: &Context<Desktop>, p: &Palette) -> AnyElement {
    let dark = cx.theme().is_dark();
    let visible = visible_commits(&s.history_query, s.filter);
    let selected = visible
        .iter()
        .find(|(i, _)| *i == s.selected)
        .or_else(|| visible.first());
    let toolbar = row()
        .px_4()
        .py_2()
        .border_b_1()
        .border_color(p.border)
        .bg(p.surface)
        .child(img("libra/logo.svg").size(px(28.0)))
        .child(div().font_weight(FontWeight::BOLD).child("Libra"))
        .child(action("repository", "▱  acme-libra  ⌄", false, cx))
        .child(mono("⑂ main   ↑1 ↓0"))
        .child(action("fetch", "Fetch", false, cx))
        .child(action("pull", "Pull", false, cx))
        .child(action("push", "Push", false, cx))
        .child(action("undo", "Undo", false, cx))
        .child(
            div().flex_1().child(
                ely_gpui_component::forms::SearchInput::new("history-search", &s.history_input)
                    .size(ControlSize::Sm),
            ),
        )
        .child(badge("Design preview", 0x5b6474, 0xf4f5f7))
        .child(
            Button::new("history-theme", if dark { "Light" } else { "Dark" })
                .size(ControlSize::Sm)
                .variant(ButtonVariant::Ghost)
                .on_click(move |_, _, cx| {
                    Theme::set_mode_now(if dark { Mode::Light } else { Mode::Dark }, cx)
                }),
        );
    let sidebar = column()
        .id("repository-sidebar")
        .overflow_y_scroll()
        .w(px(220.0))
        .flex_none()
        .p_3()
        .bg(p.bg)
        .child(label("WORKSPACE", p))
        .children(
            [
                Page::Lanes,
                Page::History,
                Page::Sessions,
                Page::Review,
                Page::Operations,
                Page::Memory,
                Page::Search,
            ]
            .into_iter()
            .map(|page| nav(page, cx)),
        )
        .child(label("BRANCHES", p))
        .children(
            [
                ("main", 0x2563eb),
                ("feat/search", 0x0f766e),
                ("agent/rate-limit", 0xc2410c),
                ("fix/limiter-config", 0x94a3b8),
            ]
            .into_iter()
            .map(|(branch, color)| {
                row()
                    .py_1()
                    .child(div().size(px(7.0)).rounded_full().bg(rgb(color)))
                    .child(mono(branch))
            }),
        )
        .child(label("AGENTS", p))
        .children(["Claude Code", "Codex", "OpenCode"].iter().map(|name| {
            row()
                .child(agent(name))
                .child(*name)
                .child(div().flex_1())
                .child(muted(
                    if *name == "OpenCode" {
                        "Trust…"
                    } else {
                        "capturing"
                    },
                    p,
                ))
        }))
        .child(note(
            "OpenCode RPC adapter is closed until trusted. Nothing is captured.",
            p,
        ))
        .child(label("TRACE REFS", p))
        .child(mono("♙ refs/trace/*   local only"))
        .child(action("trace-push", "Push trace refs…", false, cx))
        .child(nav(Page::Settings, cx))
        .child(nav(Page::About, cx));
    let filters = row()
        .flex_wrap()
        .px_4()
        .py_3()
        .child(title("History"))
        .children(
            ["All", "Human", "Agent-authored", "Has checkpoint"]
                .into_iter()
                .enumerate()
                .map(|(i, text)| {
                    Button::new(("history-filter", i), text)
                        .size(ControlSize::Sm)
                        .variant(if s.filter == i {
                            ButtonVariant::Primary
                        } else {
                            ButtonVariant::Ghost
                        })
                        .on_click(cx.listener(move |s, _, _, cx| {
                            s.filter = i;
                            cx.notify();
                        }))
                }),
        );
    let list = column()
        .gap_0()
        .flex_1()
        .min_w(px(360.0))
        .bg(p.surface)
        .child(filters)
        .child(
            row()
                .px_4()
                .h(px(32.0))
                .border_y_1()
                .border_color(p.border)
                .child(label("GRAPH", p).w(px(44.0)))
                .child(label("MESSAGE", p).flex_1())
                .child(label("AUTHOR", p).w(px(150.0)))
                .child(label("COMMIT", p).w(px(70.0))),
        )
        .child(
            column()
                .id("commit-list")
                .gap_0()
                .overflow_y_scroll()
                .flex_1()
                .children(visible.iter().copied().map(|(i, c)| {
                    let is_branch = c.1 == "Codex";
                    row()
                        .id(("commit", i))
                        .gap_2()
                        .h(px(48.0))
                        .px_3()
                        .flex_none()
                        .border_b_1()
                        .border_color(p.border)
                        .bg(if selected.is_some_and(|(index, _)| *index == i) {
                            p.selection
                        } else {
                            p.surface
                        })
                        .cursor_pointer()
                        .hover(|v| v.bg(p.hover))
                        .on_click(cx.listener(move |s, _, _, cx| {
                            s.selected = i;
                            cx.notify();
                        }))
                        .child(
                            div()
                                .w(px(44.0))
                                .h_full()
                                .relative()
                                .child(
                                    div()
                                        .absolute()
                                        .left(px(10.0))
                                        .w(px(2.0))
                                        .h_full()
                                        .bg(p.accent),
                                )
                                .when(i < 5, |d| {
                                    d.child(
                                        div()
                                            .absolute()
                                            .left(px(30.0))
                                            .w(px(2.0))
                                            .h_full()
                                            .bg(rgb(0x0f766e)),
                                    )
                                })
                                .child(
                                    div()
                                        .absolute()
                                        .left(px(if is_branch { 26.0 } else { 6.0 }))
                                        .top(px(19.0))
                                        .size(px(10.0))
                                        .rounded_full()
                                        .bg(if is_branch {
                                            rgb(0x0f766e).into()
                                        } else {
                                            p.accent
                                        }),
                                ),
                        )
                        .child(
                            column()
                                .flex_1()
                                .gap_1()
                                .overflow_hidden()
                                .when(!c.4.is_empty(), |d| {
                                    d.child(muted(c.4, p).text_color(p.accent))
                                })
                                .child(div().truncate().child(c.0)),
                        )
                        .child(
                            row()
                                .w(px(150.0))
                                .flex_none()
                                .child(agent(c.1))
                                .child(div().truncate().child(c.1)),
                        )
                        .child(
                            column()
                                .gap_1()
                                .w(px(70.0))
                                .flex_none()
                                .child(mono(c.2))
                                .child(muted(c.3, p)),
                        )
                })),
        );
    let c = selected
        .map(|(_, commit)| *commit)
        .unwrap_or(&("No matching commits", "", "", "", ""));
    let inspector = column()
        .w(px(365.0))
        .flex_none()
        .h_full()
        .gap_0()
        .border_l_1()
        .border_color(p.border)
        .bg(p.surface)
        .child(
            column()
                .p_4()
                .border_b_1()
                .border_color(p.border)
                .when(selected.is_some(), |d| {
                    d.child(muted(format!("{} · {} · {} ago", c.2, c.1, c.3), p))
                })
                .child(
                    div()
                        .text_size(px(16.0))
                        .font_weight(FontWeight::BOLD)
                        .child(c.0),
                )
                .when(selected.is_some(), |d| {
                    d.child(tabs(&["Creation history", "Changes · 3", "Raw"], s.tab, cx))
                }),
        )
        .child(
            column()
                .id("creation-inspector")
                .overflow_y_scroll()
                .flex_1()
                .p_4()
                .children(if selected.is_some() {
                    inspector_body(s, cx, p, c)
                } else {
                    vec![
                        muted("Clear the search or choose another author filter.", p)
                            .into_any_element(),
                    ]
                }),
        );
    column()
        .gap_0()
        .size_full()
        .child(toolbar)
        .child(
            row()
                .gap_0()
                .items_stretch()
                .flex_1()
                .min_h(px(0.0))
                .child(sidebar)
                .child(list)
                .child(inspector),
        )
        .into_any_element()
}

fn inspector_body(
    s: &Desktop,
    cx: &Context<Desktop>,
    p: &Palette,
    c: &(&str, &str, &str, &str, &str),
) -> Vec<AnyElement> {
    if s.tab == 1 {
        return vec![
            table(
                &["File", "Change"],
                &[
                    vec!["src/gateway/limiter.rs", "+142"],
                    vec!["src/gateway/mod.rs", "+9 −2"],
                    vec!["config/gateway.toml", "+6"],
                ],
                p,
            )
            .into_any_element(),
        ];
    }
    if s.tab == 2 {
        return vec![
            code(format!(
                "commit {}\nauthor {}\n\n{}\n\nDemonstration record",
                c.2, c.1, c.0
            ))
            .into_any_element(),
        ];
    }
    if c.1 == "eli" {
        return vec![
            title("No agent capture for this commit").into_any_element(),
            muted(
                "Authored by hand. There is no prompt, session or checkpoint to show.",
                p,
            )
            .into_any_element(),
        ];
    }
    vec![
        row()
            .flex_wrap()
            .child(badge("Prompt", 0xc2410c, 0xfff1e8))
            .child("→")
            .child(badge("Session", 0xc2410c, 0xfff1e8))
            .child("→")
            .child(badge("Checkpoint", 0x334155, 0xf1f5f9))
            .child("→")
            .child(badge("Commit", 0xffffff, 0x111827))
            .into_any_element(),
        label("PROMPT", p).into_any_element(),
        note(
            if c.1 == "Claude Code" {
                "Add a token-bucket rate limiter to the gateway. 100 requests per minute per API key, configurable in \
                 gateway.toml. Keep the existing middleware order."
            } else {
                "Captured demonstration prompt associated with this agent's commit."
            },
            p,
        )
        .into_any_element(),
        label("SESSION", p).into_any_element(),
        row()
            .child(agent(c.1))
            .child(
                column()
                    .gap_1()
                    .child(c.1.to_string())
                    .child(muted("ses_7f3a91 · 14 turns · trusted adapter", p)),
            )
            .into_any_element(),
        label("TOOL EVENTS", p).into_any_element(),
        table(
            &["Tool", "Source", "Result"],
            &[
                vec!["READ", "gateway/mod.rs", ""],
                vec!["EDIT", "gateway/limiter.rs", "+142"],
                vec!["EDIT", "gateway.toml", "+6"],
                vec!["BASH", "cargo test", "exit 0"],
            ],
            p,
        )
        .into_any_element(),
        card(p)
            .child(mono("ckpt_0192"))
            .child(muted("3 files captured before commit", p))
            .child(action("export", "Export redacted…", true, cx))
            .into_any_element(),
        muted(
            "Shown only from captured source evidence. Libra does not infer plans, test results or approvals.",
            p,
        )
        .into_any_element(),
    ]
}

type Commit = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
);

fn visible_commits(query: &str, filter: usize) -> Vec<(usize, &'static Commit)> {
    let query = query.trim().to_lowercase();
    COMMITS
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            (c.0.to_lowercase().contains(&query)
                || c.1.to_lowercase().contains(&query)
                || c.2.contains(&query))
                && match filter {
                    1 => c.1 == "eli",
                    2 | 3 => c.1 != "eli",
                    _ => true,
                }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::visible_commits;

    #[test]
    fn filters_intersect_with_case_insensitive_search() {
        assert_eq!(visible_commits("  CODEX  ", 2).len(), 2);
        assert!(visible_commits("codex", 1).is_empty());
        assert_eq!(visible_commits("C07D5AA", 0)[0].0, 2);
        assert_eq!(visible_commits("", 1).len(), 5);
        assert!(visible_commits("no matching record", 0).is_empty());
    }
}
