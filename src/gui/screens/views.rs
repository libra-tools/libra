//! Sparse views, local overlays and dirty status cache.
use ely_gpui_component::theme::Palette;
use gpui::{AnyElement, Context, IntoElement, ParentElement, prelude::*};

use super::super::{
    desktop::Desktop,
    widgets::{action, badge, card, check, field, mono, muted, note, row, table, title},
};
pub fn render(s: &Desktop, cx: &Context<Desktop>, p: &Palette) -> AnyElement {
    row().id("views-page").size_full().overflow_y_scroll().items_start().p_6().gap_5()
        .child(card(p).flex_1().gap_4().child(row().child(title("Sparse view")).child(badge("Enabled",0x166534,0xdcfce7)))
            .child(muted("Narrows what file lists and the working-tree diff show. It never changes files on disk.",p))
            .child(table(&["Patterns · gitignore syntax",""],&[vec!["include","src/**"],vec!["include","docs/**"],vec!["exclude","!src/gen/**"]],p))
            .child(field("NEW PATTERN","include path/**",p)).child(action("view-add","Add",true,cx))
            .child(note("Always unfiltered\nStatus, staged diff and commit-to-commit diffs. You always see exactly what a commit will record.",p))
            .child(action("disable-view","Disable (keep patterns)",false,cx)).child(action("clear-view","Clear all",false,cx)))
        .child(card(p).flex_1().gap_4().child(title("Local layers")).child(action("add-layer","Add layer…",true,cx))
            .child(muted("Local overlays copied onto the working tree on demand. They never enter a commit.",p))
            .child(card(p).child(check("ci-layer","ci · ./ci · priority 10",true,s,cx)).child(badge("applied · 4 files",0x166534,0xdcfce7)))
            .child(card(p).child(check("scratch-layer","scratch · ./overlays/scratch",false,s,cx)).child(muted("priority 0 · not applied",p)))
            .child(note("1 path refused\nscratch/Cargo.toml would overwrite a tracked file, so it is skipped.",p))
            .child(action("apply-layers","Apply enabled layers",true,cx)).child(action("unapply-layers","Unapply",false,cx))
            .child(muted("Unapply skips overlay files you edited by hand. Higher priority wins when two layers write the same path.",p)))
        .child(card(p).flex_1().gap_4().child(title("Status cache")).child(muted("Agents and tools can mark paths dirty so status stays fast on large trees. Marks can only over-report.",p))
            .child(title("Marked dirty · 3")).child(mono("src/gateway/limiter.rs")).child(mono("src/gateway/routes.rs")).child(mono("config/old.toml · deleted"))
            .child(note("Snapshot taken at HEAD 9f2c4e1, 12m ago",p)).child(action("full-rescan","Full rescan",false,cx))).into_any_element()
}
