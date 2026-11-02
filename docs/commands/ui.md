# `libra ui`

Open the Libra desktop design preview with bundled demonstration data.
This is a Libra extension. Git has no equivalent.

The window is part of every standard Libra build. No Cargo feature flag is
required; a plain `cargo build` produces a binary where `libra ui` opens the
window.

## Synopsis

```
libra ui
```

## Description

`libra ui` with no further arguments opens the Libra desktop on the process
main thread. The interface uses [Ely GPUI Components](https://github.com/ELY-Open-Source/Ely-GPUI-Components),
pinned to repository revision `dd727f4`, with `gpui-pre` 0.3.10. It opens on
History, with a commit graph, author filters, and a creation-history inspector.
The toolbar and **All tools** (`⌘K`) open the other desktop views:

- History, agent lanes, Sessions (Overview / Conversation / Changes), capability
  map, search, operation log and memory episodes.
- Agent trust and doctor, review runs, investigation, merge review and automation.
- Welcome/import, conflict resolution, blame, bisect, stashes/tags/worktrees and
  interactive rebase.
- Cloud/remotes, sparse views/layers, file dependencies, Mega2, settings, splash,
  startup-error illustration and About.

These views cover the 26 supplied design screens using native GPUI components. Splash and startup error
are inspectable illustrations in All tools; they do not run repository checks.
Sessions has light and dark themes, readable-content search and local filters.
Form editing and checkboxes affect the preview only. No settings are saved.

This is an interface preview based on the supplied desktop design. All records
and status indicators are bundled demonstration data. Navigation, selection,
tabs and theme changes are local UI interactions. Repository, agent, trust,
restore, network and account actions do not execute. The window is labeled
**Design preview**; these actions show a preview notice. Close the window or
press `⌘Q` to exit; `Esc` dismisses the command palette.
The logo is a bundled copy of `https://libra.tools/libra-logo-without-words.svg`;
fonts and assets load offline. macOS is the current validation target.

The command does not read or write a repository.

`libra ui --help` prints this command's usage and does not open a window.
Any other argument is a usage error, also without opening a window.

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

`ui` takes no flags of its own. Global flags that appear before the
subcommand (`libra --no-pager ui`) are ignored by the window.

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | The window was opened and then closed. |
| `128` | The window failed to open, or Linux has no graphical display (`LBR-UNSUPPORTED-001`). |
| `129` | Extra arguments. `libra ui --help` exits 0. |

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
