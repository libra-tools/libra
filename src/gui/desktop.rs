//! Navigation and presentation-only state for the desktop design.

use std::collections::HashMap;

use ely_gpui_component::{
    buttons::{Button, ButtonVariant},
    forms::{InputEvent, TextInput},
    primitives::FocusScope,
    theme::{ActiveTheme, ControlSize, Mode, Theme},
};
use gpui::{
    AppContext, Context, Entity, FocusHandle, InteractiveElement, IntoElement, ParentElement,
    Render, Styled, Window, div, img, prelude::FluentBuilder, px,
};

use super::{
    screens,
    view::SessionsWindow,
    widgets::{badge, muted, nav, row},
};

gpui::actions!(libra_desktop, [OpenSearch, CloseOverlay]);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Page {
    About,
    SplashError,
    Splash,
    Settings,
    Mega,
    Deps,
    Views,
    Cloud,
    Rebase,
    Refs,
    Bisect,
    Blame,
    Conflict,
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
        Self::Conflict,
        Self::Blame,
        Self::Bisect,
        Self::Refs,
        Self::Rebase,
        Self::Cloud,
        Self::Views,
        Self::Deps,
        Self::Mega,
        Self::Settings,
        Self::Splash,
        Self::SplashError,
        Self::About,
    ];
    pub fn key(self) -> &'static str {
        match self {
            Self::About => "about",
            Self::SplashError => "splash_error",
            Self::Splash => "splash",
            Self::Settings => "settings",
            Self::Mega => "mega",
            Self::Deps => "deps",
            Self::Views => "views",
            Self::Cloud => "cloud",
            Self::Rebase => "rebase",
            Self::Refs => "refs",
            Self::Bisect => "bisect",
            Self::Blame => "blame",
            Self::Conflict => "conflict",
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
            Self::About => "About Libra",
            Self::SplashError => "Startup error",
            Self::Splash => "Splash",
            Self::Settings => "Settings",
            Self::Mega => "Mega2 browser",
            Self::Deps => "Dependencies",
            Self::Views => "Views & layers",
            Self::Cloud => "Cloud & remotes",
            Self::Rebase => "Rebase",
            Self::Refs => "Stashes, tags & worktrees",
            Self::Bisect => "Bisect",
            Self::Blame => "Blame",
            Self::Conflict => "Conflicts",
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
    pub return_page: Page,
    pub tab: usize,
    pub selected: usize,
    pub filter: usize,
    pub rebase_actions: [usize; 6],
    pub notice: Option<String>,
    pub sessions: Entity<SessionsWindow>,
    pub input: Entity<TextInput>,
    pub history_input: Entity<TextInput>,
    pub history_query: String,
    pub query: String,
    pub toggles: HashMap<&'static str, bool>,
    pub draft: Entity<TextInput>,
    pub review_comment: Entity<TextInput>,
}

impl Desktop {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        cx.bind_keys([
            gpui::KeyBinding::new("cmd-k", OpenSearch, Some("LibraDesktop")),
            gpui::KeyBinding::new("escape", CloseOverlay, Some("LibraDesktop")),
        ]);
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
        let history_input = cx.new(|cx| TextInput::new(window, cx).placeholder("Search history"));
        cx.subscribe(&history_input, |s, input, event: &InputEvent, cx| {
            if *event == InputEvent::Changed {
                s.history_query = input.read(cx).text().to_string();
                cx.notify();
            }
        })
        .detach();
        let draft = cx
            .new(|cx| TextInput::new(window, cx).placeholder("What should this lane accomplish?"));
        let review_comment =
            cx.new(|cx| TextInput::new(window, cx).placeholder("Leave a review comment…"));
        Self {
            history_input,
            history_query: String::new(),
            review_comment,
            return_page: Page::History,
            toggles: HashMap::new(),
            draft,
            query: String::new(),
            focus,
            page: Page::History,
            tab: 0,
            selected: 0,
            filter: 0,
            rebase_actions: [0, 4, 3, 1, 0, 5],
            notice: None,
            sessions,
            input,
        }
    }
    pub fn navigate(&mut self, page: Page) {
        if page == Page::Search && self.page != Page::Search {
            self.return_page = self.page;
        }
        self.page = page;
        self.tab = 0;
        self.selected = match page {
            Page::Mega => 5,
            Page::Deps => 3,
            _ => 0,
        };
        self.filter = 0;
        self.notice = None;
    }
}

impl Render for Desktop {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.theme().colors.clone();
        let dark = cx.theme().is_dark();
        let body = match self.page {
            Page::About => screens::about::render(self, cx, &p),
            Page::SplashError => screens::splash_error::render(self, cx, &p),
            Page::Splash => screens::splash::render(self, cx, &p),
            Page::Settings => screens::settings::render(self, cx, &p),
            Page::Mega => screens::mega::render(self, cx, &p),
            Page::Deps => screens::deps::render(self, cx, &p),
            Page::Views => screens::views::render(self, cx, &p),
            Page::Cloud => screens::cloud::render(self, cx, &p),
            Page::Rebase => screens::rebase::render(self, cx, &p),
            Page::Refs => screens::refs::render(self, cx, &p),
            Page::Bisect => screens::bisect::render(self, cx, &p),
            Page::Blame => screens::blame::render(self, cx, &p),
            Page::Conflict => screens::conflict::render(self, cx, &p),
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
        div()
            .id("desktop-keybindings")
            .key_context("LibraDesktop")
            .on_action(cx.listener(|s, _: &OpenSearch, window, cx| {
                s.navigate(Page::Search);
                s.tab = 4;
                s.input.update(cx, |input, cx| {
                    input.set_text("", cx);
                    window.focus(input.focus(), cx);
                });
                cx.notify();
            }))
            .on_action(cx.listener(|s, _: &CloseOverlay, window, cx| {
                if s.page == Page::Search {
                    s.navigate(s.return_page);
                }
                s.notice = None;
                window.focus(&s.focus, cx);
                cx.notify();
            }))
            .size_full()
            .child(
                FocusScope::new(&self.focus)
                    .root()
                    .size_full()
                    .flex()
                    .flex_col()
                    .bg(p.bg)
                    .text_color(p.fg)
                    .font_family("Inter")
                    .text_size(px(13.0))
                    .when(self.page != Page::History, |root| {
                        root.child(
                            row()
                                .id("global-navigation")
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
                                .children(
                                    [
                                        Page::History,
                                        Page::Lanes,
                                        Page::Sessions,
                                        Page::Reviews,
                                        Page::Memory,
                                        Page::Agents,
                                    ]
                                    .into_iter()
                                    .map(|page| {
                                        nav(page, cx).variant(if self.page == page {
                                            ButtonVariant::Subtle
                                        } else {
                                            ButtonVariant::Ghost
                                        })
                                    }),
                                )
                                .child(div().flex_1())
                                .child(nav(Page::Search, cx))
                                .child(nav(Page::Settings, cx))
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
                    })
                    .child(
                        div()
                            .id(gpui::SharedString::from(format!(
                                "page-{}",
                                self.page.key()
                            )))
                            .flex_1()
                            .min_h(px(0.0))
                            .overflow_hidden()
                            .child(body),
                    )
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
