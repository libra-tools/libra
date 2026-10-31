//! Merge review with illustrative line-level provenance.
use ely_gpui_component::theme::Palette;
use gpui::{AnyElement, Context, IntoElement, ParentElement, div, prelude::*, px, rgb};

use super::super::{
    desktop::{Desktop, Page},
    widgets::{
        action, agent, badge, card, column, label, mono, muted, nav, note, row, tabs, title,
    },
};
const LINES: &[(&str, &str)] = &[
    ("", "pub struct TokenBucket {"),
    ("CC", "    capacity: f64,"),
    ("CC", "    tokens: f64,"),
    ("CC", "    refill_rate: f64,"),
    ("CC", "    last_refill: Instant,"),
    ("", "}"),
    ("", ""),
    ("CC", "impl TokenBucket {"),
    ("CC", "    pub fn new(capacity: f64) -> Self {"),
    ("E", "        let capacity = capacity.max(1.0);"),
    ("CC", "        Self { capacity, tokens: capacity,"),
    ("CC", "            refill_rate: capacity / 60.0,"),
    ("CC", "            last_refill: Instant::now() }"),
    ("CC", "    }"),
    ("", ""),
    ("CC", "    pub fn try_acquire(&mut self) -> bool {"),
    ("CC", "        self.refill();"),
    ("E", "        if self.tokens < 1.0 { return false; }"),
    ("E", "        self.tokens -= 1.0;"),
    ("E", "        true"),
    ("CC", "    }"),
    ("", "}"),
];
pub fn render(s: &Desktop, cx: &Context<Desktop>, p: &Palette) -> AnyElement {
    column().size_full().gap_0()
        .child(row().px_5().py_3().bg(p.surface).child(title("Review")).child(mono("agent/rate-limit → main")).child(muted("3 files · 2 commits · stacked: fix/limiter-config",p)).child(div().flex_1()).child(tabs(&["Unified","Split"],s.tab,cx)))
        .child(note("Preview · line-level attribution (roadmap)",p))
        .child(row().items_stretch().gap_0().flex_1().min_h(px(0.0))
            .child(column().w(px(225.0)).flex_none().p_4().child(label("FILES",p)).children(["src/gateway/limiter.rs","src/gateway/mod.rs","config/gateway.toml"].iter().enumerate().map(|(i,path)|{
                div().id(("review-file",i)).p_2().rounded(px(6.0)).bg(if s.selected==i{p.selection}else{p.bg}).cursor_pointer().on_click(cx.listener(move|s,_,_,cx|{s.selected=i;cx.notify();})).child(mono(*path))
            })).child(label("ATTRIBUTION",p)).child(row().child(agent("Claude Code")).child("Claude Code")).child(row().child(agent("You")).child("You")).child(muted("Unchanged context",p)))
            .child(column().id("review-diff").overflow_y_scroll().flex_1().gap_0().bg(p.surface)
                .child(row().px_4().py_3().child(mono(["src/gateway/limiter.rs","src/gateway/mod.rs","config/gateway.toml"].get(s.selected).copied().unwrap_or("src/gateway/limiter.rs"))).child(badge("+18 −1",0x166534,0xdcfce7)))
                .children(LINES.iter().enumerate().map(|(i,(who,text))| {
                    row().gap_2().h(px(25.0)).px_3().flex_none().bg(if who.is_empty(){p.surface}else{p.success_subtle})
                        .when(s.tab==1,|r|r.child(mono(if who.is_empty(){*text}else{""}).flex_1().text_color(p.fg_muted)))
                        .child(div().w(px(30.0)).child(badge(*who,if *who=="CC"{0xc2410c}else{0x334155},if *who=="CC"{0xfff1e8}else{0xe2e8f0})))
                        .child(muted(format!("{:>2}",i+1),p).w(px(25.0))).child(mono(if who.is_empty(){" "}else{"+"})).child(mono(*text).flex_1())
                }))
                .child(card(p).m_4().border_color(rgb(0xfdba8c)).child(row().child(agent("Claude Code")).child("Lines 12–13 · Claude Code"))
                    .child(mono("ses_7f3a91 · turn 3 · ckpt_0191")).child("“…100 requests per minute per API key, configurable in gateway.toml.”").child(nav(Page::Sessions,cx))))
            .child(column().id("review-inspector").overflow_y_scroll().w(px(295.0)).flex_none().p_4().border_l_1().border_color(p.border)
                .child(card(p).child(title("Authorship in this file")).child(row().child(agent("Claude Code")).child("Claude Code · 14 lines")).child(row().child(agent("You")).child("You · 4 lines")))
                .child(card(p).child(title("Captured evidence")).child(mono("cargo test -p gateway")).child(badge("exit 0 · ses_7f3a91",0x166534,0xdcfce7)).child(muted("Only what the agent actually ran. No synthetic checks.",p)))
                .child(card(p).child(title("Comment")).child(ely_gpui_component::forms::Input::new(&s.draft))
                    .child(row().child(action("approve","Approve",true,cx)).child(action("request-changes","Request changes",false,cx))).child(action("send-comment","Send back to Claude Code",false,cx)))
                .child(action("merge","Merge into main",true,cx)).child(muted("The merge commit keeps links to both sessions and their checkpoints.",p)))).into_any_element()
}
