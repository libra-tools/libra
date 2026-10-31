//! Stashes, tags and worktrees as three independent panels.
use ely_gpui_component::theme::Palette;
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled,
};

use super::super::{
    desktop::Desktop,
    widgets::{action, badge, card, check, field, mono, muted, note, row, title},
};
pub fn render(s: &Desktop, cx: &Context<Desktop>, p: &Palette) -> AnyElement {
    row()
        .id("refs-page")
        .size_full()
        .overflow_y_scroll()
        .items_start()
        .p_6()
        .gap_5()
        .child(
            card(p)
                .flex_1()
                .gap_4()
                .child(title("Stashes"))
                .child(action("stash-changes", "Stash changes…", true, cx))
                .child(
                    card(p)
                        .bg(p.selection)
                        .child(mono("stash@{0}  wip limiter"))
                        .child(muted("1d · main", p))
                        .child(mono("M src/gateway/limiter.rs\nA notes/limiter.md"))
                        .child(row().child(action("stash-apply", "Apply", false, cx)).child(action(
                            "stash-pop",
                            "Pop",
                            true,
                            cx,
                        )))
                        .child(action("stash-branch", "New branch from stash", false, cx))
                        .child(action("stash-drop", "Drop", false, cx)),
                )
                .child(
                    card(p)
                        .child(mono("stash@{1}"))
                        .child("On feat/search: index experiments")
                        .child(muted("4d", p)),
                )
                .child(
                    card(p)
                        .child(mono("stash@{2}"))
                        .child("WIP on main: 18ad40b")
                        .child(muted("1w", p)),
                )
                .child(check("stash-untracked", "Include untracked", false, s, cx))
                .child(check("stash-index", "Keep index", false, s, cx)),
        )
        .child(
            card(p)
                .flex_1()
                .gap_4()
                .child(title("Tags"))
                .child(muted("Sort: version ⌄", p))
                .child(
                    card(p)
                        .child(mono("v0.4.0"))
                        .child(badge("signed · verified", 0x166534, 0xdcfce7))
                        .child(muted("e5f0c88", p)),
                )
                .child(card(p).child(mono("v0.3.2")).child(muted("annotated · b71d0aa", p)))
                .child(
                    card(p)
                        .child(mono("bench-baseline"))
                        .child(muted("lightweight · f31c9e7", p)),
                )
                .child(title("New tag"))
                .child(field("NAME", "v0.4.1", p))
                .child(field("AT", "HEAD", p))
                .child(field("MESSAGE", "Release notes…", p))
                .child(check("sign-tag", "Sign", true, s, cx))
                .child(action("tag-create", "Create tag", true, cx)),
        )
        .child(
            card(p)
                .flex_1()
                .gap_4()
                .child(title("Worktrees"))
                .child(action("worktree-add", "Add worktree…", true, cx))
                .child(
                    card(p)
                        .child(row().child(mono("main")).child(badge("current", 0x1d4ed8, 0xeaf1ff)))
                        .child(muted("main · ~/work/acme-libra", p)),
                )
                .child(
                    card(p)
                        .child(
                            row()
                                .child(mono("wt-review"))
                                .child(badge("locked", 0x92400e, 0xfef3c7)),
                        )
                        .child(muted("feat/search\n~/work/acme-review · “on external drive”", p))
                        .child(
                            row()
                                .child(action("unlock", "Unlock", false, cx))
                                .child(action("move-worktree", "Move…", false, cx))
                                .child(action("remove-worktree", "Remove…", false, cx)),
                        ),
                )
                .child(
                    card(p)
                        .child(
                            row()
                                .child(mono("wt-bench"))
                                .child(badge("missing", 0xb91c1c, 0xfee2e2)),
                        )
                        .child(muted("detached", p))
                        .child(note(
                            "Directory no longer exists. Prune removes the record; repair can relink it if you moved \
                             it.",
                            p,
                        ))
                        .child(
                            row()
                                .child(action("repair-worktree", "Repair…", false, cx))
                                .child(action("prune-worktree", "Prune", false, cx)),
                        ),
                )
                .child(muted(
                    "Each worktree shares history; agent sessions are filtered per worktree in Sessions.",
                    p,
                ))
                .child(action("worktree-doctor", "Doctor", false, cx)),
        )
        .into_any_element()
}
