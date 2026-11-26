//! Startup splash: real repository context and data-surface availability.
use ely_gpui_component::theme::Palette;
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, div};

use super::super::{
    desktop::Desktop,
    widgets::{badge, column, label, mono, title},
};

pub fn render(s: &Desktop, _cx: &Context<Desktop>, p: &Palette) -> AnyElement {
    let availability = &s.availability;
    column()
        .size_full()
        .items_center()
        .justify_center()
        .child(title("Libra"))
        .child(
            div()
                .mt_4()
                .flex()
                .flex_col()
                .gap_1()
                .child(badge(
                    match availability.repo {
                        crate::gui::data::SourceState::Available => "repository available",
                        crate::gui::data::SourceState::Absent => "repository absent",
                        _ => "repository unavailable",
                    },
                    0x5b6474,
                    0xf4f5f7,
                ))
                .child(mono(format!(
                    "{} · {} · HEAD {} · {}",
                    availability.repo_label,
                    availability.branch,
                    availability.head,
                    availability.object_format
                )))
                .child(label("DATA SURFACES", p))
                .children(
                    availability
                        .sources
                        .iter()
                        .map(|source| mono(format!("{}: {}", source.name, source.label))),
                ),
        )
        .into_any_element()
}
