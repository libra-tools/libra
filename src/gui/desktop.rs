//! Navigation and presentation-only state for the desktop design.

use std::collections::HashMap;

use ely_gpui_component::{
    buttons::{Button, ButtonVariant},
    forms::{InputEvent, TextInput},
    primitives::FocusScope,
    theme::{ActiveTheme, ControlSize, Mode, Theme},
};
use gpui::{
    Context, Entity, FocusHandle, IntoElement, ParentElement, Render, Window, div, img, prelude::*,
    px,
};

use super::{screens, view::SessionsWindow, widgets::*};

gpui::actions!(libra_desktop, [OpenSearch]);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Page {
    Welcome,
    Automation,
    Review,
    Investigate,
    Reviews,
    Agents,
    Memory,
    Operations,
    Search,
    Capabilities,
    Lanes,
    History,
    Sessions,
}

impl Page {
    pub const ALL: &'static [Self] = &[
        Self::History,
        Self::Sessions,
        Self::Lanes,
        Self::Capabilities,
        Self::Search,
        Self::Operations,
        Self::Memory,
        Self::Agents,
        Self::Reviews,
        Self::Investigate,
        Self::Review,
        Self::Automation,
        Self::Welcome,
    ];
    pub fn key(self) -> &'static str {
        match self {
            Self::Welcome => "welcome",
            Self::Automation => "automation",
            Self::Review => "review",
            Self::Investigate => "investigate",
            Self::Reviews => "reviews",
            Self::Agents => "agents",
            Self::Memory => "memory",
            Self::Operations => "operations",
            Self::Search => "search",
            Self::Capabilities => "capabilities",
            Self::Lanes => "lanes",
            Self::History => "history",
            Self::Sessions => "sessions",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Welcome => "Welcome",
            Self::Automation => "Automation",
            Self::Review => "Merge review",
            Self::Investigate => "Investigate",
            Self::Reviews => "Review runs",
            Self::Agents => "Agents",
            Self::Memory => "Memory",
            Self::Operations => "Operations",
            Self::Search => "All tools",
            Self::Capabilities => "Capability map",
            Self::Lanes => "Lanes",
            Self::History => "History",
            Self::Sessions => "Sessions",
        }
    }
}

pub struct Desktop {
    pub focus: FocusHandle,
    pub page: Page,
    pub tab: usize,
    pub selected: usize,
    pub filter: usize,
    pub notice: Option<String>,
    pub sessions: Entity<SessionsWindow>,
    pub input: Entity<TextInput>,
    pub query: String,
    pub toggles: HashMap<&'static str, bool>,
    pub draft: Entity<TextInput>,
}

impl Desktop {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        cx.bind_keys([gpui::KeyBinding::new(
            "cmd-k",
            OpenSearch,
            Some("LibraDesktop"),
        )]);
        let search = cx.new(|cx| {
            TextInput::new(window, cx).placeholder("Search redacted content and readable reasoning")
        });
        let sessions = cx.new(|cx| SessionsWindow::new(cx.focus_handle(), search, cx));
        let input = cx
            .new(|cx| TextInput::new(window, cx).placeholder("Search commits, prompts, sessions…"));
        cx.subscribe(&input, |s, input, event: &InputEvent, cx| {
            if *event == InputEvent::Changed {
                s.query = input.read(cx).text().to_string();
                cx.notify();
            }
        })
        .detach();
        let draft = cx
            .new(|cx| TextInput::new(window, cx).placeholder("What should this lane accomplish?"));
        Self {
            toggles: HashMap::new(),
            draft,
            query: String::new(),
            focus,
            page: Page::History,
            tab: 0,
            selected: 0,
            filter: 0,
            notice: None,
            sessions,
            input,
        }
    }
    pub fn navigate(&mut self, page: Page) {
        self.page = page;
        self.tab = 0;
        self.selected = 0;
        self.filter = 0;
        self.notice = None;
    }
}

impl Render for Desktop {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.theme().colors.clone();
        let dark = cx.theme().is_dark();
        let body = match self.page {
            Page::Welcome => screens::welcome::render(self, cx, &p),
            Page::Automation => screens::automation::render(self, cx, &p),
            Page::Review => screens::review::render(self, cx, &p),
            Page::Investigate => screens::investigate::render(self, cx, &p),
            Page::Reviews => screens::reviews::render(self, cx, &p),
            Page::Agents => screens::agents::render(self, cx, &p),
            Page::Memory => screens::memory::render(self, cx, &p),
            Page::Operations => screens::operations::render(self, cx, &p),
            Page::Search => screens::search::render(self, cx, &p),
            Page::Capabilities => screens::capabilities::render(self, cx, &p),
            Page::Lanes => screens::lanes::render(self, cx, &p),
            Page::History => screens::history::render(self, cx, &p),
            Page::Sessions => self.sessions.clone().into_any_element(),
        };
        FocusScope::new(&self.focus).root().size_full().child(
            div()
                .id("desktop-root")
                .key_context("LibraDesktop")
                .on_action(cx.listener(|s, _: &OpenSearch, _, cx| {
                    s.navigate(Page::Search);
                    s.tab = 4;
                    cx.notify();
                }))
                .size_full()
                .flex()
                .flex_col()
                .bg(p.bg)
                .text_color(p.fg)
                .font_family("Inter")
                .text_size(px(13.0))
                .child(
                    row()
                        .flex_none()
                        .h(px(58.0))
                        .px_4()
                        .border_b_1()
                        .border_color(p.border)
                        .bg(p.surface)
                        .child(img("libra/logo.svg").size(px(30.0)))
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::BOLD)
                                .mr_3()
                                .child("Libra"),
                        )
                        .children(Page::ALL.iter().take(6).map(|page| nav(*page, cx)))
                        .child(div().flex_1())
                        .child(badge("Design preview", 0x5b6474, 0xf4f5f7))
                        .child(
                            Button::new("theme", if dark { "Light" } else { "Dark" })
                                .size(ControlSize::Sm)
                                .variant(ButtonVariant::Ghost)
                                .on_click(move |_, _, cx| {
                                    Theme::set_mode_now(
                                        if dark { Mode::Light } else { Mode::Dark },
                                        cx,
                                    )
                                }),
                        ),
                )
                .child(div().flex_1().min_h(px(0.0)).overflow_hidden().child(body))
                .when_some(self.notice.clone(), |root, text| {
                    root.child(
                        row()
                            .px_4()
                            .py_2()
                            .bg(p.surface)
                            .border_t_1()
                            .border_color(p.border)
                            .child(muted(text, &p))
                            .child(div().flex_1())
                            .child(
                                Button::new("dismiss", "Dismiss")
                                    .size(ControlSize::Sm)
                                    .on_click(cx.listener(|s, _, _, cx| {
                                        s.notice = None;
                                        cx.notify();
                                    })),
                            ),
                    )
                }),
        )
    }
}
