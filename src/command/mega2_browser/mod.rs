//! plan-20260912 MB-02: human-readable directory browser TUI over the MB-01
//! validated listing. Owns canonical current path, selection, history/cache and
//! the terminal lifecycle; performs no prefetch and delegates every fetch to
//! [`Mega2TreeSession`]. The [`noninteractive`] submodule runs the same
//! functions without a terminal: one operation flag, one request
//! (plan-20261001 MN-02).

pub mod noninteractive;
pub mod tag_panel;
pub mod terminal;

pub use tag_panel::{TAG_PAGE_SIZE, TagEditor, TagPanel, TagPanelAction};

#[cfg(unix)]
pub(crate) mod terminal_unix;
#[cfg(windows)]
pub(crate) mod terminal_windows;

use std::io::{self, Read, Write};

#[cfg(unix)]
use libc::{STDIN_FILENO, STDOUT_FILENO};

use self::terminal::TerminalGuard;
use crate::{
    internal::protocol::{
        mega2_auth::Mega2Token,
        mega2_entry::{Mega2EntryClient, validate_entry_name},
        mega2_mutate::Mega2MutateClient,
        mega2_tag::{CreateTagOptions, Mega2TagClient},
        mega2_tree::{
            ContentType, Listing, MAX_CACHE_ENTRIES, MAX_NAME_BYTES, Mega2TreeSession,
            normalize_path,
        },
    },
    utils::error::{CliError, CliResult, StableErrorCode},
};

/// Logical key input after escape-sequence parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Enter,
    Backspace,
    Home,
    Reload,
    /// `+`: start the create-directory editor.
    Create,
    /// `d`: ask to delete the selected directory.
    Delete,
    /// `m`: start the move editor for the selected directory.
    Move,
    /// `R`: start the same-parent rename editor (distinct from `r` reload).
    Rename,
    /// `t`: toggle the tag panel.
    Tags,
    /// Lone `Esc`: cancel the active editor (or quit when idle).
    Cancel,
    Quit,
    Other(char),
}

/// What the event loop must do after a key is handled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionResult {
    /// Fetch the current path (enter/back/reload), exactly one request.
    FetchCurrent,
    /// Run one confirmed remote directory creation, then reload once.
    CreateDirectory {
        parent: String,
        name: String,
    },
    /// Run one confirmed remote directory deletion, then reload once.
    DeleteDirectory {
        parent: String,
        name: String,
    },
    /// Run one confirmed remote move/rename, then reload once.
    MoveDirectory {
        from_parent: String,
        from_name: String,
        to_parent: String,
        to_name: String,
    },
    /// One explicit tag page fetch (panel open or page change; never prefetched).
    FetchTags {
        page: u64,
    },
    /// One confirmed tag creation, then refresh the panel page.
    CreateTag {
        name: String,
        message: Option<String>,
    },
    /// One confirmed tag deletion, then refresh the panel page.
    DeleteTag {
        name: String,
    },
    Continue,
    Quit,
}

/// The single active modal editor. Only one may be open at a time; input is
/// sanitized (printable, bounded) and validated before any request can be
/// planned. `Esc` always closes the editor without touching the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Editor {
    /// `+`: create a directory under the current path.
    CreateDir(String),
    /// `d`: confirmation line for deleting the selected directory.
    ConfirmDelete(String),
    /// `m`: move the selected directory under an edited destination parent.
    MoveDir { name: String, input: String },
    /// `R`: rename the selected directory (same parent, edited name).
    RenameDir { name: String, input: String },
}

/// Browser state: the only local truth is the canonical current path.
pub struct BrowserState {
    pub session: Mega2TreeSession,
    pub path: String,
    pub git_ref: Option<String>,
    pub selection: usize,
    pub entries: Vec<(String, ContentType)>,
    pub history: Vec<String>,
    pub status: Option<String>,
    pub running: bool,
    /// The single active modal editor, if any (sanitized input only).
    pub editor: Option<Editor>,
    /// The tag panel when it is open (directory keys become inert).
    pub panel: Option<TagPanel>,
}

impl BrowserState {
    pub fn new(session: Mega2TreeSession, path: &str, git_ref: Option<String>) -> CliResult<Self> {
        let path = crate::internal::protocol::mega2_tree::normalize_path(path)?;
        Ok(Self {
            session,
            path,
            git_ref,
            selection: 0,
            entries: Vec::new(),
            history: Vec::new(),
            status: None,
            running: true,
            editor: None,
            panel: None,
        })
    }

    /// Joins a validated parent path with a validated entry name.
    pub fn child_path(parent: &str, name: &str) -> String {
        if parent == "/" {
            format!("/{name}")
        } else {
            format!("{parent}/{name}")
        }
    }

    /// Parent of a canonical path; never climbs above `/`.
    pub fn parent_path(path: &str) -> String {
        match path.rfind('/') {
            None | Some(0) => "/".to_string(),
            Some(idx) => path[..idx].to_string(),
        }
    }

    /// One fetch per navigation action; no prefetch, no recursion.
    pub async fn fetch_current(&mut self) -> CliResult<()> {
        let listing = self
            .session
            .fetch(&self.path, self.git_ref.as_deref())
            .await?;
        self.apply_listing(listing);
        Ok(())
    }

    /// Whether the current selection blocks creating (a file is selected).
    fn selection_blocks_create(&self) -> bool {
        matches!(
            self.entries.get(self.selection),
            Some((_, ContentType::File))
        )
    }

    /// The selected entry's name when it is a directory.
    fn selected_directory_name(&self) -> Option<String> {
        match self.entries.get(self.selection) {
            Some((name, ContentType::Directory)) => Some(name.clone()),
            _ => None,
        }
    }

    /// Handles one key while an editor is open. Input is sanitized on entry
    /// (printable characters only, bounded length) and validated with the
    /// MB-04/MB-07 rules before any request can be planned.
    fn handle_editor_key(&mut self, key: Key) -> ActionResult {
        let Some(editor) = self.editor.take() else {
            return ActionResult::Continue;
        };
        match editor {
            Editor::CreateDir(mut input) => match key {
                Key::Enter => match validate_entry_name(&input) {
                    Ok(()) => ActionResult::CreateDirectory {
                        parent: self.path.clone(),
                        name: input,
                    },
                    Err(err) => {
                        self.status = Some(err.message().to_string());
                        self.editor = Some(Editor::CreateDir(input));
                        ActionResult::Continue
                    }
                },
                Key::Backspace => {
                    input.pop();
                    self.editor = Some(Editor::CreateDir(input));
                    ActionResult::Continue
                }
                Key::Cancel | Key::Quit => {
                    self.status = Some("create cancelled".to_string());
                    ActionResult::Continue
                }
                Key::Other(ch) => {
                    push_bounded(&mut input, ch);
                    self.editor = Some(Editor::CreateDir(input));
                    ActionResult::Continue
                }
                _ => {
                    self.editor = Some(Editor::CreateDir(input));
                    ActionResult::Continue
                }
            },
            Editor::ConfirmDelete(name) => match key {
                Key::Enter | Key::Other('y') | Key::Other('Y') => ActionResult::DeleteDirectory {
                    parent: self.path.clone(),
                    name,
                },
                Key::Cancel | Key::Quit | Key::Other('n') | Key::Other('N') => {
                    self.status = Some("delete cancelled".to_string());
                    ActionResult::Continue
                }
                _ => {
                    self.editor = Some(Editor::ConfirmDelete(name));
                    ActionResult::Continue
                }
            },
            Editor::MoveDir { name, mut input } => match key {
                Key::Enter => match normalize_path(&input) {
                    Ok(parent) => ActionResult::MoveDirectory {
                        from_parent: self.path.clone(),
                        from_name: name.clone(),
                        to_parent: parent,
                        to_name: name,
                    },
                    Err(err) => {
                        self.status = Some(err.message().to_string());
                        self.editor = Some(Editor::MoveDir { name, input });
                        ActionResult::Continue
                    }
                },
                Key::Backspace => {
                    input.pop();
                    self.editor = Some(Editor::MoveDir { name, input });
                    ActionResult::Continue
                }
                Key::Cancel | Key::Quit => {
                    self.status = Some("move cancelled".to_string());
                    ActionResult::Continue
                }
                Key::Other(ch) => {
                    push_bounded(&mut input, ch);
                    self.editor = Some(Editor::MoveDir { name, input });
                    ActionResult::Continue
                }
                _ => {
                    self.editor = Some(Editor::MoveDir { name, input });
                    ActionResult::Continue
                }
            },
            Editor::RenameDir { name, mut input } => match key {
                Key::Enter => match validate_entry_name(&input) {
                    Ok(()) => ActionResult::MoveDirectory {
                        from_parent: self.path.clone(),
                        from_name: name,
                        to_parent: self.path.clone(),
                        to_name: input,
                    },
                    Err(err) => {
                        self.status = Some(err.message().to_string());
                        self.editor = Some(Editor::RenameDir { name, input });
                        ActionResult::Continue
                    }
                },
                Key::Backspace => {
                    input.pop();
                    self.editor = Some(Editor::RenameDir { name, input });
                    ActionResult::Continue
                }
                Key::Cancel | Key::Quit => {
                    self.status = Some("rename cancelled".to_string());
                    ActionResult::Continue
                }
                Key::Other(ch) => {
                    push_bounded(&mut input, ch);
                    self.editor = Some(Editor::RenameDir { name, input });
                    ActionResult::Continue
                }
                _ => {
                    self.editor = Some(Editor::RenameDir { name, input });
                    ActionResult::Continue
                }
            },
        }
    }

    fn apply_listing(&mut self, listing: Listing) {
        self.entries = listing
            .entries
            .into_iter()
            .map(|e| (e.name, e.content_type))
            .collect();
        if self.selection >= self.entries.len() {
            self.selection = 0;
        }
        self.status = None;
    }

    /// Handles one key against the state; the caller performs `FetchCurrent`.
    pub fn handle_key(&mut self, key: Key) -> ActionResult {
        if self.editor.is_some() {
            return self.handle_editor_key(key);
        }
        if let Some(panel) = self.panel.as_mut() {
            return match panel.handle_key(key) {
                TagPanelAction::Continue => ActionResult::Continue,
                TagPanelAction::Close => {
                    self.panel = None;
                    ActionResult::Continue
                }
                TagPanelAction::Fetch { page } => ActionResult::FetchTags { page },
                TagPanelAction::Create { name, message } => {
                    ActionResult::CreateTag { name, message }
                }
                TagPanelAction::Delete { name } => ActionResult::DeleteTag { name },
            };
        }
        match key {
            Key::Up => {
                if self.selection > 0 {
                    self.selection -= 1;
                }
                ActionResult::Continue
            }
            Key::Down => {
                if !self.entries.is_empty() && self.selection + 1 < self.entries.len() {
                    self.selection += 1;
                }
                ActionResult::Continue
            }
            Key::Enter => {
                if let Some((name, content_type)) = self.entries.get(self.selection) {
                    match content_type {
                        ContentType::Directory => {
                            let next = Self::child_path(&self.path, name);
                            self.history.push(self.path.clone());
                            // Bounded history: keep at most MAX_CACHE_ENTRIES entries.
                            if self.history.len() > MAX_CACHE_ENTRIES {
                                self.history.remove(0);
                            }
                            self.path = next;
                            self.selection = 0;
                            ActionResult::FetchCurrent
                        }
                        ContentType::File => {
                            self.status = Some(format!("file: {name} (no preview)"));
                            ActionResult::Continue
                        }
                    }
                } else {
                    ActionResult::Continue
                }
            }
            Key::Backspace | Key::Home => {
                if self.path != "/" {
                    self.path = Self::parent_path(&self.path);
                    self.selection = 0;
                    ActionResult::FetchCurrent
                } else {
                    ActionResult::Continue
                }
            }
            Key::Reload => ActionResult::FetchCurrent,
            Key::Create => {
                if self.selection_blocks_create() {
                    self.status = Some(
                        "select a directory (or no entry) to create a directory here".to_string(),
                    );
                } else {
                    self.editor = Some(Editor::CreateDir(String::new()));
                    self.status = None;
                }
                ActionResult::Continue
            }
            Key::Delete => {
                match self.selected_directory_name() {
                    Some(name) => {
                        self.editor = Some(Editor::ConfirmDelete(name));
                        self.status = None;
                    }
                    None => {
                        self.status =
                            Some("select a directory to delete (files are inert)".to_string());
                    }
                }
                ActionResult::Continue
            }
            Key::Move => {
                match self.selected_directory_name() {
                    Some(name) => {
                        self.editor = Some(Editor::MoveDir {
                            name,
                            input: self.path.clone(),
                        });
                        self.status = None;
                    }
                    None => {
                        self.status = Some("select a directory to move".to_string());
                    }
                }
                ActionResult::Continue
            }
            Key::Rename => {
                match self.selected_directory_name() {
                    Some(name) => {
                        self.editor = Some(Editor::RenameDir {
                            name: name.clone(),
                            input: name,
                        });
                        self.status = None;
                    }
                    None => {
                        self.status = Some("select a directory to rename".to_string());
                    }
                }
                ActionResult::Continue
            }
            Key::Tags => {
                self.panel = Some(TagPanel::new());
                ActionResult::FetchTags { page: 1 }
            }
            Key::Cancel | Key::Quit => {
                self.running = false;
                ActionResult::Quit
            }
            Key::Other(_) => ActionResult::Continue,
        }
    }
}

/// Appends one printable character while staying within the shared name bound.
pub(crate) fn push_bounded(input: &mut String, ch: char) {
    if !ch.is_control() && input.len() + ch.len_utf8() <= MAX_NAME_BYTES {
        input.push(ch);
    }
}

/// Maps raw terminal bytes (after raw-mode reading) to logical keys.
pub fn parse_key(byte: u8, sequence: &[u8]) -> Key {
    match byte {
        b'\x1b' => match sequence {
            [b'[', b'A'] => Key::Up,
            [b'[', b'B'] => Key::Down,
            _ => Key::Other('?'),
        },
        b'\r' | b'\n' => Key::Enter,
        127 | 8 => Key::Backspace,
        b'q' | b'Q' => Key::Quit,
        b'r' => Key::Reload,
        b'R' => Key::Rename,
        b'+' => Key::Create,
        b'd' => Key::Delete,
        b'm' => Key::Move,
        b't' => Key::Tags,
        b'h' | b'H' => Key::Home,
        b'k' | b'K' => Key::Up,
        b'j' | b'J' => Key::Down,
        other => Key::Other(other as char),
    }
}

/// Prints only printable characters; anything else becomes a visible placeholder
/// so hostile names can never execute control sequences in the terminal.
pub fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// Renders one frame: header, selection list (dirs first per MB-01 ordering),
/// status line. All untrusted strings pass through [`sanitize`].
pub fn render(state: &BrowserState, server: &str) -> String {
    let mut out = String::new();
    out.push_str("\x1b[2J\x1b[H");
    out.push_str(&format!(
        "mega2 browser — server: {server}  ref: {}  path: {}\r\n",
        state.git_ref.as_deref().unwrap_or("(default)"),
        state.path
    ));
    out.push_str("──────────────────────────────────────────────\r\n");
    if let Some(panel) = state.panel.as_ref() {
        out.push_str(&panel.render_lines());
        return out;
    }
    if state.entries.is_empty() {
        out.push_str("(empty directory)\r\n");
    }
    for (idx, (name, content_type)) in state.entries.iter().enumerate() {
        let marker = if idx == state.selection { ">" } else { " " };
        let kind = match content_type {
            ContentType::Directory => "dir ",
            ContentType::File => "file",
        };
        out.push_str(&format!("{marker} {kind}  {}\r\n", sanitize(name)));
    }
    out.push_str("──────────────────────────────────────────────\r\n");
    match state.editor.as_ref() {
        Some(Editor::CreateDir(input)) => {
            out.push_str(&format!("new directory: {}\r\n", sanitize(input)));
            out.push_str("Enter create · Esc cancel\r\n");
        }
        Some(Editor::ConfirmDelete(name)) => {
            out.push_str(&format!(
                "delete '{}'? Enter/y confirm · Esc cancel\r\n",
                sanitize(name)
            ));
        }
        Some(Editor::MoveDir { name, input }) => {
            out.push_str(&format!(
                "move '{}' to parent: {}\r\n",
                sanitize(name),
                sanitize(input)
            ));
            out.push_str("Enter move · Esc cancel\r\n");
        }
        Some(Editor::RenameDir { name, input }) => {
            out.push_str(&format!(
                "rename '{}' to: {}\r\n",
                sanitize(name),
                sanitize(input)
            ));
            out.push_str("Enter rename · Esc cancel\r\n");
        }
        None => {}
    }
    if state.editor.is_some() {
        out.push_str("-------------------------------\r\n");
    }
    let status = state.status.as_deref().unwrap_or(
        "Up/Down select · Enter open · + new dir · d delete · m move · R rename · r reload · q quit",
    );
    out.push_str(&sanitize(status));
    out.push_str("\r\n");
    out
}

/// Requires stdin and stdout to be TTYs; refuses before any terminal change.
pub fn ensure_tty() -> CliResult<()> {
    let (stdin_tty, stdout_tty) = stdin_stdout_are_ttys();
    tty_required(stdin_tty, stdout_tty)
}

#[cfg(unix)]
fn stdin_stdout_are_ttys() -> (bool, bool) {
    // SAFETY: STDIN_FILENO/STDOUT_FILENO are valid; isatty only reads.
    let stdin_tty = unsafe { libc::isatty(STDIN_FILENO) } == 1;
    let stdout_tty = unsafe { libc::isatty(STDOUT_FILENO) } == 1;
    (stdin_tty, stdout_tty)
}

#[cfg(windows)]
fn stdin_stdout_are_ttys() -> (bool, bool) {
    use windows_sys::Win32::{
        Foundation::{HANDLE, INVALID_HANDLE_VALUE},
        System::Console::{GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE},
    };
    fn is_console(handle: HANDLE) -> bool {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut mode: u32 = 0;
        // SAFETY: handle came from GetStdHandle; mode is writable.
        let ok = unsafe { GetConsoleMode(handle, &mut mode) };
        ok != 0
    }
    // SAFETY: STD_*_HANDLE are standard handle selectors.
    let stdin_tty = is_console(unsafe { GetStdHandle(STD_INPUT_HANDLE) });
    // SAFETY: as above.
    let stdout_tty = is_console(unsafe { GetStdHandle(STD_OUTPUT_HANDLE) });
    (stdin_tty, stdout_tty)
}

#[cfg(not(any(unix, windows)))]
fn stdin_stdout_are_ttys() -> (bool, bool) {
    (false, false)
}

/// Pure TTY gate, testable on every platform (fail-closed seam).
pub fn tty_required(stdin_tty: bool, stdout_tty: bool) -> CliResult<()> {
    if !stdin_tty || !stdout_tty {
        return Err(CliError::fatal(
            "mega2 browser: interactive mode requires stdin and stdout to be terminals (use --json for scripts)",
        )
        .with_stable_code(StableErrorCode::Unsupported));
    }
    Ok(())
}

/// Reads one logical key from raw-mode stdin. A lone `Esc` (no sequence bytes
/// within a short window) maps to [`Key::Quit`].
pub fn read_key(stdin: &mut io::Stdin) -> io::Result<Key> {
    let mut byte = [0u8; 1];
    stdin.read_exact(&mut byte)?;
    if byte[0] != 0x1b {
        return Ok(parse_key(byte[0], &[]));
    }
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;

        let raw_fd = stdin.as_raw_fd();
        let mut pollfd = libc::pollfd {
            fd: raw_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: pollfd points to one valid descriptor entry.
        let ready = unsafe { libc::poll(&mut pollfd, 1, 100) };
        if ready <= 0 {
            return Ok(Key::Cancel); // lone Esc cancels an editor / quits when idle
        }
    }
    let mut seq = [0u8; 2];
    let n = stdin.read(&mut seq)?;
    let mut padded = [0u8; 2];
    padded[..n.min(2)].copy_from_slice(&seq[..n.min(2)]);
    if n == 0 {
        return Ok(Key::Quit);
    }
    Ok(parse_key(byte[0], &padded))
}

/// Runs one confirmed creation: exactly one MB-04 POST, then exactly one MB-01
/// reload on success. On failure the caller keeps the last safe listing.
pub async fn perform_create(
    state: &mut BrowserState,
    client: &Mega2EntryClient,
    parent: &str,
    name: &str,
) -> CliResult<()> {
    client.create_directory(parent, name).await?;
    state.fetch_current().await
}

/// Runs one confirmed deletion: exactly one MB-07 POST, then exactly one MB-01
/// reload on success. On failure the caller keeps the last safe listing.
pub async fn perform_delete(
    state: &mut BrowserState,
    client: &Mega2MutateClient,
    parent: &str,
    name: &str,
) -> CliResult<()> {
    client
        .delete_directory(parent, name, Some(ContentType::Directory))
        .await?;
    state.fetch_current().await
}

/// Runs one confirmed move/rename: exactly one MB-07 POST, then exactly one
/// MB-01 reload on success.
pub async fn perform_move(
    state: &mut BrowserState,
    client: &Mega2MutateClient,
    from_parent: &str,
    from_name: &str,
    to_parent: &str,
    to_name: &str,
) -> CliResult<()> {
    client
        .move_entry(
            from_parent,
            from_name,
            to_parent,
            to_name,
            Some(ContentType::Directory),
        )
        .await?;
    state.fetch_current().await
}

/// Runs one explicit tag page fetch (no prefetch) and applies it to the panel.
pub async fn perform_fetch_tags(
    state: &mut BrowserState,
    client: &Mega2TagClient,
    page: u64,
) -> CliResult<()> {
    let page_data = client.list_tags(page, TAG_PAGE_SIZE, "/").await?;
    if let Some(panel) = state.panel.as_mut() {
        panel.apply_page(page, page_data.total, page_data.items);
    }
    Ok(())
}

/// Runs one confirmed tag creation (exactly one POST) and refreshes the page.
pub async fn perform_create_tag(
    state: &mut BrowserState,
    client: &Mega2TagClient,
    name: &str,
    message: Option<&str>,
) -> CliResult<()> {
    let options = CreateTagOptions {
        name,
        message,
        ..CreateTagOptions::default()
    };
    client.create_tag(&options).await?;
    refresh_tag_panel(state, client).await;
    Ok(())
}

/// Runs one confirmed tag deletion (exactly one DELETE) and refreshes the page.
pub async fn perform_delete_tag(
    state: &mut BrowserState,
    client: &Mega2TagClient,
    name: &str,
) -> CliResult<()> {
    client.delete_tag(name, "/").await?;
    refresh_tag_panel(state, client).await;
    Ok(())
}

/// Refreshes the currently visible tag page after a successful mutation.
async fn refresh_tag_panel(state: &mut BrowserState, client: &Mega2TagClient) {
    let page = state.panel.as_ref().map(|panel| panel.page).unwrap_or(1);
    match client.list_tags(page, TAG_PAGE_SIZE, "/").await {
        Ok(page_data) => {
            if let Some(panel) = state.panel.as_mut() {
                panel.apply_page(page, page_data.total, page_data.items);
            }
        }
        Err(e) => {
            if let Some(panel) = state.panel.as_mut() {
                panel.status = Some(format!("error: {}", e.message()));
            }
        }
    }
}

/// Full interactive run: TTY check → terminal guard → event loop with exactly
/// one fetch per navigation action and no background work.
pub async fn run(
    server: &str,
    start_path: &str,
    git_ref: Option<&str>,
    token: Option<Mega2Token>,
) -> CliResult<()> {
    ensure_tty()?;
    let session = Mega2TreeSession::new(server)?;
    let mut state = BrowserState::new(session, start_path, git_ref.map(str::to_string))?;
    let entry_client = Mega2EntryClient::new(server, token.clone())?;
    let mutate_client = Mega2MutateClient::new(server, token.clone())?;
    let tag_client = Mega2TagClient::new(server, token)?;
    let mut guard = TerminalGuard::enter()?;

    state.fetch_current().await?;

    let result = (async {
        let mut stdin = io::stdin();
        let mut stdout = io::stdout();
        let mut out_buf: Vec<u8> = Vec::with_capacity(4096);
        while state.running {
            out_buf.clear();
            out_buf.extend_from_slice(render(&state, server).as_bytes());
            stdout.write_all(&out_buf).map_err(|e| {
                CliError::fatal(format!("mega2 browser: output write failed: {e}"))
                    .with_stable_code(StableErrorCode::IoWriteFailed)
            })?;
            stdout.flush().map_err(|e| {
                CliError::fatal(format!("mega2 browser: output flush failed: {e}"))
                    .with_stable_code(StableErrorCode::IoWriteFailed)
            })?;

            let key = read_key(&mut stdin).map_err(|e| {
                CliError::fatal(format!("mega2 browser: input read failed: {e}"))
                    .with_stable_code(StableErrorCode::IoReadFailed)
            })?;
            let action = state.handle_key(key);
            match action {
                ActionResult::Quit => break,
                ActionResult::FetchCurrent => {
                    if let Err(e) = state.fetch_current().await {
                        // Keep the last safe listing; show a secret-free status.
                        state.status = Some(format!("error: {}", e.message()));
                    }
                }
                ActionResult::CreateDirectory { parent, name } => {
                    if let Err(e) = perform_create(&mut state, &entry_client, &parent, &name).await
                    {
                        // 401/403/duplicate/timeout: keep the last safe listing and a
                        // secret-free status line; raw mode is untouched.
                        state.status = Some(format!("error: {}", e.message()));
                    }
                }
                ActionResult::DeleteDirectory { parent, name } => {
                    if let Err(e) = perform_delete(&mut state, &mutate_client, &parent, &name).await
                    {
                        // 401/403/missing/timeout: keep the last safe listing and a
                        // secret-free status line; raw mode is untouched.
                        state.status = Some(format!("error: {}", e.message()));
                    }
                }
                ActionResult::MoveDirectory {
                    from_parent,
                    from_name,
                    to_parent,
                    to_name,
                } => {
                    if let Err(e) = perform_move(
                        &mut state,
                        &mutate_client,
                        &from_parent,
                        &from_name,
                        &to_parent,
                        &to_name,
                    )
                    .await
                    {
                        state.status = Some(format!("error: {}", e.message()));
                    }
                }
                ActionResult::FetchTags { page } => {
                    if let Err(e) = perform_fetch_tags(&mut state, &tag_client, page).await
                        && let Some(panel) = state.panel.as_mut()
                    {
                        panel.status = Some(format!("error: {}", e.message()));
                    }
                }
                ActionResult::CreateTag { name, message } => {
                    if let Err(e) =
                        perform_create_tag(&mut state, &tag_client, &name, message.as_deref()).await
                        && let Some(panel) = state.panel.as_mut()
                    {
                        panel.status = Some(format!("error: {}", e.message()));
                    }
                }
                ActionResult::DeleteTag { name } => {
                    if let Err(e) = perform_delete_tag(&mut state, &tag_client, &name).await
                        && let Some(panel) = state.panel.as_mut()
                    {
                        panel.status = Some(format!("error: {}", e.message()));
                    }
                }
                ActionResult::Continue => {}
            }
        }
        Ok::<(), CliError>(())
    })
    .await;

    // Terminal restoration is mandatory on every exit path; restoration
    // failure must not mask the primary error.
    if let Err(restore_error) = guard.restore() {
        match result {
            Ok(()) => Err(CliError::fatal(format!(
                "mega2 browser: failed to restore the terminal: {restore_error}"
            ))
            .with_stable_code(StableErrorCode::IoWriteFailed)),
            Err(primary) => {
                let _ = restore_error;
                Err(primary)
            }
        }
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::TcpListener,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        thread,
        time::Duration,
    };

    use super::*;

    /// Minimal blocking mock of the mega2 tree route; counts requests.
    struct MockTreeServer {
        addr: std::net::SocketAddr,
        requests: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
    }

    impl MockTreeServer {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
            listener.set_nonblocking(true).expect("nonblocking");
            let addr = listener.local_addr().expect("addr");
            let requests = Arc::new(AtomicUsize::new(0));
            let stop = Arc::new(AtomicBool::new(false));
            let requests_clone = Arc::clone(&requests);
            let stop_clone = Arc::clone(&stop);
            thread::spawn(move || {
                let body = r#"{"req_result":true,"data":{"tree_items":[{"name":"sub","path":"/","content_type":"directory"},{"name":"file.txt","path":"/","content_type":"file"}]},"err_message":""}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                while !stop_clone.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            use std::io::{Read, Write};
                            let mut buf = [0u8; 8192];
                            let _ = stream.read(&mut buf);
                            requests_clone.fetch_add(1, Ordering::SeqCst);
                            let _ = stream.write_all(response.as_bytes());
                            let _ = stream.flush();
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                addr,
                requests,
                stop,
            }
        }

        fn url(&self) -> String {
            format!("http://{}", self.addr)
        }

        fn requests(&self) -> usize {
            self.requests.load(Ordering::SeqCst)
        }
    }

    impl Drop for MockTreeServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
        }
    }

    fn state_on(server: &str) -> BrowserState {
        let session = Mega2TreeSession::new(server).expect("session");
        BrowserState::new(session, "/", None).expect("state")
    }

    #[tokio::test]
    async fn navigation_keys_fetch_exactly_once_and_files_are_inert() {
        let server = MockTreeServer::start();
        let mut state = state_on(&server.url());

        state.fetch_current().await.expect("initial listing");
        assert_eq!(server.requests(), 1);

        // Cursor motion and reload-free keys never touch the network.
        assert_eq!(state.handle_key(Key::Down), ActionResult::Continue);
        assert_eq!(state.handle_key(Key::Up), ActionResult::Continue);
        assert_eq!(server.requests(), 1);

        // Enter on the directory requests exactly one fetch once applied.
        assert_eq!(state.handle_key(Key::Enter), ActionResult::FetchCurrent);
        assert_eq!(state.path, "/sub");
        state.fetch_current().await.expect("child listing");
        assert_eq!(server.requests(), 2);

        // Enter on a file is inert (status only, no fetch).
        state.selection = 1;
        assert_eq!(state.handle_key(Key::Enter), ActionResult::Continue);
        assert!(state.status.as_deref().unwrap_or("").contains("file.txt"));
        assert_eq!(server.requests(), 2);

        // Back to root requests one fetch; back at root is inert.
        assert_eq!(state.handle_key(Key::Backspace), ActionResult::FetchCurrent);
        assert_eq!(state.path, "/");
        state.fetch_current().await.expect("root listing");
        assert_eq!(server.requests(), 3);
        assert_eq!(state.handle_key(Key::Backspace), ActionResult::Continue);
        assert_eq!(server.requests(), 3);

        // Quit stops the loop without network activity.
        assert_eq!(state.handle_key(Key::Quit), ActionResult::Quit);
        assert!(!state.running);
        assert_eq!(server.requests(), 3);
    }

    #[test]
    fn path_never_climbs_above_root() {
        assert_eq!(BrowserState::parent_path("/"), "/");
        assert_eq!(BrowserState::parent_path("/a"), "/");
        assert_eq!(BrowserState::parent_path("/a/b"), "/a");
        assert_eq!(BrowserState::child_path("/", "x"), "/x");
        assert_eq!(BrowserState::child_path("/a", "b"), "/a/b");
    }

    #[test]
    fn parse_key_maps_arrows_and_control_keys() {
        assert_eq!(parse_key(0x1b, b"[A"), Key::Up);
        assert_eq!(parse_key(0x1b, b"[B"), Key::Down);
        assert_eq!(parse_key(b'\r', &[]), Key::Enter);
        assert_eq!(parse_key(127, &[]), Key::Backspace);
        assert_eq!(parse_key(8, &[]), Key::Backspace);
        assert_eq!(parse_key(b'q', &[]), Key::Quit);
        assert_eq!(parse_key(b'r', &[]), Key::Reload);
        assert_eq!(parse_key(b'h', &[]), Key::Home);
        assert_eq!(parse_key(b'x', &[]), Key::Other('x'));
    }

    #[test]
    fn hostile_text_is_sanitized_in_render() {
        assert_eq!(sanitize("a\u{1b}[31mb"), "a?[31mb");
        let session = Mega2TreeSession::new("https://example.com").expect("session");
        let mut state = BrowserState::new(session, "/", None).expect("state");
        state.entries = vec![("evil\u{1b}[2Jname".to_string(), ContentType::Directory)];
        let frame = render(&state, "https://example.com");
        assert!(!frame.contains("evil\u{1b}[2Jname"), "raw escape leaked");
        assert!(frame.contains("evil?[2Jname"), "sanitized name shown");
    }

    #[test]
    fn tty_and_platform_gates_fail_closed() {
        assert!(tty_required(false, true).is_err());
        assert!(tty_required(true, false).is_err());
        assert!(tty_required(true, true).is_ok());
        assert!(terminal::select_platform_impl(false, false).is_err());
        assert!(terminal::select_platform_impl(true, false).is_ok());
        assert!(terminal::select_platform_impl(false, true).is_ok());
    }

    #[test]
    fn tty_gate_error_is_not_an_internal_invariant() {
        let err = tty_required(false, false).expect_err("non-tty refused");
        assert_eq!(err.stable_code(), StableErrorCode::Unsupported);
        assert!(err.message().contains("requires stdin and stdout"));
    }
}
