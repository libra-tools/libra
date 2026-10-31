//! Per-line authorship and file history.
use ely_gpui_component::theme::Palette;
use gpui::{AnyElement, Context, IntoElement, ParentElement, div, prelude::*, px};

use super::super::{
    desktop::{Desktop, Page},
    widgets::{agent, badge, card, column, mono, muted, nav, row, tabs, title},
};
pub fn render(s: &Desktop, cx: &Context<Desktop>, p: &Palette) -> AnyElement {
    let lines = [
        "use std::time::Instant;",
        "pub struct TokenBucket {",
        "    capacity: f64,",
        "    tokens: f64,",
        "    refill_rate: f64,",
        "    last_refill: Instant,",
        "}",
        "",
        "impl TokenBucket {",
        "    pub fn new(capacity: f64) -> Self {",
        "        let capacity = capacity.max(1.0);",
        "        Self { capacity, tokens: capacity,",
        "            refill_rate: capacity / 60.0,",
        "            last_refill: Instant::now() }",
        "    }",
        "    pub fn try_acquire(&mut self) -> bool {",
        "        self.refill();",
        "        if self.tokens < 1.0 { return false; }",
        "        self.tokens -= 1.0;",
        "        true",
        "    }",
        "}",
    ];
    column()
        .size_full()
        .gap_0()
        .child(
            row()
                .px_5()
                .py_3()
                .bg(p.surface)
                .child(title("src/gateway/limiter.rs"))
                .child(muted("at main · 9f2c4e1", p))
                .child(div().flex_1())
                .child(tabs(&["Color by author", "Color by age"], s.tab, cx)),
        )
        .child(
            row()
                .items_stretch()
                .gap_0()
                .flex_1()
                .min_h(px(0.0))
                .child(
                    column()
                        .id("blame-lines")
                        .overflow_y_scroll()
                        .flex_1()
                        .gap_0()
                        .bg(p.surface)
                        .children(lines.iter().enumerate().map(|(i, line)| {
                            let human = [10, 17, 18, 19].contains(&i);
                            row()
                                .h(px(29.0))
                                .flex_none()
                                .gap_2()
                                .px_3()
                                .bg(if human { p.bg } else { p.surface })
                                .child(badge(
                                    if human { "E" } else { "CC" },
                                    if s.tab == 1 {
                                        0x1d4ed8
                                    } else if human {
                                        0x334155
                                    } else {
                                        0xc2410c
                                    },
                                    if s.tab == 1 {
                                        0xeaf1ff
                                    } else if human {
                                        0xe2e8f0
                                    } else {
                                        0xfff1e8
                                    },
                                ))
                                .child(mono(if human { "41be07a" } else { "9f2c4e1" }).w(px(70.0)))
                                .child(
                                    muted(
                                        if human {
                                            "Tighten config"
                                        } else {
                                            "Add token-bucket limiter"
                                        },
                                        p,
                                    )
                                    .w(px(160.0)),
                                )
                                .child(muted(format!("{:>2}", i + 1), p).w(px(28.0)))
                                .child(mono(*line))
                        })),
                )
                .child(
                    column()
                        .id("blame-history")
                        .overflow_y_scroll()
                        .w(px(320.0))
                        .flex_none()
                        .p_5()
                        .border_l_1()
                        .border_color(p.border)
                        .child(
                            card(p)
                                .child(title("Lines 11, 18–20"))
                                .child(row().child(agent("You")).child("You · 41be07a"))
                                .child("Tighten limiter config validation")
                                .child(muted(
                                    "Hand-authored. No agent capture for this commit.",
                                    p,
                                )),
                        )
                        .child(
                            card(p)
                                .child(title("Lines 2–10, 12–17, 21"))
                                .child(
                                    row()
                                        .child(agent("Claude Code"))
                                        .child("Claude Code · 9f2c4e1"),
                                )
                                .child("“Add a token-bucket rate limiter to the gateway…”")
                                .child(nav(Page::Sessions, cx)),
                        )
                        .child(
                            card(p)
                                .child(title("File history"))
                                .child(mono("5c0d9e2  Reject zero burst size"))
                                .child(muted("you · 10m", p))
                                .child(mono("41be07a  Tighten config validation"))
                                .child(muted("you · 38m", p))
                                .child(mono("9f2c4e1  Add token-bucket limiter"))
                                .child(muted("CC · 1h", p)),
                        ),
                ),
        )
        .into_any_element()
}
