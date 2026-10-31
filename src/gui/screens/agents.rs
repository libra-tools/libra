//! Agent trust cards and doctor diagnostics, all illustrative.
use ely_gpui_component::theme::Palette;
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, px,
};

use super::super::{
    desktop::Desktop,
    widgets::{action, agent, badge, card, code, column, label, muted, note, row, table, title},
};
pub fn render(_: &Desktop, cx: &Context<Desktop>, p: &Palette) -> AnyElement {
    column()
        .id("agents-page")
        .size_full()
        .overflow_y_scroll()
        .p_6()
        .gap_5()
        .child(
            row()
                .child(title("Agents"))
                .child(div().flex_1())
                .child(action("add-agent", "+ Add agent", true, cx)),
        )
        .child(muted(
            "Capture is enabled per repository and never changes how you run the agent. Adapters are fail-closed \
             until trusted.",
            p,
        ))
        .child(
            row()
                .items_start()
                .gap_5()
                .child(
                    column()
                        .flex_1()
                        .child(table(
                            &["Agent", "Support", "Hooks", "RPC adapter", "Review / investigate"],
                            &[
                                vec!["CC  Claude Code", "Full capture", "Installed", "Trusted", "Ready"],
                                vec!["CX  Codex", "Full capture", "Installed", "Trusted", "Ready"],
                                vec!["OC  OpenCode", "Capture", "Not installed", "Closed", "Trust first"],
                                vec!["Cursor", "Experimental", "Not installed", "Unsupported", "Unavailable"],
                                vec![
                                    "Gemini CLI",
                                    "Experimental",
                                    "Not installed",
                                    "Unsupported",
                                    "Unavailable",
                                ],
                            ],
                            p,
                        ))
                        .child(
                            card(p)
                                .border_color(p.warning)
                                .child(
                                    row()
                                        .child(agent("OpenCode"))
                                        .child(title("Trust the OpenCode RPC adapter?")),
                                )
                                .child(note(
                                    "Until trusted, Libra captures nothing from OpenCode and returns an actionable \
                                     error instead of degrading silently. Trust is scoped to this directory and can \
                                     be revoked any time.",
                                    p,
                                ))
                                .child(code("libra agent rpc trust opencode --dir ~/work/acme-libra"))
                                .child(
                                    row()
                                        .child(action("trust-agent", "Trust for this repo", true, cx))
                                        .child(action("trust-later", "Not now", false, cx)),
                                ),
                        ),
                )
                .child(
                    column()
                        .w(px(355.0))
                        .flex_none()
                        .child(
                            card(p)
                                .child(row().child(title("Agent doctor")).child(muted("ran 2m ago", p)))
                                .child(action("doctor", "Run again", false, cx))
                                .child(label("✓  HOOK INSTALLATION", p))
                                .child("Claude Code and Codex hooks present")
                                .child(label("✓  CAPTURE STATE", p))
                                .child("1 running session, 13 stopped")
                                .child(badge("! Checkpoint store", 0x92400e, 0xfffbeb))
                                .child("2 stale catalog rows, 1 missing object_index row")
                                .child(label("i  MANAGED TASK DIRECTORIES", p))
                                .child("1 unowned directory reported, not deleted")
                                .child(action("doctor-repair", "Repair checkpoint store", false, cx))
                                .child(code("libra agent doctor --repair")),
                        )
                        .child(
                            card(p)
                                .child(title("Sandbox"))
                                .child("Backend · macOS Seatbelt · preview")
                                .child(muted(
                                    "External reviewer CLIs use their own sandbox settings. Libra's internal sandbox \
                                     does not confine them.",
                                    p,
                                )),
                        )
                        .child(
                            card(p)
                                .child(title("Trace refs"))
                                .child(muted("Local only. Nothing has been pushed.", p))
                                .child(action("push-traces", "Push to origin…", false, cx)),
                        ),
                ),
        )
        .into_any_element()
}
