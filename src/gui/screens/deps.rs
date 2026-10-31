//! Dependency graph with the selected path explanation.
use ely_gpui_component::theme::Palette;
use gpui::{AnyElement, Context, IntoElement, ParentElement, div, prelude::*, px};

use super::super::{
    desktop::Desktop,
    widgets::{action, badge, card, column, field, mono, muted, note, row, tabs, title},
};
pub fn render(s: &Desktop, cx: &Context<Desktop>, p: &Palette) -> AnyElement {
    let nodes = [
        (0, "USD", "levels/forest/scene.usd", "scene"),
        (1, "MTL", "materials/bark.mtl", "material"),
        (2, "GLSL", "shaders/pbr.glsl", "shader"),
        (2, "PNG", "textures/bark_albedo.png", "texture"),
        (1, "USD", "meshes/tree.usd", "mesh"),
        (2, "MTL", "materials/bark.mtl", "already listed"),
        (1, "PNG", "textures/forest_sky.hdr", "environment"),
    ];
    column().size_full().gap_0().child(row().px_5().py_3().bg(p.surface).child(title("File dependencies")).child(mono("levels/forest/scene.usd · HEAD")).child(div().flex_1()).child(tabs(&["Depends on","Depended on by"],s.tab,cx)))
        .child(row().items_stretch().flex_1().gap_0().min_h(px(0.0))
            .child(column().id("dependency-graph").overflow_y_scroll().flex_1().p_6().gap_4()
                .child(muted("Depth · All",p)).children(nodes.iter().enumerate().filter(|(_,n)|s.tab==0||n.0<2).map(|(i,(depth,ext,path,kind))|{
                    row().ml(px(*depth as f32*42.0)).child(muted(if *depth==0{"●"}else{"└──"},p)).child(card(p).id(("dep-node",i)).flex_1().cursor_pointer().bg(if s.selected==i{p.selection}else{p.surface}).on_click(cx.listener(move|s,_,_,cx|{s.selected=i;cx.notify();}))
                        .child(row().child(badge(*ext,0x1d4ed8,0xeaf1ff)).child(mono(*path)).child(div().flex_1()).child(muted(*kind,p))))
                })).child(note("Cycles are shown once and never followed twice. A commit with no dependency note simply has an empty graph.",p)))
            .child(column().w(px(350.0)).flex_none().p_5().border_l_1().border_color(p.border)
                .child(card(p).child(title("Why does it depend on this?")).child(muted("Target file",p)).child(mono("levels/forest/scene.usd")).child("↓").child(mono("materials/bark.mtl")).child("↓").child(mono(nodes.get(s.selected).map(|n|n.2).unwrap_or("shaders/pbr.glsl"))).child(muted("Shortest path, 2 hops.",p)))
                .child(card(p).child(title("Add dependency")).child(field("FROM","levels/forest/scene.usd",p)).child(field("DEPENDS ON","materials/bark.mtl",p)).child(muted("Edges are versioned per commit. Repo-relative paths only.",p)).child(action("add-edge","Add edge",true,cx))))).into_any_element()
}
