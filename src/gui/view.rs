//! Sessions window laid out like the sessions prototype.
//!
//! The top bar, session list, and Overview / Conversation / Changes views are
//! interactive. The rows are the bundled mock catalog, not a live repository.

use std::collections::HashSet;

use ely_gpui_component::{
    buttons::{Button, ButtonVariant},
    data_display::{Badge, Tone},
    forms::{Choice, ChoiceChips, InputEvent, SearchInput, TextInput},
    overlays::Dialog,
    theme::{ActiveTheme, ControlSize},
    typography::{Heading, Paragraph},
};
use gpui::{
    Context, Entity, FocusHandle, IntoElement, ParentElement, Render, SharedString, Window, div,
    prelude::*, px,
};

use super::model::{self, Catalog, Event, Session, clock, date_bucket};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pane {
    Overview,
    Conversation,
    Changes,
}

impl Pane {
    fn key(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Conversation => "conversation",
            Self::Changes => "changes",
        }
    }

    fn from_key(key: &str) -> Self {
        match key {
            "conversation" => Self::Conversation,
            "changes" => Self::Changes,
            _ => Self::Overview,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Drawer {
    None,
    Info,
    Usage,
    Capture,
}

/// The sessions screen.
pub struct SessionsWindow {
    focus: FocusHandle,
    search: Entity<TextInput>,
    query: String,
    search_open: bool,
    sidebar_open: bool,
    pane: Pane,
    drawer: Drawer,
    map_open: bool,
    worktree: String,
    agents: Vec<String>,
    dates: Vec<String>,
    statuses: Vec<String>,
    selected: String,
    expanded: HashSet<String>,
}

impl SessionsWindow {
    pub fn new(focus: FocusHandle, search: Entity<TextInput>, cx: &mut Context<Self>) -> Self {
        cx.subscribe(&search, |window, input, event: &InputEvent, cx| {
            if *event == InputEvent::Changed || *event == InputEvent::Focus {
                window.query = input.read(cx).text().to_string();
                window.search_open = !window.query.trim().is_empty();
                cx.notify();
            }
        })
        .detach();
        Self {
            focus,
            search,
            query: String::new(),
            search_open: false,
            sidebar_open: true,
            pane: Pane::Overview,
            drawer: Drawer::None,
            map_open: false,
            worktree: "all".into(),
            agents: Vec::new(),
            dates: Vec::new(),
            statuses: Vec::new(),
            selected: "s-cc-arch".into(),
            expanded: HashSet::new(),
        }
    }

    fn catalog(&self) -> Result<&'static Catalog, &'static str> {
        match model::catalog() {
            Ok(catalog) => Ok(catalog),
            Err(error) => Err(error.as_str()),
        }
    }
}

impl Render for SessionsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors.clone();
        let this = cx.entity();
        let catalog = match self.catalog() {
            Ok(catalog) => catalog,
            Err(error) => {
                return ely_gpui_component::primitives::FocusScope::new(&self.focus)
                    .root()
                    .size_full()
                    .p_8()
                    .bg(colors.bg)
                    .child(Heading::h1("Libra Sessions"))
                    .child(Paragraph::new(error.to_string()))
                    .into_any_element();
            }
        };
        let visible = visible_sessions(catalog, self);
        let selected = catalog
            .sessions
            .iter()
            .find(|session| session.id == self.selected)
            .or_else(|| visible.first().copied());
        let capture = selected
            .map(|session| session.capture_label.as_str())
            .unwrap_or("Capture status");

        ely_gpui_component::primitives::FocusScope::new(&self.focus)
            .root()
            .size_full()
            .flex()
            .flex_col()
            .bg(colors.bg)
            .text_color(colors.fg)
            .child(header(self, catalog, capture, this.clone(), &colors))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_hidden()
                    .when(self.sidebar_open, |row| {
                        row.child(sidebar(self, catalog, &visible, this.clone(), &colors))
                    })
                    .child(main_column(self, selected, this.clone(), &colors)),
            )
            .when(self.map_open, |root| {
                let close = this.clone();
                root.child(
                    Dialog::new("capability-map", "Interface capability", move |_, cx| {
                        close.update(cx, |window, cx| {
                            window.map_open = false;
                            cx.notify();
                        });
                    })
                    .detail("Plan basis, data required, and what to show when it is missing. The rows are mocked.")
                    .child(capability_body(catalog, &colors)),
                )
            })
            .into_any_element()
    }
}

fn header(
    window: &SessionsWindow,
    catalog: &Catalog,
    capture: &str,
    this: Entity<SessionsWindow>,
    colors: &ely_gpui_component::theme::Palette,
) -> impl IntoElement {
    let worktrees = worktree_choices(catalog);
    div()
        .h(px(52.0))
        .flex()
        .flex_none()
        .items_center()
        .gap_3()
        .px_3()
        .border_b_1()
        .border_color(colors.border)
        .bg(colors.surface)
        .child(icon_button("sidebar", "≡", this.clone(), |window| {
            window.sidebar_open = !window.sidebar_open;
        }))
        .child(
            ChoiceChips::new("worktree", worktrees)
                .selected([SharedString::from(window.worktree.clone())])
                .on_change({
                    let this = this.clone();
                    move |values, _, cx| {
                        let Some(value) = values.first() else {
                            return;
                        };
                        let value = value.to_string();
                        this.update(cx, |window, cx| {
                            window.worktree = value;
                            cx.notify();
                        });
                    }
                }),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(180.0))
                .relative()
                .child(SearchInput::new("session-search", &window.search).size(ControlSize::Sm))
                .when(window.search_open, |bar| {
                    bar.child(search_results(window, catalog, this.clone(), colors))
                }),
        )
        .child(
            Badge::new(capture).dot().tone(capture_tone(
                window
                    .catalog()
                    .ok()
                    .and_then(|catalog| {
                        catalog
                            .sessions
                            .iter()
                            .find(|session| session.id == window.selected)
                    })
                    .map(|session| session.capture.as_str())
                    .unwrap_or(""),
            )),
        )
        .child(text_button(
            "map",
            "Capability map",
            ButtonVariant::Secondary,
            this.clone(),
            |window| {
                window.map_open = true;
            },
        ))
}

fn sidebar(
    window: &SessionsWindow,
    catalog: &Catalog,
    visible: &[&Session],
    this: Entity<SessionsWindow>,
    colors: &ely_gpui_component::theme::Palette,
) -> impl IntoElement {
    let filters = filter_choices();
    let selected_filters = selected_filters(window);
    div()
        .w(px(300.0))
        .flex_none()
        .h_full()
        .flex()
        .flex_col()
        .border_r_1()
        .border_color(colors.border)
        .bg(colors.bg)
        .child(
            div().p_2().child(
                ChoiceChips::new("filters", filters)
                    .multiple()
                    .selected(selected_filters)
                    .on_change({
                        let this = this.clone();
                        move |values, _, cx| {
                            let values: Vec<String> =
                                values.iter().map(|value| value.to_string()).collect();
                            this.update(cx, |window, cx| {
                                window.agents = take_prefix(&values, "agent:");
                                window.dates = take_prefix(&values, "date:");
                                window.statuses = take_prefix(&values, "status:");
                                cx.notify();
                            });
                        }
                    }),
            ),
        )
        .child(
            div()
                .id("session-list")
                .flex_1()
                .min_h(px(0.0))
                .overflow_y_scroll()
                .px_2()
                .children(
                    grouped(catalog, visible)
                        .into_iter()
                        .map(|(label, sessions)| {
                            session_group(label, sessions, &window.selected, this.clone(), colors)
                        }),
                ),
        )
        .child(
            div()
                .px_3()
                .py_2()
                .text_xs()
                .text_color(colors.fg_muted)
                .border_t_1()
                .border_color(colors.border)
                .child(format!(
                    "{} of {} sessions",
                    visible.len(),
                    catalog.sessions.len()
                )),
        )
}

fn main_column(
    window: &SessionsWindow,
    session: Option<&Session>,
    this: Entity<SessionsWindow>,
    colors: &ely_gpui_component::theme::Palette,
) -> impl IntoElement {
    let Some(session) = session else {
        return div()
            .flex_1()
            .p_8()
            .child(Paragraph::new("No sessions match the current scope."))
            .into_any_element();
    };
    div()
        .flex_1()
        .min_w(px(0.0))
        .h_full()
        .flex()
        .flex_col()
        .child(session_header(window, session, this.clone(), colors))
        .child(
            div()
                .flex_1()
                .min_h(px(0.0))
                .flex()
                .overflow_hidden()
                .child(
                    div()
                        .id("session-body")
                        .flex_1()
                        .min_w(px(0.0))
                        .overflow_y_scroll()
                        .p_4()
                        .child(pane_body(window, session, this.clone(), colors)),
                )
                .when(window.drawer != Drawer::None, |row| {
                    row.child(drawer(window.drawer, session, this, colors))
                }),
        )
        .into_any_element()
}

fn session_header(
    state: &SessionsWindow,
    session: &Session,
    this: Entity<SessionsWindow>,
    colors: &ely_gpui_component::theme::Palette,
) -> impl IntoElement {
    use super::widgets::{agent, column, mono, muted, row, title};
    column()
        .flex_none()
        .px_6()
        .pt_4()
        .pb_3()
        .border_b_1()
        .border_color(colors.border)
        .child(
            row()
                .child(agent(&session.agent))
                .child(session.agent.clone())
                .child(muted(
                    format!(
                        "{} · {} – {}",
                        session.start.get(..10).unwrap_or(""),
                        clock(&session.start),
                        clock(&session.end)
                    ),
                    colors,
                ))
                .child(div().flex_1())
                .child(drawer_button(
                    "info",
                    "Session info",
                    Drawer::Info,
                    state.drawer,
                    this.clone(),
                ))
                .child(drawer_button(
                    "usage",
                    "Usage",
                    Drawer::Usage,
                    state.drawer,
                    this.clone(),
                ))
                .child(drawer_button(
                    "capture",
                    "Capture details",
                    Drawer::Capture,
                    state.drawer,
                    this.clone(),
                )),
        )
        .child(title(session.title.clone()))
        .child(
            mono(format!(
                "libra @ {} · HEAD {} · {}",
                session.worktree, session.head, session.model
            ))
            .text_color(colors.fg_muted),
        )
        .child(muted(
            if session.summary.is_empty() {
                "This session has no summary; identify it by agent and time.".to_string()
            } else {
                format!("{} · {}", session.summary_source, session.summary)
            },
            colors,
        ))
        .child(
            ChoiceChips::new(
                "pane",
                [
                    Choice::new("overview", "Overview"),
                    Choice::new("conversation", "Conversation"),
                    Choice::new("changes", "Changes"),
                ],
            )
            .selected([SharedString::from(state.pane.key())])
            .on_change(move |values, _, cx| {
                if let Some(value) = values.first() {
                    this.update(cx, |s, cx| {
                        s.pane = Pane::from_key(value);
                        cx.notify();
                    });
                }
            }),
        )
}

fn pane_body(
    window: &SessionsWindow,
    session: &Session,
    this: Entity<SessionsWindow>,
    colors: &ely_gpui_component::theme::Palette,
) -> gpui::AnyElement {
    match window.pane {
        Pane::Overview => overview(session, this, colors).into_any_element(),
        Pane::Conversation => conversation(window, session, this, colors).into_any_element(),
        Pane::Changes => changes(session, colors).into_any_element(),
    }
}

fn overview(
    session: &Session,
    this: Entity<SessionsWindow>,
    colors: &ely_gpui_component::theme::Palette,
) -> impl IntoElement {
    let first_user = session.turns.iter().find_map(|turn| {
        if turn.user.is_empty() {
            None
        } else {
            Some(turn.user.as_str())
        }
    });
    let last_answer = session.turns.iter().rev().find_map(|turn| {
        turn.events.iter().rev().find_map(|event| match event {
            Event::Assistant { text, .. } if !text.is_empty() => Some(text.as_str()),
            _ => None,
        })
    });
    div()
        .flex()
        .flex_col()
        .gap_4()
        .child(
            div()
                .flex()
                .gap_2()
                .flex_wrap()
                .child(status_card(
                    "Is the session still running?",
                    &session.running_label,
                    &format!("Ended at {}", clock(&session.end)),
                    colors,
                ))
                .child(status_card(
                    "Is capture and archiving done?",
                    &session.capture_label,
                    capture_detail(session),
                    colors,
                ))
                .child(status_card(
                    "Can the content be read?",
                    &session.readable_label,
                    "Ciphertext stays out of search and summaries.",
                    colors,
                ))
                .child(status_card(
                    "What evidence was recorded?",
                    if session.evidence.is_empty() {
                        "None recorded"
                    } else {
                        session.evidence.as_str()
                    },
                    "Recorded commands only. Nothing here re-runs them.",
                    colors,
                )),
        )
        .when(first_user.is_some() || last_answer.is_some(), |body| {
            body.child(
                div()
                    .flex()
                    .gap_3()
                    .child(quote_card(
                        "First real question",
                        first_user.unwrap_or("No user text"),
                        this.clone(),
                        colors,
                    ))
                    .child(quote_card(
                        "Last reply",
                        last_answer.unwrap_or("No assistant text"),
                        this,
                        colors,
                    )),
            )
        })
        .child(section(
            "Recorded verification results",
            if session.verification.is_empty() {
                div()
                    .text_color(colors.fg_muted)
                    .child("No verification results captured.")
                    .into_any_element()
            } else {
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(session.verification.iter().map(|item| {
                        div()
                            .flex()
                            .gap_2()
                            .items_center()
                            .child(Badge::new(item.result.clone()).tone(
                                if item.result == "failed" {
                                    Tone::Danger
                                } else {
                                    Tone::Success
                                },
                            ))
                            .child(item.label.clone())
                            .child(div().text_color(colors.fg_muted).child(item.detail.clone()))
                            .into_any_element()
                    }))
                    .into_any_element()
            },
            colors,
        ))
        .child(section(
            "Capture completeness",
            div()
                .flex()
                .gap_2()
                .items_center()
                .child(
                    Badge::new(session.capture_label.clone())
                        .dot()
                        .tone(capture_tone(&session.capture)),
                )
                .child(
                    div()
                        .text_color(colors.fg_muted)
                        .child(session.readable_label.clone()),
                )
                .into_any_element(),
            colors,
        ))
        .when(!session.ai_summary.is_empty(), |body| {
            body.child(section(
                "AI semantic summary",
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(
                        session
                            .ai_summary
                            .iter()
                            .map(|line| div().child(line.clone()).into_any_element()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors.fg_muted)
                            .child("Summaries use readable content only. Ciphertext is excluded."),
                    )
                    .into_any_element(),
                colors,
            ))
        })
}

fn conversation(
    window: &SessionsWindow,
    session: &Session,
    this: Entity<SessionsWindow>,
    colors: &ely_gpui_component::theme::Palette,
) -> impl IntoElement {
    if session.turns.is_empty() {
        return div()
            .text_color(colors.fg_muted)
            .child("No readable content.")
            .into_any_element();
    }
    div()
        .flex()
        .flex_col()
        .gap_4()
        .children(
            session
                .turns
                .iter()
                .map(|turn| turn_card(window, session, turn, this.clone(), colors)),
        )
        .into_any_element()
}

fn changes(session: &Session, colors: &ely_gpui_component::theme::Palette) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(section(
            "File records",
            if session.files.is_empty() {
                div()
                    .text_color(colors.fg_muted)
                    .child("No sourced file records.")
                    .into_any_element()
            } else {
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(session.files.iter().map(|file| {
                        div()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(file.path.clone()),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(colors.fg_muted)
                                    .child(file.source.clone()),
                            )
                            .when(!file.note.is_empty(), |row| {
                                row.child(div().text_sm().child(file.note.clone()))
                            })
                            .into_any_element()
                    }))
                    .into_any_element()
            },
            colors,
        ))
        .child(section(
            "Linked commits",
            if session.commits.is_empty() {
                div()
                    .text_color(colors.fg_muted)
                    .child("No linked commits.")
                    .into_any_element()
            } else {
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(session.commits.iter().map(|commit| {
                        div()
                            .flex()
                            .gap_2()
                            .items_center()
                            .child(Badge::new(commit.sha.clone()).tone(Tone::Accent))
                            .child(commit.message.clone())
                            .when(!commit.kind.is_empty(), |row| {
                                row.child(Badge::new(commit.kind.clone()))
                            })
                            .into_any_element()
                    }))
                    .into_any_element()
            },
            colors,
        ))
}

fn turn_card(
    window: &SessionsWindow,
    session: &Session,
    turn: &model::Turn,
    this: Entity<SessionsWindow>,
    colors: &ely_gpui_component::theme::Palette,
) -> gpui::AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .gap_2()
                .items_center()
                .child(Badge::new(format!("Turn {}", turn.n)).tone(Tone::Accent))
                .child(div().text_color(colors.fg_muted).child(turn.time.clone()))
                .child(
                    div()
                        .text_xs()
                        .text_color(colors.fg_muted)
                        .child(session.agent.clone()),
                ),
        )
        .when(!turn.user.is_empty(), |card| {
            card.child(
                div()
                    .p_3()
                    .rounded(px(8.0))
                    .bg(colors.selection)
                    .child(div().text_xs().text_color(colors.fg_muted).child("User"))
                    .child(turn.user.clone()),
            )
        })
        .children(
            turn.events
                .iter()
                .enumerate()
                .map(|(index, event)| event_row(window, turn, index, event, this.clone(), colors)),
        )
        .into_any_element()
}

fn event_row(
    window: &SessionsWindow,
    turn: &model::Turn,
    index: usize,
    event: &Event,
    this: Entity<SessionsWindow>,
    colors: &ely_gpui_component::theme::Palette,
) -> gpui::AnyElement {
    let key = format!("{}:{index}", turn.id);
    let open = window.expanded.contains(&key);
    match event {
        Event::Reasoning {
            status,
            provider,
            text,
        } => {
            let preview: String = text.chars().take(90).collect();
            div()
                .flex()
                .flex_col()
                .rounded(px(8.0))
                .border_1()
                .border_color(colors.border)
                .child(row_button(
                    format!("reason-{key}"),
                    format!(
                        "Reasoning · {provider} · {} · {preview}",
                        plain_status(status)
                    ),
                    this.clone(),
                    move |window| toggle(window, key.clone()),
                ))
                .when(open, |block| {
                    block.child(div().p_3().text_sm().child(plain(text)))
                })
                .into_any_element()
        }
        Event::Tool {
            name,
            input,
            output,
            status,
        } => div()
            .flex()
            .flex_col()
            .rounded(px(8.0))
            .border_1()
            .border_color(colors.border)
            .child(row_button(
                format!("tool-{key}"),
                format!("{name} · {} · {input}", tool_status(status)),
                this,
                move |window| toggle(window, key.clone()),
            ))
            .when(open && !output.is_empty(), |block| {
                block.child(div().p_3().text_sm().bg(colors.bg).child(output.clone()))
            })
            .into_any_element(),
        Event::Assistant { text, model } => div()
            .p_3()
            .rounded(px(8.0))
            .bg(colors.surface)
            .border_1()
            .border_color(colors.border)
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors.fg_muted)
                            .child("Assistant"),
                    )
                    .when(!model.is_empty(), |row| {
                        row.child(Badge::new(model.clone()))
                    }),
            )
            .child(plain(text))
            .into_any_element(),
        Event::Note { label, text } => div()
            .flex()
            .flex_col()
            .gap_1()
            .p_3()
            .rounded(px(8.0))
            .bg(colors.surface)
            .child(Badge::new(label.clone()).tone(Tone::Warning))
            .when(!text.is_empty(), |block| block.child(text.clone()))
            .into_any_element(),
    }
}

fn drawer(
    drawer: Drawer,
    session: &Session,
    this: Entity<SessionsWindow>,
    colors: &ely_gpui_component::theme::Palette,
) -> impl IntoElement {
    let (title, body) = match drawer {
        Drawer::Info => (
            "Session info",
            format!(
                "{}\n{}\n{}\nlibra @ {} · HEAD {}\n{}",
                session.agent,
                session.model,
                session.id,
                session.worktree,
                session.head,
                session.summary_source
            ),
        ),
        Drawer::Usage => (
            "Usage",
            format!(
                "Input {}\nOutput {}\nCalls {}\nReasoning {}\n{}",
                session.usage.input,
                session.usage.output,
                session.usage.calls,
                session.usage.reasoning,
                session.usage.cost_note
            ),
        ),
        Drawer::Capture => (
            "Capture details",
            format!(
                "{}\n{}\n{}",
                session.capture_label, session.readable_label, session.evidence
            ),
        ),
        Drawer::None => ("", String::new()),
    };
    div()
        .id("session-drawer")
        .w(px(300.0))
        .flex_none()
        .h_full()
        .overflow_y_scroll()
        .p_3()
        .border_l_1()
        .border_color(colors.border)
        .bg(colors.surface)
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
                .child(div().flex_1().child(text_button(
                    "close-drawer",
                    "Close",
                    ButtonVariant::Ghost,
                    this,
                    |window| {
                        window.drawer = Drawer::None;
                    },
                ))),
        )
        .child(Paragraph::new(body))
}

fn search_results(
    window: &SessionsWindow,
    catalog: &Catalog,
    this: Entity<SessionsWindow>,
    colors: &ely_gpui_component::theme::Palette,
) -> impl IntoElement {
    let query = window.query.trim().to_lowercase();
    let hits = search_hits(catalog, window, &query);
    div()
        .id("search-results")
        .absolute()
        .top(px(36.0))
        .left_0()
        .right_0()
        .max_h(px(280.0))
        .overflow_y_scroll()
        .p_2()
        .rounded(px(8.0))
        .border_1()
        .border_color(colors.border)
        .bg(colors.surface)
        .flex()
        .flex_col()
        .gap_1()
        .children(if hits.is_empty() {
            vec![
                div()
                    .text_sm()
                    .text_color(colors.fg_muted)
                    .child("No matches in stored, redacted text.")
                    .into_any_element(),
            ]
        } else {
            hits.into_iter()
                .map(|hit| {
                    let id = hit.id.clone();
                    row_button(
                        format!("hit-{id}-{}", hit.label),
                        format!("{} · {}", hit.title, hit.label),
                        this.clone(),
                        move |window| {
                            window.selected = id.clone();
                            window.pane = Pane::Conversation;
                            window.search_open = false;
                        },
                    )
                })
                .collect()
        })
        .child(
            div()
                .text_xs()
                .text_color(colors.fg_muted)
                .child("Only readable projections are indexed. Searching does not import history."),
        )
}

fn capability_body(
    catalog: &Catalog,
    colors: &ely_gpui_component::theme::Palette,
) -> impl IntoElement {
    div()
        .id("capability-rows")
        .flex()
        .flex_col()
        .gap_2()
        .max_h(px(420.0))
        .overflow_y_scroll()
        .children(catalog.capability_map.iter().map(|row| {
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .pb_2()
                .border_b_1()
                .border_color(colors.border)
                .child(
                    div()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(row.capability.clone()),
                )
                .child(div().text_sm().child(row.basis.clone()))
                .child(
                    div()
                        .text_sm()
                        .text_color(colors.fg_muted)
                        .child(row.data.clone()),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(colors.fg_muted)
                        .child(format!("Missing: {}", row.missing)),
                )
                .into_any_element()
        }))
}

fn session_group(
    label: &'static str,
    sessions: Vec<&Session>,
    selected: &str,
    this: Entity<SessionsWindow>,
    colors: &ely_gpui_component::theme::Palette,
) -> gpui::AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .mb_2()
        .child(
            div()
                .px_2()
                .py_1()
                .text_xs()
                .text_color(colors.fg_muted)
                .child(label),
        )
        .children(sessions.into_iter().map(|session| {
            let id = session.id.clone();
            let active = session.id == selected;
            div()
                .id(SharedString::from(format!("session-{id}")))
                .px_2()
                .py_2()
                .rounded(px(8.0))
                .cursor_pointer()
                .when(active, |row| row.bg(colors.selection))
                .hover(|row| row.bg(colors.hover))
                .on_click({
                    let this = this.clone();
                    move |_, _, cx| {
                        let id = id.clone();
                        this.update(cx, |window, cx| {
                            window.selected = id;
                            window.drawer = Drawer::None;
                            cx.notify();
                        });
                    }
                })
                .child(
                    div()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(session.title.clone()),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .mt_1()
                        .child(Badge::new(session.agent.clone()).tone(agent_tone(&session.agent)))
                        .child(
                            div()
                                .text_xs()
                                .text_color(colors.fg_muted)
                                .child(clock(&session.start).to_string()),
                        )
                        .child(Badge::new(session.running_label.clone()).dot()),
                )
                .when(!session.summary.is_empty(), |row| {
                    row.child(
                        div()
                            .text_sm()
                            .text_color(colors.fg_muted)
                            .child(session.summary.clone()),
                    )
                })
                .into_any_element()
        }))
        .into_any_element()
}

fn status_card(
    question: &str,
    answer: &str,
    detail: &str,
    colors: &ely_gpui_component::theme::Palette,
) -> impl IntoElement {
    div()
        .w(px(220.0))
        .p_3()
        .rounded(px(8.0))
        .bg(colors.surface)
        .border_1()
        .border_color(colors.border)
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(colors.fg_muted)
                .child(question.to_string()),
        )
        .child(
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(answer.to_string()),
        )
        .child(
            div()
                .text_sm()
                .text_color(colors.fg_muted)
                .child(detail.to_string()),
        )
}

fn quote_card(
    title: &str,
    body: &str,
    this: Entity<SessionsWindow>,
    colors: &ely_gpui_component::theme::Palette,
) -> impl IntoElement {
    div()
        .flex_1()
        .p_3()
        .rounded(px(8.0))
        .bg(colors.surface)
        .border_1()
        .border_color(colors.border)
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .child(
                    div()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(title.to_string()),
                )
                .child(div().flex_1())
                .child(text_button(
                    format!("goto-{title}"),
                    "Conversation",
                    ButtonVariant::Ghost,
                    this,
                    |window| window.pane = Pane::Conversation,
                )),
        )
        .child(Paragraph::new(plain(body)))
}

fn section(
    title: &str,
    body: gpui::AnyElement,
    colors: &ely_gpui_component::theme::Palette,
) -> impl IntoElement {
    div()
        .p_3()
        .rounded(px(8.0))
        .bg(colors.surface)
        .border_1()
        .border_color(colors.border)
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title.to_string()),
        )
        .child(body)
}

fn icon_button(
    id: &'static str,
    label: &'static str,
    this: Entity<SessionsWindow>,
    apply: impl Fn(&mut SessionsWindow) + 'static,
) -> impl IntoElement {
    text_button(id, label, ButtonVariant::Ghost, this, apply)
}

fn text_button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    variant: ButtonVariant,
    this: Entity<SessionsWindow>,
    apply: impl Fn(&mut SessionsWindow) + 'static,
) -> impl IntoElement {
    Button::new(id, label)
        .variant(variant)
        .size(ControlSize::Sm)
        .on_click(move |_, _, cx| {
            this.update(cx, |window, cx| {
                apply(window);
                cx.notify();
            });
        })
}

fn row_button(
    id: String,
    label: String,
    this: Entity<SessionsWindow>,
    apply: impl Fn(&mut SessionsWindow) + 'static,
) -> gpui::AnyElement {
    Button::new(SharedString::from(id), SharedString::from(label))
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Sm)
        .on_click(move |_, _, cx| {
            this.update(cx, |window, cx| {
                apply(window);
                cx.notify();
            });
        })
        .into_any_element()
}

fn drawer_button(
    id: &'static str,
    label: &'static str,
    drawer: Drawer,
    current: Drawer,
    this: Entity<SessionsWindow>,
) -> impl IntoElement {
    let variant = if current == drawer {
        ButtonVariant::Subtle
    } else {
        ButtonVariant::Ghost
    };
    text_button(id, label, variant, this, move |window| {
        window.drawer = if window.drawer == drawer {
            Drawer::None
        } else {
            drawer
        };
    })
}

fn toggle(window: &mut SessionsWindow, key: String) {
    if !window.expanded.insert(key.clone()) {
        window.expanded.remove(&key);
    }
}

fn visible_sessions<'a>(catalog: &'a Catalog, window: &SessionsWindow) -> Vec<&'a Session> {
    catalog
        .sessions
        .iter()
        .filter(|session| {
            (window.worktree == "all" || session.worktree == window.worktree)
                && (window.agents.is_empty()
                    || window
                        .agents
                        .iter()
                        .any(|agent| session.agent.starts_with(agent)))
                && (window.dates.is_empty()
                    || window
                        .dates
                        .iter()
                        .any(|date| date_bucket(&session.start, &catalog.today) == date))
                && (window.statuses.is_empty()
                    || window
                        .statuses
                        .iter()
                        .any(|status| status == &session.capture))
        })
        .collect()
}

fn grouped<'a>(
    catalog: &Catalog,
    visible: &[&'a Session],
) -> Vec<(&'static str, Vec<&'a Session>)> {
    let mut groups: Vec<(&'static str, Vec<&Session>)> = Vec::new();
    for label in ["Today", "Yesterday", "Earlier"] {
        let bucket = label.to_ascii_lowercase();
        let items: Vec<&Session> = visible
            .iter()
            .copied()
            .filter(|session| date_bucket(&session.start, &catalog.today) == bucket)
            .collect();
        if !items.is_empty() {
            groups.push((label, items));
        }
    }
    groups
}

fn worktree_choices(catalog: &Catalog) -> Vec<Choice> {
    let mut choices = vec![Choice::new("all", "All worktrees")];
    let mut seen = HashSet::new();
    for session in &catalog.sessions {
        if seen.insert(session.worktree.clone()) {
            choices.push(Choice::new(
                session.worktree.clone(),
                session.worktree.clone(),
            ));
        }
    }
    choices
}

fn filter_choices() -> Vec<Choice> {
    [
        ("agent:Claude Code", "Claude Code"),
        ("agent:Codex", "Codex"),
        ("agent:OpenCode", "OpenCode"),
        ("date:today", "Today"),
        ("date:yesterday", "Yesterday"),
        ("date:earlier", "Earlier"),
        ("status:complete", "Capture complete"),
        ("status:finalize_pending", "Finalize pending"),
        ("status:oc_partial", "Coverage partial"),
        ("status:partial", "Incomplete"),
        ("status:metadata_only", "Metadata only"),
    ]
    .into_iter()
    .map(|(value, label)| Choice::new(value, label))
    .collect()
}

fn selected_filters(window: &SessionsWindow) -> Vec<SharedString> {
    window
        .agents
        .iter()
        .map(|agent| SharedString::from(format!("agent:{agent}")))
        .chain(
            window
                .dates
                .iter()
                .map(|date| SharedString::from(format!("date:{date}"))),
        )
        .chain(
            window
                .statuses
                .iter()
                .map(|status| SharedString::from(format!("status:{status}"))),
        )
        .collect()
}

fn take_prefix(values: &[String], prefix: &str) -> Vec<String> {
    values
        .iter()
        .filter_map(|value| value.strip_prefix(prefix).map(str::to_string))
        .collect()
}

struct Hit {
    id: String,
    title: String,
    label: String,
}

fn search_hits(catalog: &Catalog, window: &SessionsWindow, query: &str) -> Vec<Hit> {
    if query.is_empty() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    for session in catalog
        .sessions
        .iter()
        .filter(|session| window.worktree == "all" || session.worktree == window.worktree)
    {
        push_hit(&mut hits, session, "Title", &session.title, query);
        push_hit(&mut hits, session, "Summary", &session.summary, query);
        for turn in &session.turns {
            push_hit(&mut hits, session, "User", &turn.user, query);
            for event in &turn.events {
                match event {
                    Event::Reasoning { text, .. } => {
                        push_hit(&mut hits, session, "Reasoning", text, query)
                    }
                    Event::Assistant { text, .. } => {
                        push_hit(&mut hits, session, "Reply", text, query)
                    }
                    Event::Tool { name, input, .. } => {
                        push_hit(&mut hits, session, name, input, query);
                    }
                    Event::Note { label, text } => push_hit(&mut hits, session, label, text, query),
                }
            }
        }
        if hits.len() >= 12 {
            break;
        }
    }
    hits.truncate(12);
    hits
}

fn push_hit(hits: &mut Vec<Hit>, session: &Session, label: &str, text: &str, query: &str) {
    if hits.len() >= 12 || text.is_empty() || !text.to_lowercase().contains(query) {
        return;
    }
    hits.push(Hit {
        id: session.id.clone(),
        title: session.title.clone(),
        label: label.to_string(),
    });
}

fn agent_tone(agent: &str) -> Tone {
    if agent.starts_with("Claude") {
        Tone::Warning
    } else if agent.starts_with("Codex") {
        Tone::Success
    } else {
        Tone::Accent
    }
}

fn capture_tone(capture: &str) -> Tone {
    match capture {
        "complete" => Tone::Success,
        "finalize_pending" | "oc_partial" | "partial" => Tone::Warning,
        "metadata_only" => Tone::Neutral,
        _ => Tone::Neutral,
    }
}

fn capture_detail(session: &Session) -> &'static str {
    match session.capture.as_str() {
        "complete" => "Content captured and archived",
        "finalize_pending" => "Session stopped, finalize pending",
        "partial" => "Some turns or blocks are missing",
        "metadata_only" => "Content was not exported",
        "oc_partial" => "Checkpoints written, coverage partial",
        _ => "Capture state recorded",
    }
}

fn plain_status(status: &str) -> String {
    status.replace('_', " ")
}

fn tool_status(status: &str) -> &'static str {
    if status == "ok" {
        "Succeeded"
    } else {
        "Recorded"
    }
}

fn plain(text: &str) -> String {
    text.replace("**", "")
}
