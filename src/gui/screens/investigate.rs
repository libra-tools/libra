//! Sequential investigation transcript and findings sidebar.
use ely_gpui_component::theme::Palette;
use gpui::{AnyElement, Context, IntoElement, ParentElement, prelude::*, px};

use super::super::{
    desktop::Desktop,
    widgets::{
        action, agent, badge, card, code, column, field, label, mono, muted, note, row, title,
    },
};
pub fn render(_: &Desktop, cx: &Context<Desktop>, p: &Palette) -> AnyElement {
    row().size_full().items_stretch().gap_0()
        .child(column().id("investigation").overflow_y_scroll().flex_1().p_6().bg(p.surface).gap_5()
            .child(row().child(mono("iv-2f81c0")).child(badge("paused",0x92400e,0xfef3c7)).child(muted("turn 4 of 6 · round 2",p)))
            .child(title("Why do p99 latencies spike after the limiter was added?"))
            .child(row().child(label("ORDER",p)).child(agent("Claude Code")).child("claude-code →").child(agent("Codex")).child("codex →").child(agent("OpenCode")).child("opencode"))
            .child(note("One investigator at a time, never in parallel · quorum 2 · max 6 turns",p))
            .children([("Claude Code","Turn 1 · claude-code · round 1","The global mutex around the bucket serialises every request. Contention, not the refill math, explains the tail."),("Codex","Turn 2 · codex · round 1","Agrees on the mutex. Adds that refill() runs inside the lock and calls the clock on every request."),("OpenCode","Turn 3 · opencode · round 1","Suggests sharding buckets per key with a concurrent map. Notes it could not reproduce the spike in the read-only workspace."),("Claude Code","Next: turn 4 · claude-code","Paused here. Continue resumes the same scope and order.")].iter().map(|(a,t,b)|card(p).child(row().child(agent(a)).child(*t)).child(*b))))
        .child(column().id("investigate-sidebar").overflow_y_scroll().w(px(370.0)).flex_none().p_5()
            .child(row().child(action("continue-investigate","Continue",true,cx)).child(action("cancel-investigate","Cancel run",false,cx)))
            .child(code("libra investigate continue iv-2f81c0"))
            .child(card(p).child(title("findings.md")).child(muted("single writer · 3 stances",p)).child(label("STANCES",p))
                .child("claude-code · global mutex causes contention").child("codex · refill runs inside the lock").child("opencode · shard buckets per key").child(action("evidence","Attach evidence file…",false,cx)))
            .child(card(p).child(title("New investigation")).child(field("TOPIC","Describe the question to investigate…",p))
                .child(row().child(field("MAX TURNS","6",p)).child(field("QUORUM","2",p)))
                .child(muted("Drag agents to set the order. Read-only, isolated workspace.",p)).child(action("new-investigation","Start investigation",true,cx)))).into_any_element()
}
