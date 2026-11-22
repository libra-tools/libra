//! Search and command palette over pages and the availability projection.
use ely_gpui_component::{buttons::Button, theme::Palette};
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, prelude::FluentBuilder, px,
};

use super::super::{
    desktop::{Desktop, Page},
    widgets::{column, label, mono, muted, title},
};

pub fn render(s: &Desktop, cx: &Context<Desktop>, p: &Palette) -> AnyElement {
    let query = s.query.trim().to_lowercase();
    let pages: Vec<Page> = Page::ALL
        .iter()
        .copied()
        .filter(|page| page.label().to_lowercase().contains(&query))
        .collect();
    let availability = &s.availability;
    column()
        .size_full()
        .items_center()
        .p_6()
        .child(
            column()
                .id("search-results")
                .w_full()
                .max_w(px(900.0))
                .max_h_full()
                .overflow_y_scroll()
                .gap_2()
                .child(title("Search"))
                .child(s.input.clone())
                .child(label("PAGES", p))
                .children(pages.iter().copied().map(|page| {
                    Button::new(
                        gpui::SharedString::from(format!("page-{}", page.key())),
                        page.label(),
                    )
                    .on_click(cx.listener(move |s, _, _, cx| {
                        s.navigate(page);
                        cx.notify();
                    }))
                }))
                .when(pages.is_empty(), |body| {
                    body.child(muted("No pages match the query.", p))
                })
                .child(label("SURFACES", p))
                .children(availability.sources.iter().map(|source| {
                    mono(format!("{}: {}", source.name, source.label))
                }))
                .when(query.is_empty(), |body| {
                    body.child(muted(
                        "Search covers pages and data-surface states; captured content lives in Sessions.",
                        p,
                    ))
                }),
        )
        .into_any_element()
}
