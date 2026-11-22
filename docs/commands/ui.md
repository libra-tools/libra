# `libra ui`

Open the Libra desktop design preview with read-only repository context and bundled demonstration data.
This is a Libra extension. Git has no equivalent.

The window is part of every standard Libra build. No Cargo feature flag is
required; a plain `cargo build` produces a binary where `libra ui` opens the
window.

## Synopsis

```
libra ui
```

## Description

`libra ui` with no positional arguments opens the Libra desktop on the process
main thread; global flags on either side of the subcommand do not affect the
window. The interface uses [Ely GPUI Components](https://github.com/ELY-Open-Source/Ely-GPUI-Components),
pinned to repository revision `dd727f4`, with `gpui-pre` 0.3.10. It opens on
History, with real commits (newest first by commit time, from a traversal
limited to 400 commits and capped at 200 rows; parents decide merge markers), the Human / Agent-authored / Has
checkpoint attribution filters backed by the capture catalog, and a
creation-history inspector.
The toolbar and **All tools** (`⌘K`) open the other desktop views:

- History, agent lanes, Sessions (Overview / Conversation / Changes), capability
  map, search, operation log and memory episodes.
- Agent trust and doctor, review runs, investigation, merge review and automation.
- Welcome/import, conflict resolution, blame, bisect, stashes/tags/worktrees and
  interactive rebase.
- Cloud/remotes, sparse views/layers, file dependencies, Mega2, settings, splash,
  startup-error illustration and About.

These views cover the 26 supplied design screens using native GPUI components. Splash shows the real repository/data-surface availability; the startup-error
screen is an inspectable illustration in All tools and runs no repository checks.
Sessions has light and dark themes, readable-content search and local filters.
Form editing and checkboxes affect the preview only. No settings are saved.

This is an interface preview based on the supplied desktop design. The window
reads the current working directory's repository **read-only**: the context bar shows
the working-tree folder name, storage label, branch, HEAD and object format, and the read
never creates operation rows, upgrades the schema or takes a writer boundary.
The context bar, History's commit list and attribution filters, the Sessions
list, and the Operations list are real. Operations come from one bounded
Operation v2 snapshot (newest first, 50k-row cap with a truncated marker);
paging slices that snapshot, so pages never repeat or drop rows, and a new
snapshot resets the list to page one. Summaries are redacted excerpts.
The Memory page shows bounded real episodes (≤200) from the projection; a stale
projection shows zero rows with a `libra memory rebuild` hint, matching the CLI.
Splash, Welcome, About and the capability map show the real repository context and
each data surface's observed state (unchecked surfaces show `未检查`). The window
label is **Read-only preview**; pages that still render bundled demonstration
content carry a per-page `demo` badge. Search covers pages and surface states,
and the bundled session catalog has been removed.
Reviews and Investigate show real run directories (newest 100 per kind) with
item-level states: a corrupt, oversized, incomplete or unknown-kind run affects
only its own row, and a scan cap marks the list as a truncated subset.
Sessions come from the read-only capture catalog (filtered to this
worktree/workspace scope, newest first, capped at 200 rows); model and usage
fields may be absent, summaries are redacted before rendering, and a repository
without capture
shows an empty state. Inside History, the inspector's prompt, session, tool and
checkpoint details are sample content; only the commit metadata and attribution
counts are real. Navigation, selection, tabs and theme changes are local
UI interactions. Repository, agent, trust,
restore, network and account actions do not execute. These actions show a preview notice. Every page carries a **Read-only preview** label beside the shared context bar (and in the global navigation bar where that bar is shown). Close the window or
press `⌘Q` to exit; `Esc` dismisses the command palette.
The logo is a bundled copy of `https://libra.tools/libra-logo-without-words.svg`;
fonts and assets load offline. macOS is the current validation target.

The command reads the current repository read-only and never writes it. Outside
a repository the window opens with a clear "not created"/"no repository" state
instead of failing.

`libra ui --help` prints this command's usage and does not open a window.
Any other argument except a global flag is a usage error, also without opening
a window.

Calling the command through the library dispatcher (`libra::exec`) does not
open a window; it returns `LBR-UNSUPPORTED-001` because the UI toolkit must own
the process main thread. The window has to start from the `libra` process entry
point.

On Linux, a session with neither `DISPLAY` nor `WAYLAND_DISPLAY` exits 128 with
`LBR-UNSUPPORTED-001` and a clear message before GPUI starts.

Building from source links the GPUI desktop stack. Linux builds need the
fontconfig and xkbcommon development headers (`libfontconfig1-dev`,
`libxkbcommon-dev`, and `libxkbcommon-x11-dev` on Debian/Ubuntu).

## Options

`ui` takes no flags of its own. Global flags are accepted on either side of
the subcommand (`libra --no-pager ui`, `libra ui --json`) and are ignored by
the window. Any other argument after `ui` is a usage error.

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | The window was opened and then closed. |
| `128` | The window failed to open, or Linux has no graphical display (`LBR-UNSUPPORTED-001`). |
| `129` | Unknown flags or extra arguments. `libra ui --help` exits 0. |

## Examples

```bash
# Open the desktop window.
libra ui

# Usage text. Does not open a window.
libra ui --help
```

## Comparison with Git

Git has no desktop subcommand. `ui` is `intentionally-different` in
[`COMPATIBILITY.md`](../../COMPATIBILITY.md).
