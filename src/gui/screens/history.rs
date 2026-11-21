//! History graph and captured creation-history inspector.

use ely_gpui_component::{
    buttons::{Button, ButtonVariant},
    theme::{ActiveTheme, ControlSize, Mode, Palette, Theme},
};
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, img, prelude::FluentBuilder, px, rgb,
};

use super::super::{
    data::{AttributionFilter, HistoryRow},
    desktop::{Desktop, Page},
    widgets::{
        action, agent, badge, card, code, column, label, mono, muted, nav, note, row, table, tabs,
        title,
    },
};

#[derive(Clone)]
struct RowView {
    subject: String,
    author: String,
    short: String,
    oid: String,
    parent_oids: Vec<String>,
    age: String,
    branch_label: String,
    merge: bool,
    parents: usize,
    sessions: i64,
    checkpoints: i64,
}

impl RowView {
    fn empty() -> Self {
        Self {
            subject: "No matching commits".to_string(),
            author: String::new(),
            short: String::new(),
            oid: String::new(),
            parent_oids: Vec::new(),
            age: String::new(),
            branch_label: String::new(),
            merge: false,
            parents: 0,
            sessions: 0,
            checkpoints: 0,
        }
    }
}

fn visible_history(rows: &[HistoryRow], query: &str, filter: usize) -> Vec<(usize, RowView)> {
    let query = query.trim().to_lowercase();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as usize)
        .unwrap_or(0);
    rows.iter()
        .enumerate()
        .filter(|(_, c)| {
            (c.subject.to_lowercase().contains(&query)
                || c.author.to_lowercase().contains(&query)
                || c.short.contains(&query)
                || c.oid.contains(&query))
                && {
                    let selected_filter = match filter {
                        1 => AttributionFilter::Human,
                        2 => AttributionFilter::AgentAuthored,
                        3 => AttributionFilter::HasCheckpoint,
                        _ => AttributionFilter::All,
                    };
                    c.matches(selected_filter)
                }
        })
        .map(|(i, c)| {
            (
                i,
                RowView {
                    subject: c.subject.clone(),
                    author: c.author.clone(),
                    short: c.short.clone(),
                    oid: c.oid.clone(),
                    parent_oids: c.parent_oids.clone(),
                    age: super::super::data::age_label(c.timestamp, now),
                    branch_label: if c.parents > 1 {
                        "merge".to_string()
                    } else {
                        String::new()
                    },
                    merge: c.parents > 1,
                    parents: c.parents,
                    sessions: c.sessions,
                    checkpoints: c.checkpoints,
                },
            )
        })
        .collect()
}

/// A connector exists when ANY newer visible row lists the commit at
/// `position` as a parent (covers merge branches and gaps from filtering).
fn connector_between(visible: &[(usize, RowView)], position: usize) -> bool {
    let Some((_, current)) = visible.get(position) else {
        return false;
    };
    visible
        .iter()
        .take(position)
        .any(|(_, prev)| prev.parent_oids.iter().any(|parent| parent == &current.oid))
}

pub fn render(s: &Desktop, cx: &Context<Desktop>, p: &Palette) -> AnyElement {
    let dark = cx.theme().is_dark();
    let repository_label = format!(
        "▱  {}  ⌄",
        s.snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.context.as_ref())
            .map(|context| context.worktree_label.clone())
            .unwrap_or_else(|| "no repository".to_string())
    );
    let branch_label = format!(
        "⑂ {}   ↑0 ↓0",
        s.snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.context.as_ref())
            .map(|context| context.branch.clone())
            .unwrap_or_else(|| "-".to_string())
    );
    let rows = s
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.history.as_slice())
        .unwrap_or(&[]);
    let visible = visible_history(rows, &s.history_query, s.filter);
    let selected_index = if visible.iter().any(|(i, _)| *i == s.selected) {
        Some(s.selected)
    } else {
        visible.first().map(|(i, _)| *i)
    };
    let selected_view = selected_index.and_then(|index| {
        visible
            .iter()
            .find(|(i, _)| *i == index)
            .map(|(_, view)| view.clone())
    });
    let toolbar = row()
        .px_4()
        .py_2()
        .border_b_1()
        .border_color(p.border)
        .bg(p.surface)
        .child(img("libra/logo.svg").size(px(28.0)))
        .child(div().font_weight(FontWeight::BOLD).child("Libra"))
        .child(action("repository", repository_label.clone(), false, cx))
        .child(mono(&branch_label))
        .child(action("fetch", "Fetch", false, cx))
        .child(action("pull", "Pull", false, cx))
        .child(action("push", "Push", false, cx))
        .child(action("undo", "Undo", false, cx))
        .child(
            div().flex_1().child(
                ely_gpui_component::forms::SearchInput::new("history-search", &s.history_input)
                    .size(ControlSize::Sm),
            ),
        )
        .child(
            Button::new("history-theme", if dark { "Light" } else { "Dark" })
                .size(ControlSize::Sm)
                .variant(ButtonVariant::Ghost)
                .on_click(move |_, _, cx| {
                    Theme::set_mode_now(if dark { Mode::Light } else { Mode::Dark }, cx)
                }),
        );
    let sidebar = column()
        .id("repository-sidebar")
        .overflow_y_scroll()
        .w(px(220.0))
        .flex_none()
        .p_3()
        .bg(p.bg)
        .child(label("WORKSPACE · demo", p))
        .children(
            [
                Page::Lanes,
                Page::History,
                Page::Sessions,
                Page::Review,
                Page::Operations,
                Page::Memory,
                Page::Search,
            ]
            .into_iter()
            .map(|page| nav(page, cx)),
        )
        .child(label("BRANCHES", p))
        .children(
            [
                ("main", 0x2563eb),
                ("feat/search", 0x0f766e),
                ("agent/rate-limit", 0xc2410c),
                ("fix/limiter-config", 0x94a3b8),
            ]
            .into_iter()
            .map(|(branch, color)| {
                row()
                    .py_1()
                    .child(div().size(px(7.0)).rounded_full().bg(rgb(color)))
                    .child(mono(branch))
            }),
        )
        .child(label("AGENTS", p))
        .children(["Claude Code", "Codex", "OpenCode"].iter().map(|name| {
            row()
                .child(agent(name))
                .child(*name)
                .child(div().flex_1())
                .child(muted(
                    if *name == "OpenCode" {
                        "Trust…"
                    } else {
                        "capturing"
                    },
                    p,
                ))
        }))
        .child(note(
            "OpenCode RPC adapter is closed until trusted. Nothing is captured.",
            p,
        ))
        .child(label("TRACE REFS", p))
        .child(mono("♙ refs/trace/*   local only"))
        .child(action("trace-push", "Push trace refs…", false, cx))
        .child(nav(Page::Settings, cx))
        .child(nav(Page::About, cx));
    let filters = row()
        .flex_wrap()
        .px_4()
        .py_3()
        .child(title("History"))
        .children(
            ["All", "Human", "Agent-authored", "Has checkpoint"]
                .into_iter()
                .enumerate()
                .map(|(i, text)| {
                    Button::new(("history-filter", i), text)
                        .size(ControlSize::Sm)
                        .variant(if s.filter == i {
                            ButtonVariant::Primary
                        } else {
                            ButtonVariant::Ghost
                        })
                        .on_click(cx.listener(move |s, _, _, cx| {
                            s.filter = i;
                            cx.notify();
                        }))
                }),
        );
    let list = column()
        .gap_0()
        .flex_1()
        .min_w(px(360.0))
        .bg(p.surface)
        .child(filters)
        .child(
            row()
                .px_4()
                .h(px(32.0))
                .border_y_1()
                .border_color(p.border)
                .child(label("GRAPH", p).w(px(44.0)))
                .child(label("MESSAGE", p).flex_1())
                .child(label("AUTHOR", p).w(px(150.0)))
                .child(label("COMMIT", p).w(px(70.0))),
        )
        .child(
            column()
                .id("commit-list")
                .gap_0()
                .overflow_y_scroll()
                .flex_1()
                .children(visible.iter().enumerate().map(|(position, (i, c))| {
                    let index = *i;
                    let is_branch = c.merge;
                    // Newest-first rows: any earlier (newer) visible row that
                    // lists this (older) commit as a parent links it.
                    let linked = connector_between(&visible, position);
                    row()
                        .id(("commit", index))
                        .gap_2()
                        .h(px(48.0))
                        .px_3()
                        .flex_none()
                        .border_b_1()
                        .border_color(p.border)
                        .bg(if selected_index == Some(index) {
                            p.selection
                        } else {
                            p.surface
                        })
                        .cursor_pointer()
                        .hover(|v| v.bg(p.hover))
                        .on_click(cx.listener(move |s, _, _, cx| {
                            s.selected = index;
                            cx.notify();
                        }))
                        .child(
                            div()
                                .w(px(44.0))
                                .h_full()
                                .relative()
                                .when(linked, |d| {
                                    d.child(
                                        div()
                                            .absolute()
                                            .left(px(10.0))
                                            .w(px(2.0))
                                            .h_full()
                                            .bg(p.accent),
                                    )
                                })
                                .when(c.parents > 1, |d| {
                                    d.child(
                                        div()
                                            .absolute()
                                            .left(px(30.0))
                                            .w(px(2.0))
                                            .h_full()
                                            .bg(rgb(0x0f766e)),
                                    )
                                })
                                .child(
                                    div()
                                        .absolute()
                                        .left(px(if is_branch { 26.0 } else { 6.0 }))
                                        .top(px(19.0))
                                        .size(px(10.0))
                                        .rounded_full()
                                        .bg(if is_branch {
                                            rgb(0x0f766e).into()
                                        } else {
                                            p.accent
                                        }),
                                ),
                        )
                        .child(
                            column()
                                .flex_1()
                                .gap_1()
                                .overflow_hidden()
                                .when(!c.branch_label.is_empty(), |d| {
                                    d.child(muted(&c.branch_label, p).text_color(p.accent))
                                })
                                .child(div().truncate().child(c.subject.clone())),
                        )
                        .child(
                            row()
                                .w(px(150.0))
                                .flex_none()
                                .child(agent(&c.author))
                                .child(div().truncate().child(c.author.clone())),
                        )
                        .child(
                            column()
                                .gap_1()
                                .w(px(70.0))
                                .flex_none()
                                .child(mono(&c.short))
                                .child(muted(&c.age, p)),
                        )
                })),
        );
    let has_selection = selected_view.is_some();
    let c = selected_view.unwrap_or_else(RowView::empty);
    let inspector = column()
        .w(px(365.0))
        .flex_none()
        .h_full()
        .gap_0()
        .border_l_1()
        .border_color(p.border)
        .bg(p.surface)
        .child(
            column()
                .p_4()
                .border_b_1()
                .border_color(p.border)
                .when(has_selection, |d| {
                    d.child(muted(
                        format!("{} · {} · {} ago", c.short, c.author, c.age),
                        p,
                    ))
                })
                .child(
                    div()
                        .text_size(px(16.0))
                        .font_weight(FontWeight::BOLD)
                        .child(c.subject.clone()),
                )
                .when(has_selection, |d| {
                    d.child(muted(
                        "Inspector tabs are sample content; commit metadata above is real.",
                        p,
                    ))
                    .child(tabs(
                        &["Creation history", "Changes · 3", "Raw"],
                        s.tab,
                        cx,
                    ))
                }),
        )
        .child(
            column()
                .id("creation-inspector")
                .overflow_y_scroll()
                .flex_1()
                .p_4()
                .children(if has_selection {
                    inspector_body(s, cx, p, &c)
                } else {
                    vec![
                        muted(
                            match s.snapshot.as_ref() {
                                None => "Loading history…",
                                Some(snapshot) if snapshot.history_error.is_some() => snapshot
                                    .history_error
                                    .as_deref()
                                    .unwrap_or("History is unavailable."),
                                Some(snapshot) if !snapshot.state.is_available() => {
                                    "The repository has no readable history."
                                }
                                Some(snapshot)
                                    if snapshot.state.is_available()
                                        && snapshot.history.is_empty() =>
                                {
                                    "No commits yet."
                                }
                                _ => "Clear the search or choose another attribution filter.",
                            },
                            p,
                        )
                        .into_any_element(),
                    ]
                }),
        );
    column()
        .gap_0()
        .size_full()
        .child(toolbar)
        .child(
            row()
                .gap_0()
                .items_stretch()
                .flex_1()
                .min_h(px(0.0))
                .child(sidebar)
                .child(list)
                .child(inspector),
        )
        .into_any_element()
}

fn inspector_body(s: &Desktop, cx: &Context<Desktop>, p: &Palette, c: &RowView) -> Vec<AnyElement> {
    if s.tab == 1 {
        return vec![
            table(
                &["File", "Change"],
                &[
                    vec!["src/gateway/limiter.rs", "+142"],
                    vec!["src/gateway/mod.rs", "+9 −2"],
                    vec!["config/gateway.toml", "+6"],
                ],
                p,
            )
            .into_any_element(),
        ];
    }
    if s.tab == 2 {
        return vec![
            code(format!(
                "commit {}\nauthor {}\n\n{}\n\nDemonstration record",
                c.short, c.author, c.subject
            ))
            .into_any_element(),
        ];
    }
    if c.sessions == 0 {
        return vec![
            title("No agent capture for this commit").into_any_element(),
            muted(
                "Authored by hand. There is no prompt, session or checkpoint to show.",
                p,
            )
            .into_any_element(),
        ];
    }
    vec![
        row()
            .flex_wrap()
            .child(badge("Prompt", 0xc2410c, 0xfff1e8))
            .child("→")
            .child(badge("Session", 0xc2410c, 0xfff1e8))
            .child("→")
            .child(badge("Checkpoint", 0x334155, 0xf1f5f9))
            .child("→")
            .child(badge("Commit", 0xffffff, 0x111827))
            .into_any_element(),
        label("PROMPT", p).into_any_element(),
        note(
            if c.author == "Claude Code" {
                "Add a token-bucket rate limiter to the gateway. 100 requests per minute per API key, configurable in \
                 gateway.toml. Keep the existing middleware order."
            } else {
                "Captured demonstration prompt associated with this agent's commit."
            },
            p,
        )
        .into_any_element(),
        label("SESSION", p).into_any_element(),
        row()
            .child(agent(&c.author))
            .child(
                column()
                    .gap_1()
                    .child(c.author.clone())
                    .child(muted(
                        format!(
                            "{} session(s) · {} checkpoint(s) linked",
                            c.sessions, c.checkpoints
                        ),
                        p,
                    )),
            )
            .into_any_element(),
        label("TOOL EVENTS", p).into_any_element(),
        table(
            &["Tool", "Source", "Result"],
            &[
                vec!["READ", "gateway/mod.rs", ""],
                vec!["EDIT", "gateway/limiter.rs", "+142"],
                vec!["EDIT", "gateway.toml", "+6"],
                vec!["BASH", "cargo test", "exit 0"],
            ],
            p,
        )
        .into_any_element(),
        card(p)
            .child(mono(format!("{} checkpoint(s)", c.checkpoints)))
            .child(muted("3 files captured before commit", p))
            .child(action("export", "Export redacted…", true, cx))
            .into_any_element(),
        muted(
            "Session, prompt and tool content is demonstration until capture details are connected. Libra does not infer plans, test results or approvals.",
            p,
        )
        .into_any_element(),
    ]
}

#[cfg(test)]
mod tests {
    use super::{super::super::data::HistoryRow, RowView, connector_between, visible_history};

    fn row(
        subject: &str,
        author: &str,
        short: &str,
        sessions: i64,
        checkpoints: i64,
    ) -> HistoryRow {
        HistoryRow {
            oid: format!("{short}0000000000000000000000000000000000"),
            short: short.to_string(),
            author: author.to_string(),
            subject: subject.to_string(),
            timestamp: 0,
            parents: 1,
            parent_oids: vec!["parent".to_string()],
            sessions,
            checkpoints,
        }
    }

    #[test]
    fn uid02_search_and_filter() {
        let rows = vec![
            row("Add limiter", "Claude Code", "9f2c4e1", 1, 0),
            row("Tighten config", "eli", "41be07a", 0, 0),
            row("feat(search): index", "Codex", "c07d5aa", 1, 2),
        ];
        let mut merged = row("merge branch", "eli", "deadbee", 0, 0);
        merged.parents = 2;
        let mut rows = rows;
        rows.push(merged);
        assert_eq!(visible_history(&rows, "  CODEX  ", 2).len(), 1);
        assert!(visible_history(&rows, "codex", 1).is_empty());
        assert_eq!(visible_history(&rows, "C07D5AA", 0)[0].1.short, "c07d5aa");
        assert_eq!(visible_history(&rows, "", 1).len(), 2);
        assert!(visible_history(&rows, "no matching record", 0).is_empty());
        assert_eq!(
            visible_history(&rows, "", 3)[0].1.short,
            "c07d5aa",
            "Has checkpoint keeps only checkpoint-linked rows"
        );
        assert_eq!(visible_history(&rows, "", 0).len(), 4);
        let merge_row = visible_history(&rows, "merge branch", 0);
        assert_eq!(merge_row[0].1.branch_label, "merge");

        // Connector predicate: an intervening row keeps the merge edge
        // (non-adjacent parent), while unrelated rows are not connected.
        let view = |oid: &str, parents: &[&str]| RowView {
            subject: oid.to_string(),
            author: String::new(),
            short: oid.to_string(),
            oid: oid.to_string(),
            parent_oids: parents.iter().map(|p| p.to_string()).collect(),
            age: String::new(),
            branch_label: String::new(),
            merge: parents.len() > 1,
            parents: parents.len(),
            sessions: 0,
            checkpoints: 0,
        };
        let chain = vec![
            (0, view("m", &["a", "side"])),
            (1, view("x", &["elsewhere"])),
            (2, view("a", &["root"])),
        ];
        assert!(
            connector_between(&chain, 2),
            "an intervening row still links the non-adjacent merge parent"
        );
        assert!(
            !connector_between(&chain, 0),
            "no connector above the newest row"
        );
        assert!(
            !connector_between(&chain, 1),
            "an unrelated intervening row is not connected"
        );
    }
}
