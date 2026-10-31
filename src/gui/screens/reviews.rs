//! Multi-reviewer runs, findings, and launch form.
use ely_gpui_component::theme::Palette;
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, px,
};

use super::super::{
    desktop::Desktop,
    widgets::{
        action, agent, badge, card, check, code, column, field, label, mono, muted, note, row,
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
                .id("review-runs")
                .overflow_y_scroll()
                .w(px(245.0))
                .flex_none()
                .p_4()
                .child(title("Runs"))
                .children(
                    [
                        ("rv-7c2e9a", "partial", "HEAD~1..HEAD", "3 reviewers · 12m ago"),
                        ("rv-6b1d82", "success", "feat/search", "2 reviewers · 2h ago"),
                        ("rv-4a8f31", "failed", "ckpt_0191", "1 reviewer · yesterday"),
                    ]
                    .iter()
                    .enumerate()
                    .map(|(i, r)| {
                        card(p)
                            .id(("review-run", i))
                            .cursor_pointer()
                            .bg(if s.selected == i { p.selection } else { p.surface })
                            .on_click(cx.listener(move |s, _, _, cx| {
                                s.selected = i;
                                cx.notify();
                            }))
                            .child(mono(r.0))
                            .child(badge(r.1, 0x92400e, 0xfef3c7))
                            .child(r.2)
                            .child(muted(r.3, p))
                    }),
                )
                .child(action("clean-runs", "Clean terminal runs…", false, cx)),
        )
        .child(
            column()
                .id("review-findings")
                .overflow_y_scroll()
                .flex_1()
                .p_5()
                .bg(p.surface)
                .child(
                    row()
                        .child(title(
                            ["rv-7c2e9a", "rv-6b1d82", "rv-4a8f31"]
                                .get(s.selected)
                                .copied()
                                .unwrap_or("rv-7c2e9a"),
                        ))
                        .child(badge("partial", 0x92400e, 0xfef3c7)),
                )
                .child(muted("2 of 3 reviewers finished", p))
                .child(field("SCOPE", "HEAD~1..HEAD", p))
                .child(muted(
                    "Workspace · isolated mirror, ignore rules applied\nOutput · redacted, 64 KiB per stream",
                    p,
                ))
                .child(
                    row().items_stretch().children(
                        [
                            ("Claude Code", "success", "3 findings · 1m 48s"),
                            ("Codex", "success", "2 findings · 2m 05s"),
                            ("OpenCode", "timeout", "no findings · stderr kept"),
                        ]
                        .iter()
                        .map(|(name, state, detail)| {
                            card(p)
                                .flex_1()
                                .child(agent(name))
                                .child(*name)
                                .child(badge(
                                    *state,
                                    if *state == "timeout" { 0x92400e } else { 0x166534 },
                                    if *state == "timeout" { 0xfef3c7 } else { 0xdcfce7 },
                                ))
                                .child(muted(*detail, p))
                        }),
                    ),
                )
                .child(
                    card(p)
                        .child(title("findings.md · claude-code"))
                        .child(muted("untrusted text · control sequences stripped", p))
                        .child(badge("High", 0xb91c1c, 0xfee2e2))
                        .child(title("Bucket is shared across all API keys"))
                        .child("The limiter is constructed once in mod.rs, so every key draws from the same bucket.")
                        .child(mono("src/gateway/mod.rs:88"))
                        .child(badge("Medium", 0x92400e, 0xfef3c7))
                        .child(title("Refill uses wall-clock time"))
                        .child("Prefer a monotonic clock to avoid bursts after clock adjustments.")
                        .child(mono("src/gateway/limiter.rs:31"))
                        .child(badge("Low", 0x334155, 0xf1f5f9))
                        .child("Missing doc comment on config field")
                        .child(mono("config/gateway.toml:14")),
                )
                .child(action("attach-review", "Attach file…", false, cx)),
        )
        .child(
            column()
                .id("new-review")
                .overflow_y_scroll()
                .w(px(295.0))
                .flex_none()
                .p_5()
                .border_l_1()
                .border_color(p.border)
                .child(title("New review run"))
                .child(label("REVIEWERS · RUN IN PARALLEL", p))
                .child(check("review-claude", "claude-code", true, s, cx))
                .child(check("review-codex", "codex", true, s, cx))
                .child(check("review-opencode", "opencode", false, s, cx))
                .child(muted("Other agents can't launch reviews yet.", p))
                .child(field("SCOPE", "Last commit · HEAD~1..HEAD", p))
                .child(field("CHECKPOINT", "ckpt_0192", p))
                .child(note(
                    "A checkpoint scope reviews only the captured transcript, read-only. A missing checkpoint fails \
                     before any run is created.",
                    p,
                ))
                .child(code("libra review --agent claude-code --agent codex"))
                .child(div().flex_1())
                .child(action("start-review", "Start review", true, cx)),
        )
        .into_any_element()
}
