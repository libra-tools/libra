//! Memory episodes and stale projection display.
//!
//! Rows come from the bounded, read-only memory projection read by the window
//! worker; a stale projection shows zero rows with a rebuild hint.
use ely_gpui_component::{
    forms::{Choice, ChoiceChips},
    theme::Palette,
};
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, px,
};

use super::super::{
    data::{MemoryRead, SourceState, memory_matches_filter},
    desktop::Desktop,
    widgets::{banner, card, column, label, mono, muted, note, row, title},
};

pub fn render(s: &Desktop, cx: &Context<Desktop>, p: &Palette) -> AnyElement {
    let empty = MemoryRead {
        state: SourceState::Stale,
        rows: Vec::new(),
    };
    let read = s
        .snapshot
        .as_ref()
        .map(|snapshot| &snapshot.memory)
        .unwrap_or(&empty);
    let filtered: Vec<_> = read
        .rows
        .iter()
        .filter(|row| memory_matches_filter(row, &s.memory_source, &s.memory_query))
        .collect();
    let mut sources = vec![Choice::new("", "All sources")];
    let mut seen = std::collections::HashSet::new();
    for row in &read.rows {
        if seen.insert(row.source_kind.clone()) {
            sources.push(Choice::new(
                row.source_kind.clone(),
                row.source_kind.clone(),
            ));
        }
    }
    let selected = filtered
        .get(s.selected)
        .copied()
        .or_else(|| filtered.first().copied());
    let status = match &read.state {
        SourceState::Stale => {
            "Memory projection is stale; rebuild it with `libra memory rebuild`.".to_string()
        }
        SourceState::Absent => "No memory projection yet.".to_string(),
        state if !state.is_available() => format!(
            "Memory source is {}: {} ({})",
            state.label(),
            state.hint().unwrap_or("retry shortly"),
            s.snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.memory_error.clone())
                .unwrap_or_else(|| "no detail".to_string())
        ),
        _ => format!("{} episode(s) in the current window", read.rows.len()),
    };
    column()
        .size_full()
        .gap_0()
        .child(banner(&status, p))
        .child(
            row()
                .items_stretch()
                .gap_0()
                .flex_1()
                .min_h(px(0.0))
                .child(
                    column()
                        .id("memory-list")
                        .overflow_y_scroll()
                        .w(px(330.0))
                        .p_4()
                        .child(title("Memory"))
                        .child(s.memory_input.clone())
                        .child(
                            ChoiceChips::new("memory-source", sources)
                                .selected(if s.memory_source.is_empty() {
                                    Vec::new()
                                } else {
                                    vec![SharedString::from(s.memory_source.clone())]
                                })
                                .on_change(cx.listener(|s, values: &[SharedString], _, cx| {
                                    s.memory_source = values
                                        .first()
                                        .map(|value| value.to_string())
                                        .unwrap_or_default();
                                    s.selected = 0;
                                    cx.notify();
                                })),
                        )
                        .children(filtered.iter().enumerate().map(|(index, entry)| {
                            card(p)
                                .id(("memory", index))
                                .cursor_pointer()
                                .bg(if s.selected == index {
                                    p.selection
                                } else {
                                    p.surface
                                })
                                .on_click(cx.listener(move |s, _, _, cx| {
                                    s.selected = index;
                                    cx.notify();
                                }))
                                .child(
                                    row()
                                        .child(mono(&entry.source_kind))
                                        .child(div().flex_1())
                                        .child(muted(clock(entry.ended_at), p)),
                                )
                                .child(muted(short_id(&entry.episode_id), p))
                                .child(muted(entry.title.clone(), p))
                        })),
                )
                .child(
                    column()
                        .id("memory-detail")
                        .overflow_y_scroll()
                        .flex_1()
                        .p_6()
                        .bg(p.surface)
                        .when_some(selected.cloned(), |body, row| {
                            body.child(mono(row.episode_id.clone()))
                                .child(title(row.title.clone()))
                                .child(muted(
                                    format!(
                                        "{} · {} · by {} · {} evidence · {} paths",
                                        row.source_kind,
                                        row.outcome,
                                        row.actor.clone().unwrap_or_else(|| "unknown".into()),
                                        row.evidence_count,
                                        row.path_count
                                    ),
                                    p,
                                ))
                                .child(label("SUMMARY", p))
                                .child(note(row.body.clone(), p))
                        })
                        .when(selected.is_none(), |body| {
                            body.child(muted("No episodes match the current search.", p))
                        }),
                ),
        )
        .into_any_element()
}

fn clock(ts: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0)
        .map(|value| value.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "—".to_string())
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}
