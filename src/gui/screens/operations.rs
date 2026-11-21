//! Operation log, ref transitions and restore preview.
//!
//! Rows come from the real Operation v2 id snapshot read by the window worker;
//! paging slices that same snapshot so pages never repeat or lose a row.
use ely_gpui_component::{
    buttons::{Button, ButtonVariant},
    forms::{Choice, ChoiceChips},
    theme::Palette,
};
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, px,
};

use super::super::{
    data::{OperationsRead, SourceState, filtered_page},
    desktop::Desktop,
    widgets::{banner, card, column, mono, muted, note, row, title},
};

pub fn render(s: &Desktop, cx: &Context<Desktop>, p: &Palette) -> AnyElement {
    // Borrow the snapshot; never clone the (potentially 50k-row) projection.
    let empty = OperationsRead {
        state: SourceState::Stale,
        rows: Vec::new(),
        truncated: false,
    };
    let read = s
        .snapshot
        .as_ref()
        .map(|snapshot| &snapshot.operations)
        .unwrap_or(&empty);
    // Filter the stable snapshot FIRST, then paginate the matching sequence so
    // a match beyond the first page is reachable.
    let (filtered, page_count) =
        filtered_page(&read.rows, &s.ops_command, &s.ops_query, s.ops_page);

    let state_note = match &read.state {
        SourceState::Absent => "No operations recorded yet.".to_string(),
        state if !state.is_available() => {
            format!(
                "Operation source is {}: {}",
                state.label(),
                state.hint().unwrap_or("retry shortly")
            )
        }
        _ => format!(
            "{} operation row(s){} · page {} of {}",
            read.rows.len(),
            if read.truncated {
                " · truncated at 50k ids"
            } else {
                ""
            },
            s.ops_page + 1,
            page_count
        ),
    };
    let mut command_choices = vec![Choice::new("", "All commands")];
    let mut seen = std::collections::HashSet::new();
    for row in &read.rows {
        if seen.insert(row.command_name.clone()) {
            command_choices.push(Choice::new(
                row.command_name.clone(),
                row.command_name.clone(),
            ));
        }
    }
    let last_page = page_count.saturating_sub(1);
    let selected = filtered
        .get(s.selected)
        .copied()
        .or_else(|| filtered.first().copied());
    column()
        .size_full()
        .gap_0()
        .child(banner(&state_note, p))
        .child(
            row()
                .items_stretch()
                .gap_0()
                .flex_1()
                .min_h(px(0.0))
                .child(
                    column()
                        .id("op-list")
                        .overflow_y_scroll()
                        .w(px(330.0))
                        .p_4()
                        .child(title("Operation log"))
                        .child(s.ops_input.clone())
                        .child(
                            ChoiceChips::new("op-command", command_choices)
                                .selected(if s.ops_command.is_empty() {
                                    Vec::new()
                                } else {
                                    vec![SharedString::from(s.ops_command.clone())]
                                })
                                .on_change(cx.listener(|s, values: &[SharedString], _, cx| {
                                    s.ops_command = values
                                        .first()
                                        .map(|value| value.to_string())
                                        .unwrap_or_default();
                                    s.ops_page = 0;
                                    s.selected = 0;
                                    cx.notify();
                                })),
                        )
                        .children(filtered.iter().enumerate().map(|(i, op)| {
                            card(p)
                                .id(("op", i))
                                .cursor_pointer()
                                .bg(if s.selected == i {
                                    p.selection
                                } else {
                                    p.surface
                                })
                                .on_click(cx.listener(move |s, _, _, cx| {
                                    s.selected = i;
                                    cx.notify();
                                }))
                                .child(
                                    row()
                                        .child(mono(&op.command_name))
                                        .child(div().flex_1())
                                        .child(muted(clock(op.end_ts), p)),
                                )
                                .child(muted(format!("{} · {}", op.status, short_id(&op.op_id)), p))
                                .child(muted(truncate_summary(&op.description), p))
                        }))
                        .child(
                            row()
                                .gap_2()
                                .child(
                                    Button::new("op-prev", "Previous")
                                        .variant(ButtonVariant::Ghost)
                                        .on_click(cx.listener(|s, _, _, cx| {
                                            s.ops_page = s.ops_page.saturating_sub(1);
                                            s.selected = 0;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("op-next", "Next")
                                        .variant(ButtonVariant::Ghost)
                                        .on_click(cx.listener(move |s, _, _, cx| {
                                            s.ops_page = (s.ops_page + 1).min(last_page);
                                            s.selected = 0;
                                            cx.notify();
                                        })),
                                ),
                        ),
                )
                .child(
                    column()
                        .id("op-detail")
                        .overflow_y_scroll()
                        .flex_1()
                        .p_6()
                        .bg(p.surface)
                        .when_some(selected.cloned(), |body, op| {
                            body.child(mono(op.op_id.clone()))
                                .child(title(format!("libra {}", op.command_name)))
                                .child(muted(
                                    format!(
                                        "{} · {} · by {} · view {}",
                                        op.status,
                                        clock(op.end_ts),
                                        if op.actor.is_empty() {
                                            "unknown"
                                        } else {
                                            &op.actor
                                        },
                                        short_id(&op.view_id)
                                    ),
                                    p,
                                ))
                                .child(
                                    card(p)
                                        .child(title("Summary"))
                                        .child(note(op.description.clone(), p)),
                                )
                        })
                        .when(selected.is_none(), |body| {
                            body.child(muted("No operations match the current search.", p))
                        }),
                ),
        )
        .into_any_element()
}

fn clock(end_ts: Option<i64>) -> String {
    end_ts
        .and_then(|value| chrono::DateTime::<chrono::Utc>::from_timestamp(value, 0))
        .map(|value| value.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "—".to_string())
}

fn truncate_summary(summary: &str) -> String {
    summary.chars().take(80).collect()
}

fn short_id(value: &str) -> String {
    value.chars().take(8).collect()
}
