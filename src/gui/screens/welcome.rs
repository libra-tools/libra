//! Welcome: repository summary inside a repository, entry points outside.
use ely_gpui_component::theme::Palette;
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, div, prelude::FluentBuilder};

use super::super::{
    desktop::Desktop,
    widgets::{badge, card, column, field, label, mono, muted, title},
};

pub fn render(s: &Desktop, _cx: &Context<Desktop>, p: &Palette) -> AnyElement {
    let availability = &s.availability;
    column()
        .size_full()
        .p_6()
        .gap_2()
        .child(title("Welcome to Libra"))
        .when(availability.repo.is_available(), |body| {
            body.child(badge("repository opened", 0x2f6f4e, 0xf2f7f3))
                .child(mono(format!(
                    "{} · {} · HEAD {} · {}",
                    availability.repo_label,
                    availability.branch,
                    availability.head,
                    availability.object_format
                )))
                .child(label("SURFACES", p))
                .children(availability.sources.iter().map(|source| {
                    mono(format!("{}: {}", source.name, source.label))
                }))
        })
        .when(
            !availability.repo.is_available()
                && !matches!(availability.repo, crate::gui::data::SourceState::Absent),
            |body| {
                body.child(muted(
                    format!(
                        "The repository is {}: {}",
                        availability.repo.label(),
                        availability.repo.hint().unwrap_or("retry shortly")
                    ),
                    p,
                ))
            },
        )
        .when(matches!(availability.repo, crate::gui::data::SourceState::Absent), |body| {
            body.child(muted(
                "No repository is open here. Import, clone or open a repository from the entries below.",
                p,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        card(p)
                            .child(title("Import a repository"))
                            .child(field(
                                "PROJECT NAME OR REPOSITORY ID",
                                crate::gui::screens::cloud::IMPORT_EXAMPLE,
                                p,
                            ))
                            .child(muted("Preview only; import does not execute here.", p)),
                    )
                    .child(card(p).child(title("Clone a repository")))
                    .child(card(p).child(title("Open a repository"))),
            )
        })
        .into_any_element()
}
