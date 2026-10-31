//! Interactive rebase todo and projected result.
use ely_gpui_component::{
    buttons::Button,
    theme::{ControlSize, Palette},
};
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, px,
};

use super::super::{
    desktop::{Desktop, Page},
    widgets::{
        action, agent, badge, card, check, code, column, field, mono, muted, nav, note, row, title,
    },
};
pub fn render(s: &Desktop, cx: &Context<Desktop>, p: &Palette) -> AnyElement {
    let commits = [
        ("41be07a", "Tighten limiter config validation", "You"),
        ("8e19d40", "fixup! Tighten limiter config validation", "You"),
        ("0e8c23a", "Add missing boundary test", "Codex"),
        ("9f2c4e1", "Add per-route cost override", "Claude Code"),
        ("b5ca1a0", "Document limiter config", "Claude Code"),
        ("af0e218", "Debug logging", "You"),
    ];
    column()
        .size_full()
        .gap_0()
        .child(
            row()
                .px_5()
                .py_3()
                .bg(p.surface)
                .child(title("Interactive rebase"))
                .child(mono("fix/limiter-config onto main · 6 commits"))
                .child(div().flex_1())
                .child(action("rebase-cancel", "Cancel", false, cx))
                .child(action("rebase-start", "Start rebase", true, cx)),
        )
        .child(
            row()
                .id("rebase-body")
                .overflow_y_scroll()
                .items_start()
                .flex_1()
                .p_6()
                .gap_5()
                .child(
                    column()
                        .flex_1()
                        .child(muted(
                            "Oldest first. Drag to reorder. Fixup commits are already placed by autosquash.",
                            p,
                        ))
                        .children(commits.iter().enumerate().map(|(i, c)| {
                            let modes = ["pick", "reword", "edit", "squash", "fixup", "drop"];
                            let mode = s.rebase_actions.get(i).copied().unwrap_or(0);
                            card(p).child(
                                row()
                                    .child(muted("⋮⋮", p))
                                    .child(
                                        Button::new(("rebase-action", i), modes[mode % 6])
                                            .size(ControlSize::Sm)
                                            .on_click(cx.listener(move |s, _, _, cx| {
                                                if let Some(a) = s.rebase_actions.get_mut(i) {
                                                    *a = (*a + 1) % 6;
                                                }
                                                cx.notify();
                                            })),
                                    )
                                    .child(mono(c.0))
                                    .child(div().flex_1().child(c.1))
                                    .child(agent(c.2)),
                            )
                        }))
                        .child(field("EXEC · AFTER EACH COMMIT", "cargo test -p gateway", p)),
                )
                .child(
                    column()
                        .w(px(340.0))
                        .flex_none()
                        .child(
                            card(p)
                                .child(title("Options"))
                                .child(check("autosquash", "Autosquash fixup! commits", true, s, cx))
                                .child(check("update-refs", "Update stacked branch refs", true, s, cx))
                                .child(check("autostash", "Autostash local changes", true, s, cx))
                                .child(check(
                                    "reuse-resolution",
                                    "Reuse recorded conflict resolutions",
                                    true,
                                    s,
                                    cx,
                                ))
                                .child(check("rebase-signoff", "Sign off commits", false, s, cx)),
                        )
                        .child(
                            card(p)
                                .child(title("Result"))
                                .children(commits.iter().enumerate().map(|(i, c)| {
                                    row().child(div().flex_1().child(c.1)).child(badge(
                                        match s.rebase_actions[i] {
                                            1 => "reworded",
                                            3 => "squashed",
                                            4 => "fixup",
                                            5 => "dropped",
                                            _ => "kept",
                                        },
                                        0x334155,
                                        0xf1f5f9,
                                    ))
                                }))
                                .child(muted("Stacked branch fix/limiter-docs moves with it.", p)),
                        )
                        .child(note(
                            "Provenance follows the commits. Rewritten commits keep their links to agent sessions and \
                             checkpoints. The rebase is one entry in the operation log.",
                            p,
                        ))
                        .child(nav(Page::History, cx))
                        .child(code("libra rebase -i --autosquash --update-refs --autostash main")),
                ),
        )
        .into_any_element()
}
