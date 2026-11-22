//! Capability map: real availability of the five data surfaces.
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
        .child(title("Capability map"))
        .child(muted(
            "Each surface is labelled with the state the window actually observed.",
            p,
        ))
        .child(label("SURFACE · STATE", p))
        .children(availability.sources.iter().map(|source| {
            mono(format!(
                "{}: {} ({})",
                source.name,
                source.label,
                source.state.label()
            ))
        }))
        .into_any_element()
}
