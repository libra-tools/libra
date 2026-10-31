//! Cloud backup, remotes and trace-ref sharing presentation.
use ely_gpui_component::theme::Palette;
use gpui::{AnyElement, Context, IntoElement, ParentElement, prelude::*, px};

use super::super::{
    desktop::Desktop,
    widgets::{
        action, badge, card, check, code, column, field, mono, muted, note, row, table, title,
    },
};
pub fn render(s: &Desktop, cx: &Context<Desktop>, p: &Palette) -> AnyElement {
    row().id("cloud-page").size_full().overflow_y_scroll().items_start().p_6().gap_5()
        .child(column().flex_1().child(card(p).child(row().child(title("Cloud backup")).child(badge("37 objects unsynced",0x92400e,0xfef3c7)))
            .child(muted("Object index and metadata in Cloudflare D1, objects in R2.",p))
            .child(table(&["Property","Value"],&[vec!["Project name","acme-libra"],vec!["Repository ID","7c1f0a52-3e9b-4d1a-9b55-0e2f4c8d61aa"],vec!["Last sync","yesterday 18:40"],vec!["Includes","objects, refs, agent-capture catalog"]],p))
            .child(check("full-upload","Force full re-upload",false,s,cx)).child(field("BATCH SIZE","100",p))
            .child(row().child(action("unsynced","Show unsynced",false,cx)).child(action("cloud-sync","Sync now",true,cx)))
            .child(note("Refs, the project name and the capture catalog are frozen before any remote write, so a sync never mixes two states.",p)))
            .child(card(p).child(title("Restore from cloud")).child(field("PROJECT NAME OR REPOSITORY ID","acme-libra",p))
                .child(check("metadata-only","Metadata only (skip object download)",false,s,cx)).child(action("restore-cloud","Restore into new folder…",true,cx))))
        .child(column().w(px(410.0)).flex_none()
            .child(card(p).child(title("Remotes")).child(action("remote-add","Add remote…",false,cx))
                .child(mono("origin")).child(mono("git@github.com:acme/acme.git")).child(action("remote-browser","Open in browser",false,cx))
                .child(mono("mega")).child(mono("https://mega.acme.internal/acme")).child(action("mega-browse","Browse",false,cx)))
            .child(card(p).child(row().child(title("Trace refs")).child(badge("local only",0x334155,0xf1f5f9)))
                .child(muted("Agent capture refs never leave this machine unless you push them here.",p)).child(field("TO","origin ⌄",p)).child(action("trace-refs-push","Push trace refs",true,cx)).child(code("libra agent push --remote origin")))
            .child(card(p).child(title("Cloud credentials")).child(badge("D1 account, database and token set",0x166534,0xdcfce7)).child(badge("R2 endpoint, bucket and keys set",0x166534,0xdcfce7))
                .child(muted("Stored in the encrypted vault. Values are never shown.",p)))).into_any_element()
}
