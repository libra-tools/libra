//! Operation log, ref transitions and restore preview.
use ely_gpui_component::theme::Palette;
use gpui::{AnyElement, Context, IntoElement, ParentElement, div, prelude::*, px};

use super::super::{
    desktop::Desktop,
    widgets::{action, banner, card, code, column, label, mono, muted, note, row, table, title},
};
pub fn render(s: &Desktop, cx: &Context<Desktop>, p: &Palette) -> AnyElement {
    let ops = [
        (
            "rebase -i main~4",
            "23:02 today",
            "019e3f00",
            "main · 3 refs",
        ),
        ("commit", "22:49 today", "019e3e91", "main · 1 ref"),
        (
            "switch feat/search",
            "22:10 today",
            "019e3d01",
            "wt-review · HEAD",
        ),
        (
            "merge agent/rate-limit",
            "21:40 today",
            "019e3c88",
            "main · 2 refs",
        ),
        (
            "reset --soft HEAD~1",
            "yesterday",
            "019e3a20",
            "main · 2 refs",
        ),
    ];
    let op = ops.get(s.selected).unwrap_or(&ops[0]);
    column().size_full().gap_0().child(banner("2 concurrent operation heads. A linked worktree and this one both wrote while offline from each other.").child(action("reconcile", "Reconcile…", false, cx)))
        .child(row().items_stretch().gap_0().flex_1().min_h(px(0.0))
            .child(column().id("op-list").overflow_y_scroll().w(px(330.0)).p_4().child(title("Operation log")).child(label("COMMAND · ALL", p)).children(ops.iter().enumerate().map(|(i,o)| {
                card(p).id(("op",i)).cursor_pointer().bg(if s.selected == i {p.selection} else {p.surface}).on_click(cx.listener(move |s,_,_,cx| {s.selected=i;cx.notify();}))
                    .child(row().child(mono(o.0)).child(div().flex_1()).child(muted(o.1,p))).child(muted(format!("{} · {}",o.2,o.3),p))
            })))
            .child(column().id("op-detail").overflow_y_scroll().flex_1().p_6().bg(p.surface)
                .child(mono(format!("{}-8ee5-7e62-a54c-0ab1f1bba0f9",op.2))).child(title(format!("libra {}",op.0))).child(muted(format!("{} · worktree {} · by you",op.1,op.3),p))
                .child(table(&["Ref", "Before", "After"], &[vec!["HEAD","2a9e1f3","9f2c4e1"],vec!["refs/heads/main","2a9e1f3","9f2c4e1"],vec!["refs/heads/fix/limiter-config","41be07a","5c0d9e2"]],p))
                .child(card(p).child(title("Commits rewritten")).child(mono("7d13b20 → a41c7e0")).child("Scaffold search module and CLI flag").child(mono("41be07a → 5c0d9e2")).child("Tighten limiter config validation"))
                .child(card(p).child(title("Restore the view from before this operation")).child(note("Moves HEAD and the 2 branch refs above back to their captured values. The restore is itself recorded, so it can be undone too.",p))
                    .child(code(format!("libra op restore {}",op.2))).child(row().child(action("captured-view","Show captured view",false,cx)).child(action("restore","Restore",true,cx)))))).into_any_element()
}
