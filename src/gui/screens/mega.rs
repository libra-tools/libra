//! Read-only remote browser design; no requests are sent.
use ely_gpui_component::theme::Palette;
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, px,
};

use super::super::{
    desktop::Desktop,
    widgets::{action, code, column, field, mono, muted, row, title},
};
pub fn render(s: &Desktop, cx: &Context<Desktop>, p: &Palette) -> AnyElement {
    let files = [
        "▾ project",
        "    ▾ gateway",
        "        ▾ src",
        "            main.rs",
        "            lib.rs",
        "        Cargo.toml",
        "        README.md",
        "    ▸ docs",
        "    ▸ assets",
    ];
    let previews = [
        ("/project", "gateway/\ndocs/\nassets/"),
        ("/project/gateway", "src/\nCargo.toml\nREADME.md"),
        ("/project/gateway/src", "main.rs\nlib.rs"),
        (
            "/project/gateway/src/main.rs",
            "use gateway::serve;\n\nfn main() -> anyhow::Result<()> {\n    serve()\n}",
        ),
        (
            "/project/gateway/src/lib.rs",
            "pub mod limiter;\npub mod routes;",
        ),
        (
            "/project/gateway/Cargo.toml",
            "[package]\nname = \"gateway\"\nedition = \"2024\"\n\n[dependencies]\naxum = \"0.8\"\ntokio = { version = \"1\", features = [\"full\"] }\nserde = { version = \"1\", features = [\"derive\"] }",
        ),
        (
            "/project/gateway/README.md",
            "# Gateway\n\nA demonstration API gateway with per-key rate limiting.",
        ),
        (
            "/project/docs",
            "Folder preview · choose a file to read its contents.",
        ),
        (
            "/project/assets",
            "Folder preview · choose a file to read its contents.",
        ),
    ];
    let preview = previews.get(s.selected).unwrap_or(&previews[5]);
    column()
        .size_full()
        .gap_0()
        .child(
            row()
                .px_5()
                .py_3()
                .bg(p.surface)
                .child(title("Mega2 browser"))
                .child(muted("works without a repository", p))
                .child(div().flex_1())
                .child(div().w(px(270.0)).flex_none().child(field(
                    "SERVER",
                    "https://mega.acme.internal",
                    p,
                )))
                .child(div().w(px(90.0)).flex_none().child(field("REF", "main", p)))
                .child(action("mega-open", "Open", true, cx)),
        )
        .child(
            row()
                .items_stretch()
                .gap_0()
                .flex_1()
                .min_h(px(0.0))
                .child(
                    column()
                        .id("remote-tree")
                        .overflow_y_scroll()
                        .w(px(280.0))
                        .flex_none()
                        .gap_1()
                        .p_4()
                        .children(files.iter().enumerate().map(|(i, f)| {
                            div()
                                .id(("remote-file", i))
                                .py_2()
                                .px_2()
                                .rounded(px(5.0))
                                .bg(if s.selected == i { p.selection } else { p.bg })
                                .cursor_pointer()
                                .on_click(cx.listener(move |s, _, _, cx| {
                                    s.selected = i;
                                    cx.notify();
                                }))
                                .child(mono(*f))
                        })),
                )
                .child(
                    column()
                        .flex_1()
                        .bg(p.surface)
                        .p_5()
                        .child(
                            row()
                                .child(mono(preview.0))
                                .child(muted("Bundled preview", p))
                                .child(div().flex_1())
                                .child(action("copy-path", "Copy path", false, cx)),
                        )
                        .child(code(preview.1).flex_1()),
                ),
        )
        .child(
            row()
                .px_5()
                .py_3()
                .border_t_1()
                .border_color(p.border)
                .child(muted("Anonymous read · no credentials sent", p))
                .child(muted("Nothing cloned, nothing written locally", p))
                .child(div().flex_1())
                .child(muted(
                    "Limits: 1 MiB / response · 2000 entries / folder · 10 s timeout",
                    p,
                )),
        )
        .into_any_element()
}
