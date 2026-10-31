//! Search and command palette over bundled, readable demonstration content.
use ely_gpui_component::{forms::SearchInput, theme::Palette};
use gpui::{AnyElement, Context, IntoElement, ParentElement, div, prelude::*, px};

use super::super::{
    desktop::{Desktop, Page},
    model,
    widgets::{agent, badge, card, column, label, mono, muted, nav, row, tabs, title},
};
pub fn render(s: &Desktop, cx: &Context<Desktop>, p: &Palette) -> AnyElement {
    let query = s.query.to_lowercase();
    let pages = Page::ALL
        .iter()
        .copied()
        .filter(|page| page.label().to_lowercase().contains(&query))
        .collect::<Vec<_>>();
    column().size_full().items_center().p_6()
        .child(column().w_full().max_w(px(900.0)).flex_1().min_h(px(0.0)).rounded(px(12.0)).border_1().border_color(p.border).bg(p.surface).gap_0().overflow_hidden()
            .child(row().p_4().border_b_1().border_color(p.border).child(div().flex_1().child(SearchInput::new("global-search",&s.input))).child(badge("esc",0x5b6474,0xf4f5f7)))
            .child(div().px_4().py_3().child(tabs(&["Everything","Sessions","Commits","Memory","Commands"],s.tab,cx)))
            .child(column().id("search-results").overflow_y_scroll().flex_1().p_4()
                .when(s.tab==0||s.tab==1,|d|{
                    let sessions=model::catalog().as_ref().ok().map(|c| c.sessions.iter().filter(|r| format!("{} {}",r.title,r.summary).to_lowercase().contains(&query)).collect::<Vec<_>>()).unwrap_or_default();
                    d.child(label("SESSIONS · STORED, REDACTED TEXT",p)).children(sessions.iter().map(|r|card(p).child(row().child(agent(&r.agent)).child(div().flex_1().child(r.title.clone())).child(nav(Page::Sessions,cx))).child(muted(r.summary.clone(),p))))
                })
                .when(s.tab==0||s.tab==2,|d|d.child(label("COMMITS",p)).child(card(p).child(row().child(mono("c07d5aa")).child("feat(search): FTS5 index over checkpoints").child(nav(Page::History,cx))).child(muted("Codex · 1h",p))))
                .when(s.tab==0||s.tab==3,|d|d.child(label("MEMORY EPISODES",p)).child(card(p).child(mono("ep-0042")).child("Kept synchronous checkpoint writes after async trade-off review").child(muted("agent session",p))))
                .when(s.tab==0||s.tab==4,|d|d.child(title("Go to")).children(pages.iter().map(|page|row().py_1().child(nav(*page,cx)).child(div().flex_1()).child(mono(format!("libra {}",page.key()))))))
                .when(s.tab==4&&pages.is_empty(),|d|d.child(muted("No matching pages. Try History, Settings or Sessions.",p))))
            .child(row().px_4().py_3().border_t_1().border_color(p.border).child(muted("Tab to move · Return to open",p)).child(div().flex_1()).child(muted("Ciphertext and envelopes are never indexed.",p))))
        .into_any_element()
}
