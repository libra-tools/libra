//! Navigation and presentation-only state for the desktop design.

use std::{collections::HashMap, time::Duration};

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
    data::{DataHandle, Snapshot},
    screens,
    view::SessionsWindow,
    widgets::{mono, muted, nav, row},
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
    /// Operations page index over the current id snapshot.
    pub ops_page: usize,
    /// Operations command filter.
    pub ops_command: String,
    /// Operations free-text search.
    pub ops_query: String,
    /// Dedicated Operations search input (never the command palette text).
    pub ops_input: Entity<TextInput>,
    /// Memory source filter.
    pub memory_source: String,
    /// Memory free-text search.
    pub memory_query: String,
    /// Dedicated Memory search input.
    pub memory_input: Entity<TextInput>,
    /// Dedicated Runs search input (Reviews/Investigate).
    pub runs_input: Entity<TextInput>,
    /// Runs free-text search.
    pub runs_query: String,
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
    /// Background read worker; the UI only sends requests and takes snapshots.
    pub data: DataHandle,
    /// Latest snapshot delivered by the worker.
    pub snapshot: Option<Snapshot>,
    /// Bounded availability probe (published before row loading).
    pub availability: crate::gui::data::Availability,
    /// The raw probe, kept for merging with failed snapshots.
    pub probe_availability: Option<crate::gui::data::Availability>,
}

/// A new snapshot always starts on the first page (A7).
pub fn reset_page_for_new_snapshot() -> usize {
    0
}

/// Page state after a replacement snapshot is delivered: always page one.
pub fn delivered_page(_previous: usize) -> usize {
    reset_page_for_new_snapshot()
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
        let ops_input = cx.new(|cx| TextInput::new(window, cx).placeholder("Search operations"));
        cx.subscribe(&ops_input, |s, input, event: &InputEvent, cx| {
            if *event == InputEvent::Changed || *event == InputEvent::Focus {
                s.ops_query = input.read(cx).text().to_string();
                s.ops_page = 0;
                s.selected = 0;
                cx.notify();
            }
        })
        .detach();
        let memory_input = cx.new(|cx| TextInput::new(window, cx).placeholder("Search memory"));
        cx.subscribe(&memory_input, |s, input, event: &InputEvent, cx| {
            if *event == InputEvent::Changed || *event == InputEvent::Focus {
                s.memory_query = input.read(cx).text().to_string();
                s.selected = 0;
                cx.notify();
            }
        })
        .detach();
        let runs_input = cx.new(|cx| TextInput::new(window, cx).placeholder("Search runs"));
        cx.subscribe(&runs_input, |s, input, event: &InputEvent, cx| {
            if *event == InputEvent::Changed || *event == InputEvent::Focus {
                s.runs_query = input.read(cx).text().to_string();
                s.selected = 0;
                cx.notify();
            }
        })
        .detach();
        let data = DataHandle::spawn();
        data.request();
        let polled = data.clone();
        let sessions_handle = sessions.clone();
        cx.spawn(async move |this, cx| {
            loop {
                if this.upgrade().is_none() {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                if let Some(probe) = polled.take_probe() {
                    let delivered = this.update(cx, |desktop, cx| {
                        if let Some(snapshot) = desktop.snapshot.as_ref() {
                            desktop.availability = if snapshot.context.is_some() {
                                crate::gui::data::availability(Some(snapshot))
                            } else {
                                crate::gui::data::merge_availability(&probe, snapshot)
                            };
                        } else {
                            desktop.availability = probe.clone();
                        }
                        let capabilities = crate::gui::data::capabilities(&desktop.availability);
                        sessions_handle.update(cx, |window, cx| {
                            window.set_capabilities(capabilities, cx);
                        });
                        desktop.probe_availability = Some(probe);
                        cx.notify();
                    });
                    if delivered.is_err() {
                        break;
                    }
                }
                if let Some(snapshot) = polled.take_snapshot() {
                    let rows = snapshot.sessions.clone();
                    let sessions_error = snapshot.sessions_error.clone();
                    let sessions_state = snapshot.sessions_state.clone();
                    let delivered = this.update(cx, |desktop, cx| {
                        let had_context = snapshot.context.is_some();
                        desktop.snapshot = Some(snapshot);
                        if had_context {
                            desktop.availability =
                                crate::gui::data::availability(desktop.snapshot.as_ref());
                        } else if let (Some(probe), Some(snapshot)) = (
                            desktop.probe_availability.as_ref(),
                            desktop.snapshot.as_ref(),
                        ) {
                            desktop.availability =
                                crate::gui::data::merge_availability(probe, snapshot);
                        } else {
                            desktop.availability =
                                crate::gui::data::availability(desktop.snapshot.as_ref());
                        }
                        let merged_capabilities =
                            crate::gui::data::capabilities(&desktop.availability);
                        desktop.ops_page = delivered_page(desktop.ops_page);
                        desktop.selected = 0;
                        sessions_handle.update(cx, |window, cx| {
                            window.set_catalog(
                                crate::gui::view::catalog_from_sessions(&rows, merged_capabilities),
                                sessions_error,
                                Some(sessions_state),
                                cx,
                            );
                        });
                        cx.notify();
                    });
                    if delivered.is_err() {
                        break;
                    }
                }
            }
        })
        .detach();
        Self {
            data,
            snapshot: None,
            availability: crate::gui::data::Availability::unchecked(),
            probe_availability: None,
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
            ops_page: 0,
            ops_command: String::new(),
            ops_query: String::new(),
            ops_input,
            memory_source: String::new(),
            memory_query: String::new(),
            memory_input,
            runs_input,
            runs_query: String::new(),
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

impl Drop for Desktop {
    fn drop(&mut self) {
        // Bounded shutdown runs off the GPUI main thread: a stalled worker
        // never blocks the close path, and its handle stays owned by the
        // process-lifetime registry instead of being detached.
        let data = self.data.clone();
        match std::thread::Builder::new()
            .name("libra-ui-close".to_string())
            .spawn(move || {
                data.shutdown();
            }) {
            Ok(handle) => super::register_close(handle),
            // If the coordinator cannot start, do the bounded shutdown inline
            // rather than skipping cancellation.
            Err(_) => {
                self.data.shutdown();
            }
        }
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
            .flex()
            .flex_col()
            .child(
                FocusScope::new(&self.focus)
                    .root()
                    .flex_1()
                    .min_h(px(0.0))
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
            .child(status_bar(self.snapshot.as_ref(), &p))
    }
}

/// The shared footer stays below the page and its transient notices.
fn status_bar(snapshot: Option<&Snapshot>, p: &ely_gpui_component::theme::Palette) -> gpui::Div {
    let bar = row()
        .w_full()
        .h(px(26.0))
        .px_4()
        .border_t_1()
        .border_color(p.border)
        .bg(p.surface);
    let Some(snapshot) = snapshot else {
        return bar.child(muted("Reading repository…", p));
    };

    if let (super::data::SourceState::Available, Some(context)) =
        (&snapshot.state, &snapshot.context)
    {
        let location = format!("{}  ·  {}", context.worktree_label, context.branch);
        let revision = if context.head_oid.is_some() {
            format!("HEAD {}", context.head)
        } else {
            "No commits yet".to_string()
        };
        return bar
            .child(muted(location, p).flex_1().min_w(px(0.0)).truncate())
            .child(mono(revision).text_color(p.fg_muted).flex_none());
    }

    let mut status = snapshot.state.label().to_string();
    if let Some(message) = &snapshot.message {
        status.push_str(" — ");
        status.push_str(message);
    }
    if let Some(hint) = snapshot.state.hint() {
        status.push_str(" — ");
        status.push_str(hint);
    }
    bar.child(muted(status, p).min_w(px(0.0)).truncate())
}
