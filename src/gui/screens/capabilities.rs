//! Data requirements and missing-evidence states.
use ely_gpui_component::theme::Palette;
use gpui::{AnyElement, Context, IntoElement, ParentElement, prelude::*, px};

use super::super::{
    desktop::{Desktop, Page},
    model,
    widgets::{column, muted, nav, note, row, table, title},
};
pub fn render(_: &Desktop, cx: &Context<Desktop>, p: &Palette) -> AnyElement {
    let rows = match model::catalog() {
        Ok(catalog) => catalog
            .capability_map
            .iter()
            .map(|r| {
                vec![
                    r.capability.as_str(),
                    r.basis.as_str(),
                    r.data.as_str(),
                    r.missing.as_str(),
                ]
            })
            .collect::<Vec<_>>(),
        Err(error) => return note(error.clone(), p).into_any_element(),
    };
    column().id("capabilities").size_full().overflow_y_scroll().p_6().gap_5()
        .child(row().child(title("Interface capability")).child(nav(Page::Sessions, cx)))
        .child(muted("Plan basis, data required, and what to show when it is missing. The rows are mocked.", p))
        .child(note("Captured source evidence is the boundary. Missing reasoning, usage, test results or approvals remain missing.", p))
        .child(table(&["Capability", "Basis", "Data required", "When missing"], &rows, p).min_w(px(900.0))).into_any_element()
}
