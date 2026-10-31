//! Bisect range, current candidate and decision log.
use ely_gpui_component::theme::Palette;
use gpui::{AnyElement, Context, IntoElement, ParentElement, div, prelude::*, px};

use super::super::{
    desktop::{Desktop, Page},
    widgets::{
        action, badge, card, check, code, column, field, mono, muted, nav, note, row, table, title,
    },
};
pub fn render(s: &Desktop, cx: &Context<Desktop>, p: &Palette) -> AnyElement {
    column().size_full().gap_0().child(row().px_5().py_3().bg(p.surface).child(title("Bisecting")).child(muted("Find the commit where p99 latency regressed · good v0.4.0 · bad main",p)).child(div().flex_1()).child(badge("Step 3 · ~2 steps left",0x1d4ed8,0xeaf1ff)).child(action("reset-bisect","Reset to main",false,cx)))
        .child(row().id("bisect-body").overflow_y_scroll().items_start().flex_1().p_6().gap_5()
            .child(column().flex_1().gap_5()
                .child(card(p).child(row().child(title("Range")).child(muted("4 candidates left of 13",p)))
                    .child(row().py_6().justify_between().children((0..13).map(|i| div().size(px(16.0)).rounded_full().bg(if i<4{p.success}else if i==12{p.danger}else if i==7{p.accent}else{p.border}))))
                    .child(muted("● good     ● bad     ◇ skipped     ● checked out now     ○ still possible     ○ ruled out",p)))
                .child(card(p).child(mono("2a9e1f3")).child(title("Refactor request context propagation")).child(muted("you · 2h ago",p)).child("This commit is checked out. Test it, then mark it.")
                    .child(row().child(action("bisect-good","Good",true,cx)).child(action("bisect-bad","Bad",false,cx)).child(action("bisect-skip","Skip",false,cx)))
                    .child(check("bisect-terms","Use my own terms instead of good / bad (e.g. fast / slow)",false,s,cx)))
                .child(card(p).child(title("Run automatically")).child(muted("Libra checks out each candidate and runs this command. Exit 0 means good, non-zero means bad.",p))
                    .child(field("COMMAND","./scripts/bench-p99.sh",p)).child(action("bisect-run","Run",true,cx))))
            .child(column().w(px(340.0)).flex_none().child(card(p).child(title("Log")).child(table(&["Mark","Commit","Note"],&[vec!["GOOD","e5f0c88","v0.4.0"],vec!["BAD","9f2c4e1","main"],vec!["GOOD","f31c9e7","Bump tokio and axum"],vec!["SKIP","0b6a2d4","does not build"]],p)))
                .child(card(p).child(title("Captured evidence nearby")).child(note("Candidate 9f2c4e1 has an agent session that recorded cargo test -p gateway exit 0. That is a recorded result, not a benchmark, so it is not used as a mark.",p)).child(nav(Page::Sessions,cx)))
                .child(code("libra bisect log")))).into_any_element()
}
