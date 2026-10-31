//! Dependency graph with the selected path explanation.
use ely_gpui_component::theme::Palette;
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, px,
};

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
    let selected = if s.tab == 1 && s.selected > 1 {
        0
    } else {
        s.selected
    };
    let path = match selected {
        1 => vec!["levels/forest/scene.usd", "materials/bark.mtl"],
        2 | 3 => vec![
            "levels/forest/scene.usd",
            "materials/bark.mtl",
            nodes[selected].2,
        ],
        4 => vec!["levels/forest/scene.usd", "meshes/tree.usd"],
        5 => vec![
            "levels/forest/scene.usd",
            "meshes/tree.usd",
            "materials/bark.mtl",
        ],
        6 => vec!["levels/forest/scene.usd", "textures/forest_sky.hdr"],
        _ => vec!["levels/forest/scene.usd"],
    };
    column()
        .size_full()
        .gap_0()
        .child(
            row()
                .px_5()
                .py_3()
                .bg(p.surface)
                .child(title("File dependencies"))
                .child(mono("levels/forest/scene.usd · HEAD"))
                .child(div().flex_1())
                .child(tabs(&["Depends on", "Depended on by"], s.tab, cx)),
        )
        .child(
            row()
                .items_stretch()
                .flex_1()
                .gap_0()
                .min_h(px(0.0))
                .child(
                    column()
                        .id("dependency-graph")
                        .overflow_y_scroll()
                        .flex_1()
                        .p_6()
                        .gap_4()
                        .child(muted("Depth · All", p))
                        .children(nodes.iter().enumerate().filter(|(_, n)| s.tab == 0 || n.0 < 2).map(
                            |(i, (depth, ext, path, kind))| {
                                row()
                                    .ml(px(*depth as f32 * 42.0))
                                    .child(muted(if *depth == 0 { "●" } else { "└──" }, p))
                                    .child(
                                        card(p)
                                            .id(("dep-node", i))
                                            .flex_1()
                                            .cursor_pointer()
                                            .bg(if selected == i { p.selection } else { p.surface })
                                            .on_click(cx.listener(move |s, _, _, cx| {
                                                s.selected = i;
                                                cx.notify();
                                            }))
                                            .child(
                                                row()
                                                    .child(badge(*ext, 0x1d4ed8, 0xeaf1ff))
                                                    .child(mono(*path))
                                                    .child(div().flex_1())
                                                    .child(muted(*kind, p)),
                                            ),
                                    )
                            },
                        ))
                        .child(note(
                            "Cycles are shown once and never followed twice. A commit with no dependency note simply \
                             has an empty graph.",
                            p,
                        )),
                )
                .child(
                    column()
                        .w(px(350.0))
                        .flex_none()
                        .p_5()
                        .border_l_1()
                        .border_color(p.border)
                        .child(
                            card(p)
                                .child(title("Why does it depend on this?"))
                                .child(muted("Target file", p))
                                .children(path.iter().enumerate().map(|(i, path)| {
                                    column().gap_2().when(i > 0, |d| d.child("↓")).child(mono(*path))
                                }))
                                .child(muted(
                                    if path.len() == 1 {
                                        "Root file".to_string()
                                    } else {
                                        format!("Shortest path, {} hops.", path.len() - 1)
                                    }, p,
                                )),
                        )
                        .child(
                            card(p)
                                .child(title("Add dependency"))
                                .child(field("FROM", "levels/forest/scene.usd", p))
                                .child(field("DEPENDS ON", "materials/bark.mtl", p))
                                .child(muted("Edges are versioned per commit. Repo-relative paths only.", p))
                                .child(action("add-edge", "Add edge", true, cx)),
                        ),
                ),
        )
        .into_any_element()
}
