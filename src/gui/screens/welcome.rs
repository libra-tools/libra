//! Welcome and import form. The preview never opens or creates repositories.
use ely_gpui_component::theme::Palette;
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, img, px,
};

use super::super::{
    desktop::{Desktop, Page},
    widgets::{
        action, badge, card, check, code, column, field, label, muted, nav, note, row, tabs, title,
    },
};
pub fn render(s: &Desktop, cx: &Context<Desktop>, p: &Palette) -> AnyElement {
    row()
        .id("welcome")
        .size_full()
        .overflow_y_scroll()
        .items_start()
        .p_8()
        .gap_8()
        .child(
            column()
                .flex_1()
                .gap_5()
                .p_5()
                .child(img("libra/logo.svg").size(px(96.0)))
                .child(title("Version control that remembers how work was done").text_size(px(34.0)))
                .child(
                    muted(
                        "Git-compatible. Local-first. Records prompts, agent sessions and checkpoints next to your \
                         commits.",
                        p,
                    )
                    .text_size(px(15.0)),
                )
                .child(tabs(&["Import from Git", "Clone", "Open", "New"], s.tab, cx))
                .child(label("RECENT", p))
                .children(
                    [
                        ("acme-libra", "~/work/acme-libra · main", "2 agents capturing"),
                        ("gateway-sdk", "~/work/gateway-sdk · release/0.4", "capture off"),
                        ("docs-site", "~/work/docs-site · main", "capture off"),
                    ]
                    .iter()
                    .map(|(name, path, state)| {
                        card(p)
                            .id(gpui::SharedString::from(format!("recent-{name}")))
                            .child(
                                row()
                                    .child(*name)
                                    .child(div().flex_1())
                                    .child(badge(*state, 0x166534, 0xf0fdf4)),
                            )
                            .child(muted(*path, p))
                            .child(nav(Page::History, cx))
                    }),
                ),
        )
        .child(
            card(p)
                .flex_1()
                .mt_5()
                .p_6()
                .gap_5()
                .child(title(
                    [
                        "Import a Git repository",
                        "Clone a repository",
                        "Open a Libra repository",
                        "Create a new repository",
                    ]
                    .get(s.tab)
                    .copied()
                    .unwrap_or("Import a Git repository"),
                ))
                .child(muted(
                    "Libra uses Git's object model. History, branches and remotes come along.",
                    p,
                ))
                .child(field(
                    if s.tab == 1 { "REMOTE URL" } else { "GIT REPOSITORY" },
                    if s.tab == 1 {
                        "https://github.com/acme/acme.git"
                    } else {
                        "~/work/acme"
                    },
                    p,
                ))
                .child(action("browse-import", "Browse…", false, cx))
                .child(field("NEW LIBRA REPOSITORY", "~/work/acme-libra", p))
                .child(label("AFTER IMPORT", p))
                .child(check("import-claude", "Enable capture for Claude Code", true, s, cx))
                .child(check("import-codex", "Enable capture for Codex", true, s, cx))
                .child(check("import-opencode", "Enable capture for OpenCode", false, s, cx))
                .child(note(
                    "Each agent's RPC adapter stays closed until you trust it. You can change this later in Agents.",
                    p,
                ))
                .child(label("EQUIVALENT COMMAND", p))
                .child(code(
                    "libra init --from-git-repository ./acme ./acme-libra\nlibra agent enable --agent claude-code",
                ))
                .child(row().child(action("cancel-import", "Cancel", false, cx)).child(action(
                    "import-repo",
                    "Import repository",
                    true,
                    cx,
                ))),
        )
        .into_any_element()
}
