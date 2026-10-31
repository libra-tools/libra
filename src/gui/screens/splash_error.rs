//! Fail-closed startup illustration, separate from real startup errors.
use ely_gpui_component::theme::Palette;
use gpui::{AnyElement, Context, IntoElement, ParentElement, div, img, prelude::*, px, rgb};

use super::super::{
    desktop::{Desktop, Page},
    widgets::{action, column, mono, nav, row, title},
};
pub fn render(_: &Desktop, cx: &Context<Desktop>, _: &Palette) -> AnyElement {
    column().size_full().items_center().justify_center().gap_5()
        .child(row().w(px(720.0)).h(px(440.0)).gap_0().items_stretch().rounded(px(14.0)).overflow_hidden().bg(rgb(0x0b1530)).text_color(rgb(0xffffff))
            .child(column().w(px(280.0)).flex_none().justify_center().p_8().gap_4().border_r_1().border_color(rgb(0x1c2a4d))
                .child(div().p_3().rounded(px(22.0)).bg(rgb(0xffffff)).child(img("libra/logo.svg").size(px(92.0))))
                .child(title("Libra").text_size(px(38.0))).child("Version control that remembers how the work was done.").child(mono(format!("v{}",env!("CARGO_PKG_VERSION")))))
            .child(column().flex_1().p_6().gap_4().child(title("Libra stopped safely").text_size(px(16.0))).child(div().text_color(rgb(0x9fb0cf)).child("Fail-closed check"))
                .child("!  Configuration · unknown schema in global config").child(div().text_color(rgb(0x9fb0cf)).child("–  Repository · not opened\n–  Agent adapters · not started"))
                .child(column().p_3().rounded(px(10.0)).bg(rgb(0x2a1420)).border_1().border_color(rgb(0x7f1d3a)).child(mono("LBR-CONFIG-001").text_color(rgb(0xfca5b8)))
                    .child("Your global configuration was written by a newer Libra. This build stopped before reading it, so nothing was changed."))
                .child(div().flex_1()).child(row().flex_wrap().child(action("splash-update","Check for updates",true,cx)).child(action("splash-details","Copy details",false,cx)))))
        .child(nav(Page::History,cx)).into_any_element()
}
