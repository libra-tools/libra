//! Three-way conflict layout and provenance.
use ely_gpui_component::theme::Palette;
use gpui::{AnyElement, Context, IntoElement, ParentElement, div, prelude::*, px};

use super::super::{
    desktop::{Desktop, Page},
    widgets::{
        action, agent, badge, card, check, code, column, label, mono, muted, nav, note, row, title,
    },
};
pub fn render(s: &Desktop, cx: &Context<Desktop>, p: &Palette) -> AnyElement {
    column().size_full().gap_0()
        .child(row().px_5().py_3().bg(p.surface).child(title("Merging")).child(mono("agent/rate-limit → main")).child(badge("2 conflicts in 1 file · 1 resolved",0x92400e,0xfffbeb)).child(div().flex_1()).child(action("abort-merge","Abort merge",false,cx)).child(action("commit-merge","Commit merge",true,cx)))
        .child(row().items_stretch().gap_0().flex_1().min_h(px(0.0))
            .child(column().w(px(245.0)).flex_none().p_4().child(label("CONFLICTS",p)).child(card(p).bg(p.selection).child(mono("src/gateway/limiter.rs")).child(muted("1 / 2",p)))
                .child(mono("config/gateway.toml")).child(badge("resolved",0x166534,0xdcfce7)).child(note("Recorded resolution reused\n\ngateway.toml matched a conflict you resolved before and was applied automatically.",p)))
            .child(column().id("conflict-panes").overflow_y_scroll().flex_1().p_5().bg(p.surface)
                .child(title("Conflict 2 of 2 · try_acquire()"))
                .child(row().items_stretch().gap_4()
                    .child(card(p).flex_1().child(label("OURS · MAIN",p)).child(row().child(agent("You")).child("You · 41be07a"))
                        .child(muted("Commit · Tighten limiter config validation",p)).child(code("pub fn try_acquire(&mut self) -> bool {\n    self.refill();\n    if self.tokens < 1.0 { return false; }\n    self.tokens -= 1.0;\n    true\n}"))
                        .child(action("take-ours","Take ours",false,cx)))
                    .child(card(p).flex_1().child(label("THEIRS · AGENT/RATE-LIMIT",p)).child(row().child(agent("Claude Code")).child("Claude Code · ses_7f3a91"))
                        .child(muted("Prompt · Add a per-route override to the rate limiter…",p)).child(code("pub fn try_acquire(&mut self) -> bool {\n    self.refill();\n    let cost = self.route_cost.unwrap_or(1.0);\n    if self.tokens >= cost {\n        self.tokens -= cost; true\n    } else { false }\n}"))
                        .child(row().child(action("take-theirs","Take theirs",false,cx)).child(nav(Page::Sessions,cx)))))
                .child(card(p).child(row().child(label("RESULT",p)).child(muted("Edit directly, or combine:",p)).child(action("ours-first","Ours, then theirs",false,cx)).child(action("theirs-first","Theirs, then ours",false,cx)))
                    .child(code("pub fn try_acquire(&mut self) -> bool {\n    self.refill();\n    let cost = self.route_cost.unwrap_or(1.0);\n    if self.tokens < cost { return false; }\n    self.tokens -= cost;\n    true\n}"))
                    .child(check("remember-conflict","Remember this resolution",true,s,cx)).child(check("keep-provenance","Keep both sides' provenance on the merge commit",true,s,cx)).child(action("resolve","Mark resolved",true,cx))))).into_any_element()
}
