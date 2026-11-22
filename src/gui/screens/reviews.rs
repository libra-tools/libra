//! Multi-reviewer runs, findings, and launch form.
//!
//! Rows come from item-level bounded reads of real run directories; corrupt,
//! oversized or unknown-kind runs stay visible as item-level states.
use ely_gpui_component::theme::Palette;
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, px,
};

use super::super::{
    data::{RUN_ROWS_PER_KIND, RunsRead, SourceState, runs_matches_filter, visible_on},
    desktop::Desktop,
    widgets::{banner, card, column, label, mono, muted, note, row, title},
};

pub fn render(s: &Desktop, cx: &Context<Desktop>, p: &Palette) -> AnyElement {
    let empty = RunsRead {
        state: SourceState::Stale,
        rows: Vec::new(),
        truncated: false,
    };
    let read = s
        .snapshot
        .as_ref()
        .map(|snapshot| &snapshot.runs)
        .unwrap_or(&empty);
    let filtered: Vec<_> = read
        .rows
        .iter()
        .filter(|run| visible_on(run, "review") && runs_matches_filter(run, "", &s.runs_query))
        .collect();
    let selected = filtered
        .get(s.selected)
        .copied()
        .or_else(|| filtered.first().copied());
    let status = if read.state.is_available() {
        format!(
            "{} review run(s){} · newest {RUN_ROWS_PER_KIND} per kind",
            filtered.len(),
            if read.truncated {
                " · truncated subset"
            } else {
                ""
            }
        )
    } else {
        format!("Run source is {}", read.state.label())
    };
    let subset = read
        .truncated
        .then(|| "Only the inspected subset is shown.".to_string());
    row()
        .size_full()
        .items_stretch()
        .gap_0()
        .child(
            column()
                .id("review-runs")
                .overflow_y_scroll()
                .w(px(245.0))
                .flex_none()
                .p_4()
                .child(title("Review runs"))
                .child(s.runs_input.clone())
                .child(banner(&status, p))
                .when_some(subset.clone(), |body, note_text| {
                    body.child(muted(note_text, p))
                })
                .children(filtered.iter().enumerate().map(|(index, run)| {
                    card(p)
                        .id(("review-run", index))
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
                                .child(mono(&run.status))
                                .child(div().flex_1())
                                .child(muted(clock(run.started_at), p)),
                        )
                        .child(muted(
                            format!("{} · {}", run.kind, short_id(&run.run_id)),
                            p,
                        ))
                        .child(muted(run.summary.clone(), p))
                })),
        )
        .child(
            column()
                .id("review-detail")
                .overflow_y_scroll()
                .flex_1()
                .p_6()
                .bg(p.surface)
                .when_some(selected.cloned(), |body, run| {
                    body.child(mono(run.run_id.clone()))
                        .child(title(if run.summary.is_empty() {
                            "Run summary unavailable".to_string()
                        } else {
                            run.summary.clone()
                        }))
                        .child(muted(
                            format!(
                                "{} · {} · {} reviewer(s) · {}",
                                run.kind,
                                run.status,
                                run.reviewers.len(),
                                run.item_state
                            ),
                            p,
                        ))
                        .child(muted(
                            match run.rounds {
                                Some(rounds) => format!("{rounds} completed round(s)"),
                                None => "rounds not recorded".to_string(),
                            },
                            p,
                        ))
                        .child(label("CONCLUSION", p))
                        .child(muted(
                            match run.conclusion_state.as_str() {
                                "recorded" => run.conclusion.clone(),
                                "oversized" => "Conclusion oversized (not shown)".to_string(),
                                "unavailable" => "Conclusion unavailable".to_string(),
                                _ => "No conclusion recorded".to_string(),
                            },
                            p,
                        ))
                        .child(label("REVIEWERS", p))
                        .child(muted(
                            if run.reviewers.is_empty() {
                                "None recorded".to_string()
                            } else {
                                run.reviewers.join(", ")
                            },
                            p,
                        ))
                        .when(run.item_state != "available", |body| {
                            body.child(note(format!("This run is shown as {}.", run.item_state), p))
                        })
                })
                .when(selected.is_none(), |body| {
                    body.child(muted("No review runs match the current search.", p))
                }),
        )
        .into_any_element()
}

fn clock(ts: i64) -> String {
    if ts <= 0 {
        return "—".to_string();
    }
    chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0)
        .map(|value| value.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "—".to_string())
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}
