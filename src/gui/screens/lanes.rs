//! Applied branches and agent lane cards from Lanes.dc.html.
use ely_gpui_component::{
    forms::{Choice, ChoiceChips, Input},
    theme::Palette,
};
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px, rgb,
};

use super::super::{
    desktop::{Desktop, Page},
    widgets::{
        action, agent, badge, card, check, code, column, field, label, mono, muted, nav, note, row,
        title,
    },
};

pub fn render(s: &Desktop, cx: &Context<Desktop>, p: &Palette) -> AnyElement {
    let human = card(p)
        .w(px(300.0))
        .flex_none()
        .child(
            row()
                .child(agent("You"))
                .child(
                    column()
                        .gap_1()
                        .child("You")
                        .child(mono("fix/limiter-config")),
                )
                .child(badge("editing", 0x334155, 0xf1f5f9)),
        )
        .child(note("Stacked on agent/rate-limit", p))
        .child(label("UNCOMMITTED · 2", p))
        .child(file("config/gateway.toml", "+3", p))
        .child(file("src/gateway/limiter.rs", "+11 −4", p))
        .child(field(
            "COMMIT MESSAGE",
            "Reject zero or negative burst size",
            p,
        ))
        .child(action(
            "lane-commit",
            "Commit to fix/limiter-config",
            true,
            cx,
        ))
        .child(label("COMMITS · 1", p))
        .child(muted("● Tighten limiter config validation · 41be07a", p));
    let claude = card(p)
        .w(px(320.0))
        .flex_none()
        .border_color(rgb(0xfdba8c))
        .child(
            row().child(agent("Claude Code")).child(
                column()
                    .gap_1()
                    .child("Claude Code")
                    .child(mono("agent/rate-limit")),
            ),
        )
        .child(badge("● running · turn 9", 0x166534, 0xdcfce7))
        .child(note(
            "Prompt  Add a per-route override to the rate limiter and document it in gateway.toml.",
            p,
        ))
        .child(label("BEING WRITTEN · 3", p))
        .child(file("src/gateway/limiter.rs", "+38", p))
        .child(file("src/gateway/routes.rs", "+12", p))
        .child(file("config/gateway.toml", "queued", p))
        .child(code(
            "Latest tool event\nbash · cargo test -p gateway limiter",
        ))
        .child(row().child(nav(Page::Sessions, cx)).child(action(
            "checkpoint",
            "Checkpoint now",
            false,
            cx,
        )))
        .child(label("COMMITS · 1", p))
        .child(muted("● Add token-bucket rate limiter… · 9f2c4e1", p));
    let codex = card(p)
        .w(px(300.0))
        .flex_none()
        .child(
            row()
                .child(agent("Codex"))
                .child(column().gap_1().child("Codex").child(mono("feat/search"))),
        )
        .child(badge("stopped · ready", 0x92400e, 0xfef3c7))
        .child(note(
            "Prompt  Let users search across captured checkpoints by file path and prompt text.",
            p,
        ))
        .child(label("UNCOMMITTED · 0", p))
        .child(muted("Clean. Session stopped after 2 commits.", p))
        .child(
            row()
                .child(action("review-lane", "Review lane", true, cx))
                .child(action("unapply", "Unapply", false, cx)),
        )
        .child(label("COMMITS · 2", p))
        .child(muted("● FTS5 index over checkpoints · c07d5aa", p))
        .child(muted("● Scaffold search module… · 7d13b20", p));
    let selected = if s.toggles.get("new-lane-codex") == Some(&true) {
        "codex"
    } else {
        "claude"
    };
    let entity = cx.entity();
    let new = card(p)
        .w(px(290.0))
        .flex_none()
        .border_color(p.accent)
        .child(title("Start an agent lane"))
        .child(label("AGENT", p))
        .child(
            ChoiceChips::new(
                "new-lane-agent",
                vec![
                    Choice::new("claude", "Claude Code"),
                    Choice::new("codex", "Codex"),
                ],
            )
            .selected([SharedString::from(selected)])
            .on_change(move |values, _, cx| {
                entity.update(cx, |s, cx| {
                    s.toggles.insert(
                        "new-lane-codex",
                        values.first().is_some_and(|v| v == "codex"),
                    );
                    cx.notify();
                });
            }),
        )
        .child(muted("OpenCode — trust adapter first", p))
        .child(label("PROMPT", p))
        .child(Input::new(&s.draft))
        .child(check(
            "lane-capture",
            "Capture session and checkpoints",
            true,
            s,
            cx,
        ))
        .child(action("create-lane", "Create lane and launch", true, cx));
    column()
        .size_full()
        .gap_0()
        .child(
            row()
                .px_5()
                .py_3()
                .bg(p.surface)
                .border_b_1()
                .border_color(p.border)
                .child(muted("Target", p))
                .child(mono("origin/main"))
                .child(div().flex_1())
                .child(action("integrate", "Integrate upstream", false, cx))
                .child(action("new-lane", "+ New lane", true, cx)),
        )
        .child(
            column()
                .p_5()
                .child(title("Working copy · 3 lanes applied"))
                .child(muted(
                    "Each lane is a branch applied at the same time. Agents write into their own lane; you review, \
                     move hunks, and commit.",
                    p,
                )),
        )
        .child(
            row()
                .id("lane-board")
                .items_start()
                .overflow_x_scroll()
                .overflow_y_scroll()
                .flex_1()
                .px_5()
                .pb_5()
                .gap_4()
                .child(
                    card(p)
                        .w(px(190.0))
                        .flex_none()
                        .child(title("Unassigned changes"))
                        .child(muted("Drag a file or hunk into a lane.", p))
                        .child(mono("M  README.md"))
                        .child(mono("M  Cargo.lock")),
                )
                .child(human)
                .child(claude)
                .child(codex)
                .child(new),
        )
        .into_any_element()
}
fn file(path: &'static str, delta: &'static str, p: &Palette) -> impl IntoElement {
    row()
        .py_2()
        .child(mono(path).flex_1())
        .child(muted(delta, p).text_color(p.success))
}
