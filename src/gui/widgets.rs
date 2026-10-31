//! Shared native desktop primitives. No HTML or web runtime is used.

use ely_gpui_component::{
    buttons::{Button, ButtonVariant},
    theme::{ControlSize, Palette},
};
use gpui::{Context, Div, FontWeight, ParentElement, SharedString, Styled, div, px, rgb};

use super::desktop::{Desktop, Page};

pub fn row() -> Div {
    div().flex().flex_none().items_center().gap_2()
}
pub fn column() -> Div {
    div().flex().flex_col().gap_3().min_w(px(0.0))
}
pub fn muted(text: impl Into<SharedString>, p: &Palette) -> Div {
    div()
        .text_size(px(12.0))
        .text_color(p.fg_muted)
        .child(text.into())
}
pub fn title(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(19.0))
        .font_weight(FontWeight::BOLD)
        .child(text.into())
}
pub fn label(text: impl Into<SharedString>, p: &Palette) -> Div {
    div()
        .text_size(px(11.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(p.fg_muted)
        .child(text.into())
}
pub fn mono(text: impl Into<SharedString>) -> Div {
    div()
        .font_family("JetBrains Mono")
        .text_size(px(12.0))
        .child(text.into())
}
pub fn card(p: &Palette) -> Div {
    column()
        .flex_none()
        .p_4()
        .rounded(px(10.0))
        .border_1()
        .border_color(p.border)
        .bg(p.surface)
}
pub fn badge(text: impl Into<SharedString>, color: u32, background: u32) -> Div {
    div()
        .flex_none()
        .px_2()
        .py_1()
        .rounded(px(5.0))
        .text_size(px(11.0))
        .text_color(rgb(color))
        .bg(rgb(background))
        .child(text.into())
}
pub fn agent(name: &str) -> Div {
    let (tag, color, bg) = match name {
        "Claude Code" | "claude-code" => ("CC", 0xc2410c, 0xfff1e8),
        "Codex" | "codex" => ("CX", 0x0f766e, 0xe6f6f3),
        "OpenCode" | "opencode" => ("OC", 0x6d28d9, 0xf1ecfe),
        _ => ("E", 0x334155, 0xe2e8f0),
    };
    badge(tag, color, bg).font_weight(FontWeight::BOLD)
}
pub fn note(text: impl Into<SharedString>, p: &Palette) -> Div {
    card(p)
        .p_3()
        .bg(p.bg)
        .text_color(p.fg_muted)
        .text_size(px(12.0))
        .child(text.into())
}
pub fn code(text: impl Into<SharedString>) -> Div {
    mono(text)
        .p_4()
        .rounded(px(8.0))
        .bg(rgb(0x0f172a))
        .text_color(rgb(0xe2e8f0))
}
pub fn nav(page: Page, cx: &Context<Desktop>) -> Button {
    let entity = cx.entity();
    Button::new(
        SharedString::from(format!("nav-{}", page.key())),
        page.label(),
    )
    .size(ControlSize::Sm)
    .variant(ButtonVariant::Ghost)
    .on_click(move |_, window, cx| {
        entity.update(cx, |s, cx| {
            s.navigate(page);
            if page == Page::Search {
                s.tab = 4;
                s.input.update(cx, |input, cx| {
                    input.set_text("", cx);
                    window.focus(input.focus(), cx);
                });
            } else {
                window.focus(&s.focus, cx);
            }
            cx.notify();
        })
    })
}
pub fn action(
    id: &'static str,
    text: &'static str,
    primary: bool,
    cx: &Context<Desktop>,
) -> Button {
    let entity = cx.entity();
    Button::new(id, text)
        .size(ControlSize::Sm)
        .variant(if primary {
            ButtonVariant::Primary
        } else {
            ButtonVariant::Secondary
        })
        .on_click(move |_, _, cx| {
            entity.update(cx, |s, cx| {
                s.notice = Some(format!("{text} — interface preview; no changes were made."));
                cx.notify();
            })
        })
}
pub fn tabs(labels: &[&'static str], selected: usize, cx: &Context<Desktop>) -> Div {
    row()
        .flex_wrap()
        .children(labels.iter().enumerate().map(|(i, text)| {
            let entity = cx.entity();
            Button::new(("page-tab", i), *text)
                .size(ControlSize::Sm)
                .variant(if selected == i {
                    ButtonVariant::Primary
                } else {
                    ButtonVariant::Ghost
                })
                .on_click(move |_, _, cx| {
                    entity.update(cx, |s, cx| {
                        s.tab = i;
                        cx.notify();
                    })
                })
        }))
}
pub fn table(headers: &[&str], rows: &[Vec<&str>], p: &Palette) -> Div {
    column()
        .flex_none()
        .gap_0()
        .border_1()
        .border_color(p.border)
        .rounded(px(8.0))
        .overflow_hidden()
        .child(
            row().gap_3().px_3().py_2().bg(p.bg).children(
                headers
                    .iter()
                    .map(|h| label(h.to_string(), p).flex_1().min_w(px(0.0))),
            ),
        )
        .children(rows.iter().map(|cells| {
            row()
                .gap_3()
                .px_3()
                .py_3()
                .border_t_1()
                .border_color(p.border)
                .children(
                    cells
                        .iter()
                        .map(|cell| div().flex_1().min_w(px(0.0)).child(cell.to_string())),
                )
        }))
}

/// An editable, in-memory form field. No value is persisted or submitted.
#[derive(gpui::IntoElement)]
pub struct PreviewField {
    label: &'static str,
    initial: &'static str,
}

pub fn field(label_text: &'static str, value: &'static str, _p: &Palette) -> PreviewField {
    PreviewField {
        label: label_text,
        initial: value,
    }
}

impl gpui::RenderOnce for PreviewField {
    fn render(self, window: &mut gpui::Window, cx: &mut gpui::App) -> impl gpui::IntoElement {
        use ely_gpui_component::{
            forms::{Input, TextInput},
            theme::ActiveTheme,
        };
        let id = SharedString::from(format!("preview-field:{}:{}", self.label, self.initial));
        let input = window.use_keyed_state(id, cx, |window, cx| {
            let mut input = TextInput::new(window, cx).label(self.label);
            input.set_text(self.initial, cx);
            input
        });
        column()
            .flex_none()
            .gap_2()
            .child(label(self.label, &cx.theme().colors))
            .child(Input::new(&input).size(ControlSize::Sm))
    }
}

pub fn check(
    id: &'static str,
    text: &'static str,
    default: bool,
    state: &Desktop,
    cx: &Context<Desktop>,
) -> ely_gpui_component::forms::Checkbox {
    let checked = state.toggles.get(id).copied().unwrap_or(default);
    let entity = cx.entity();
    ely_gpui_component::forms::Checkbox::new(id, checked)
        .label(text)
        .on_change(move |on, _, cx| {
            entity.update(cx, |s, cx| {
                s.toggles.insert(id, on);
                cx.notify();
            });
        })
}

pub fn banner(text: impl Into<SharedString>, p: &Palette) -> Div {
    row()
        .px_4()
        .py_3()
        .bg(p.warning_subtle)
        .text_color(p.warning)
        .child(text.into())
}
