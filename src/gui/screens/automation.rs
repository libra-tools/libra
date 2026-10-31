//! Automation rule source and dry-run controls.
use ely_gpui_component::theme::Palette;
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, px,
};

use super::super::{
    desktop::Desktop,
    widgets::{
        action, badge, card, check, code, column, field, mono, muted, note, row, table, title,
    },
};
pub fn render(s: &Desktop, cx: &Context<Desktop>, p: &Palette) -> AnyElement {
    let rules = [
        (
            "nightly-review",
            "cron",
            "shell",
            "Review the latest commit overnight",
        ),
        (
            "after-commit",
            "vcs",
            "shell",
            "Record post-commit evidence",
        ),
        (
            "checkpoint-webhook",
            "hook",
            "webhook",
            "Notify when capture completes",
        ),
        ("daily-summary", "cron", "prompt", "Summarize captured work"),
        ("after-push", "vcs", "shell", "Inspect pushed refs"),
    ];
    let r = rules.get(s.selected).unwrap_or(&rules[0]);
    row()
        .size_full()
        .items_stretch()
        .gap_0()
        .child(
            column()
                .id("automation-rules")
                .overflow_y_scroll()
                .w(px(330.0))
                .flex_none()
                .p_5()
                .child(row().child(title("Rules")).child(badge("5 valid", 0x166534, 0xdcfce7)))
                .children(rules.iter().enumerate().map(|(i, r)| {
                    card(p)
                        .id(("rule", i))
                        .bg(if s.selected == i { p.selection } else { p.surface })
                        .cursor_pointer()
                        .on_click(cx.listener(move |s, _, _, cx| {
                            s.selected = i;
                            cx.notify();
                        }))
                        .child(mono(r.0))
                        .child(row().child(badge(r.1, 0x1d4ed8, 0xeaf1ff)).child(muted(r.2, p)))
                        .child(muted(r.3, p))
                }))
                .child(note(
                    "Rules come from automations.toml. Linked worktrees read the same file; a same-name overlay wins. \
                     A damaged worktree scope runs nothing.",
                    p,
                )),
        )
        .child(
            column()
                .id("automation-detail")
                .overflow_y_scroll()
                .flex_1()
                .p_6()
                .bg(p.surface)
                .child(title(r.0))
                .child(muted("cron · next due 02:30", p))
                .child(code(format!(
                    "[[rules]]\nid = \"{}\"\ntrigger = {{ kind = \"cron\", schedule = \"30 2 * * *\" }}\naction = {{ \
                     kind = \"shell\",\n  command = \"libra review --agent claude-code --agent codex\",\n  timeout_ms \
                     = 900000 }}",
                    r.0
                )))
                .child(muted(
                    "Triggers: cron · vcs (post_add, post_branch, post_commit, post_push, post_switch) · hook (agent \
                     events). Actions: shell · prompt · webhook.",
                    p,
                ))
                .child(
                    card(p)
                        .child(title("Run this rule"))
                        .child(check("automation-live", "Live (off = dry run)", false, s, cx))
                        .child(note(
                            "Dry run plans and records shell actions without spawning anything. Live runs shell \
                             actions that pass safety preflight.",
                            p,
                        ))
                        .child(field("EVALUATE AS IF IT WERE", "2026-11-01 02:30", p))
                        .child(muted("Simulated time, to check which rules would be due.", p))
                        .child(code(format!("libra automation run --rule {}", r.0)))
                        .child(
                            row()
                                .child(action("run-due", "Run all due rules", false, cx))
                                .child(action("dry-run-rule", "Dry-run rule", true, cx)),
                        ),
                )
                .child(card(p).child(title("History · last 20")).child(table(
                    &["At", "Rule", "Status", "Result"],
                    &[
                        vec!["02:30", "nightly-review", "planned", "Dry run · shell action recorded"],
                        vec!["22:49", "after-commit", "success", "Command completed"],
                        vec!["21:12", "checkpoint-webhook", "skipped", "Not due"],
                    ],
                    p,
                ))),
        )
        .into_any_element()
}
