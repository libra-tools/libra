//! About: real repository and data-surface availability.
use ely_gpui_component::theme::Palette;
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled};

use super::super::{
    desktop::Desktop,
    widgets::{column, label, mono, muted, title},
};

pub fn render(s: &Desktop, _cx: &Context<Desktop>, p: &Palette) -> AnyElement {
    let availability = &s.availability;
    column()
        .size_full()
        .p_6()
        .gap_2()
        .child(title("About Libra"))
        .child(muted(env!("CARGO_PKG_VERSION"), p))
        .child(label("REPOSITORY", p))
        .child(mono(format!(
            "{} · {} · HEAD {} · {} · {}",
            availability.repo_label,
            availability.branch,
            availability.head,
            availability.object_format,
            availability.repo.label()
        )))
        .child(label("DATA SURFACES", p))
        .children(
            availability
                .sources
                .iter()
                .map(|source| mono(format!("{}: {}", source.name, source.label))),
        )
        .child(muted(
            "Surfaces that have not been checked yet show 未检查.",
            p,
        ))
        .into_any_element()
}
