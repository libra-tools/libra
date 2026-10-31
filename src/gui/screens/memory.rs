//! Memory episodes and stale projection display.
use ely_gpui_component::{buttons::Button, theme::Palette};
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, px,
};

use super::super::{
    desktop::{Desktop, Page},
    widgets::{
        action, agent, badge, banner, card, check, column, label, mono, muted, nav, note, row,
        table, title,
    },
};
pub fn render(s: &Desktop, cx: &Context<Desktop>, p: &Palette) -> AnyElement {
    let episodes = [
        (
            "ep-0042",
            "agent session",
            "Kept synchronous checkpoint writes after async trade-off review",
        ),
        (
            "ep-0041",
            "commit",
            "Add token-bucket rate limiter to API gateway",
        ),
        (
            "ep-0040",
            "agent run",
            "Review found shared bucket across API keys",
        ),
        ("ep-0039", "bridge op", "Imported a captured checkpoint"),
    ];
    let e = episodes.get(s.selected).unwrap_or(&episodes[0]);
    column()
        .size_full()
        .gap_0()
        .child(
            banner("Memory projection is stale. 2 commits and 1 agent session landed since the last rebuild.", p)
                .child(check("allow-stale", "Allow stale", true, s, cx))
                .child(action("rebuild", "Rebuild now", false, cx)),
        )
        .child(
            row()
                .items_stretch()
                .gap_0()
                .flex_1()
                .min_h(px(0.0))
                .child(
                    column()
                        .id("episodes")
                        .overflow_y_scroll()
                        .w(px(330.0))
                        .p_4()
                        .child(title("Episodes"))
                        .child(label("ALL SOURCES · IN WINDOW", p))
                        .children(episodes.iter().enumerate().map(|(i, e)| {
                            card(p)
                                .id(("episode", i))
                                .cursor_pointer()
                                .bg(if s.selected == i { p.selection } else { p.surface })
                                .on_click(cx.listener(move |s, _, _, cx| {
                                    s.selected = i;
                                    cx.notify();
                                }))
                                .child(row().child(mono(e.0)).child(muted(e.1, p)))
                                .child(e.2)
                        })),
                )
                .child(
                    column()
                        .id("memory-detail")
                        .overflow_y_scroll()
                        .flex_1()
                        .p_6()
                        .bg(p.surface)
                        .child(
                            row()
                                .child(mono(e.0))
                                .child(badge(e.1, 0xc2410c, 0xfff1e8))
                                .child(muted("outcome: decision recorded", p)),
                        )
                        .child(title(e.2))
                        .child(
                            card(p)
                                .child(row().child(title("Body")).child(
                                    Button::new("reveal-memory", "Reveal full body").on_click(cx.listener(
                                        |s, _, _, cx| {
                                            let v = s.toggles.entry("memory-body").or_default();
                                            *v = !*v;
                                            cx.notify();
                                        },
                                    )),
                                ))
                                .child(note(
                                    if s.toggles.get("memory-body") == Some(&true) {
                                        "The session compared recovery guarantees and kept synchronous checkpoint \
                                         writes. This demonstration episode is derived from the captured discussion."
                                    } else {
                                        "Body elided. Reveal to read the derived text; it is rendered from the source \
                                         facts below and is not stored anywhere else."
                                    },
                                    p,
                                )),
                        )
                        .child(
                            card(p)
                                .child(title("Derived from"))
                                .child(
                                    row()
                                        .child(agent("Claude Code"))
                                        .child("Effect of async checkpoint writes on recovery consistency"),
                                )
                                .child(mono("s-cc-arch"))
                                .child(nav(Page::Sessions, cx))
                                .child(mono("◆ HEAD at capture   9d1e4c7")),
                        )
                        .child(card(p).child(title("Projection status")).child(table(
                            &["Field", "Value"],
                            &[
                                vec!["schema_version", "1"],
                                vec!["selector_version", "1"],
                                vec!["rules_version", "1"],
                                vec!["horizon_truncated", "false"],
                                vec!["revoked_count", "0"],
                                vec!["aged_out_count", "0"],
                                vec!["rebuilt_at", "today 22:16"],
                            ],
                            p,
                        )))
                        .child(note(
                            "Zero-authority · Every episode is a pure function of committed facts. There is nothing \
                             to edit here; to change memory, change the facts and rebuild.",
                            p,
                        )),
                ),
        )
        .into_any_element()
}
