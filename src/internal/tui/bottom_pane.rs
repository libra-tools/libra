//! Bottom pane component with input area and status bar.
//!
//! Provides the user input area and status display at the bottom of the TUI.

use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

// SAFETY: The unwrap() calls in this module are generally safe because:
// 1. They operate on data structures with guaranteed invariants (e.g., string indices)
// 2. They are used in rendering where dimensions are pre-validated
// 3. Test code uses unwrap for test assertions
use ratatui::{
    prelude::*,
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::{app_event::AgentStatus, theme};
use crate::internal::ai::{
    sandbox::{ExecApprovalRequest, NetworkAccess},
    tools::context::UserInputQuestion,
};

/// Snapshot of user-input question data for rendering (avoids borrowing the request).
#[derive(Clone, Debug)]
struct UserInputQuestionSnapshot {
    header: String,
    question: String,
    /// `None` means freeform (text-only).
    options: Option<Vec<(String, String)>>, // (label, description)
    /// Whether a "None of the above" option should be appended.
    is_other: bool,
    /// Whether typed text should be masked (reserved for future use).
    #[allow(dead_code)]
    is_secret: bool,
}

#[derive(Clone, Debug)]
struct ExecApprovalSnapshot {
    title: String,
    command: String,
    cwd: String,
    reason: Option<String>,
    is_retry: bool,
    sandbox_label: String,
    network_access: Option<NetworkAccess>,
    writable_roots: Vec<String>,
    options: Vec<(String, String)>,
}

/// State for the slash-command autocomplete popup.
#[derive(Debug)]
struct CommandPopupState {
    /// Known commands: `(name, description)`, set once at startup.
    commands: Vec<(String, String)>,
    /// Whether the popup is currently visible.
    visible: bool,
    /// Index of the currently highlighted command in the *filtered* list.
    selected: usize,
    /// First visible item in the filtered popup list.
    scroll_offset: usize,
}

const COMMAND_POPUP_MAX_VISIBLE: usize = 8;

/// The bottom pane containing input area and status.
#[derive(Debug)]
pub struct BottomPane {
    /// Current input text.
    pub input: String,
    /// Cursor position in the input.
    pub cursor_pos: usize,
    /// Current agent status.
    pub status: AgentStatus,
    /// Whether the input is focused.
    pub focused: bool,
    /// Snapshot of the current user-input questions (while awaiting input).
    user_input_questions: Option<Vec<UserInputQuestionSnapshot>>,
    /// Index of the question currently being answered (driven by App).
    pub user_input_current_question: usize,
    /// Currently selected option index (driven by App).
    pub user_input_selected_option: usize,
    /// Whether the notes input is currently focused (driven by App).
    pub user_input_notes_focused: bool,
    /// Current notes text (driven by App).
    pub user_input_notes_text: String,
    /// Snapshot of the current exec approval request (while awaiting approval).
    exec_approval: Option<ExecApprovalSnapshot>,
    /// Currently selected exec approval option.
    pub exec_approval_selected: usize,
    /// Slash-command autocomplete popup state.
    command_popup: CommandPopupState,
    /// Currently selected option in the post-plan dialog.
    pub post_plan_selected: usize,
    /// Current network access choice shown in the post-plan dialog.
    pub post_plan_network_access: bool,
    /// Current working directory shown in the input border.
    cwd: Option<PathBuf>,
    /// Current Git branch or detached HEAD label shown beside the working directory.
    git_branch: Option<String>,
    /// Current retry notice shown in the status line.
    retry_notice: Option<String>,
    /// Optional context label shown on the input box title.
    input_context_label: Option<String>,
    /// Optional placeholder override for local TUI controls.
    input_hint: Option<String>,
    /// Optional compact usage line shown between composer and status.
    usage_line: Option<String>,
    /// Optional usage detail popup shown above the bottom pane.
    usage_detail_panel: Option<String>,
    /// Optional one-line Goal status indicator. When `Some`, the
    /// pane prepends the line above the input border so the user
    /// always sees the active Goal's id-short-code + status without
    /// running `/goal status`. Per `docs/development/commands/_general.md`
    /// line 723: "active Goal 的底栏至少显示：goal id 短码、status、
    /// 当前 step、tokens/cost、blocker 或 next action". This field
    /// owns the rendered string so the App can update it after
    /// every Goal mutation without touching the renderer's state
    /// machine.
    goal_status_line: Option<String>,
}

impl BottomPane {
    /// Create a new bottom pane.
    pub fn new() -> Self {
        Self {
            input: String::new(),
            cursor_pos: 0,
            status: AgentStatus::Idle,
            focused: true,
            user_input_questions: None,
            user_input_current_question: 0,
            user_input_selected_option: 0,
            user_input_notes_focused: false,
            user_input_notes_text: String::new(),
            exec_approval: None,
            exec_approval_selected: 0,
            command_popup: CommandPopupState {
                commands: Vec::new(),
                visible: false,
                selected: 0,
                scroll_offset: 0,
            },
            post_plan_selected: 0,
            post_plan_network_access: false,
            cwd: None,
            git_branch: None,
            retry_notice: None,
            input_context_label: None,
            input_hint: None,
            usage_line: None,
            usage_detail_panel: None,
            goal_status_line: None,
        }
    }

    /// Store (or clear) the user-input questions to render.
    pub fn set_user_input_questions(&mut self, questions: Option<&[UserInputQuestion]>) {
        self.user_input_questions = questions.map(|qs| {
            qs.iter()
                .map(|q| UserInputQuestionSnapshot {
                    header: q.header.clone(),
                    question: q.question.clone(),
                    options: q.options.as_ref().map(|opts| {
                        opts.iter()
                            .map(|o| (o.label.clone(), o.description.clone()))
                            .collect()
                    }),
                    is_other: q.is_other,
                    is_secret: q.is_secret,
                })
                .collect()
        });
        self.user_input_current_question = 0;
        self.user_input_selected_option = 0;
        self.user_input_notes_focused = false;
        self.user_input_notes_text.clear();
    }

    pub fn set_exec_approval(&mut self, request: Option<&ExecApprovalRequest>) {
        if let Some(request) = request {
            let reason = approval_dialog_reason(request);
            let options = if request.cache_disabled_reason.is_some() {
                vec![
                    (
                        "Approve Once".to_string(),
                        "Allow this execution once; this decision will not be cached".to_string(),
                    ),
                    ("Deny".to_string(), "Reject this execution".to_string()),
                    (
                        "Abort Turn".to_string(),
                        "Interrupt the current turn".to_string(),
                    ),
                ]
            } else {
                vec![
                    (
                        "Approve".to_string(),
                        "Allow this execution once".to_string(),
                    ),
                    (
                        "Approve Session".to_string(),
                        "Allow this exact command for this session".to_string(),
                    ),
                    (
                        "Approve TTL".to_string(),
                        "Allow this exact command until the approval TTL expires".to_string(),
                    ),
                    (
                        "Directory TTL".to_string(),
                        "Allow matching command families in this directory until TTL expires"
                            .to_string(),
                    ),
                    (
                        "Pattern TTL".to_string(),
                        "Allow commands with the same argument pattern until TTL expires"
                            .to_string(),
                    ),
                    (
                        "Allow All Commands".to_string(),
                        "Requires a second confirmation; use only for trusted work".to_string(),
                    ),
                    ("Deny".to_string(), "Reject this execution".to_string()),
                    (
                        "Abort Turn".to_string(),
                        "Interrupt the current turn".to_string(),
                    ),
                ]
            };
            self.set_approval_dialog(
                "Sandbox approval required".to_string(),
                request.command.clone(),
                request.cwd.clone(),
                reason,
                request.is_retry,
                request.sandbox_label.clone(),
                Some(request.network_access.clone()),
                request.writable_roots.clone(),
                options,
            );
        } else {
            self.exec_approval = None;
            self.exec_approval_selected = 0;
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn set_approval_dialog(
        &mut self,
        title: String,
        command: String,
        cwd: PathBuf,
        reason: Option<String>,
        is_retry: bool,
        sandbox_label: String,
        network_access: Option<NetworkAccess>,
        writable_roots: Vec<PathBuf>,
        options: Vec<(String, String)>,
    ) {
        self.exec_approval = Some(ExecApprovalSnapshot {
            title,
            command,
            cwd: cwd.display().to_string(),
            reason,
            is_retry,
            sandbox_label,
            network_access,
            writable_roots: writable_roots
                .iter()
                .map(|path| path.display().to_string())
                .collect(),
            options,
        });
        self.exec_approval_selected = 0;
    }

    /// Reset the post-plan dialog selection.
    pub fn reset_post_plan_selection(&mut self) {
        self.post_plan_selected = 0;
    }

    pub fn set_post_plan_network_access(&mut self, network_access: bool) {
        self.post_plan_network_access = network_access;
    }

    /// Handle a character input.
    pub fn insert_char(&mut self, c: char) {
        self.input.insert(self.cursor_pos, c);
        self.cursor_pos += c.len_utf8();
    }

    /// Insert pasted text at the cursor.
    pub fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.input.insert_str(self.cursor_pos, text);
        self.cursor_pos += text.len();
    }

    /// Handle backspace.
    pub fn backspace(&mut self) {
        if self.cursor_pos > 0 {
            // Find the start of the previous character
            let prev_pos = self.prev_char_pos();
            self.input.remove(prev_pos);
            self.cursor_pos = prev_pos;
        }
    }

    /// Handle delete.
    pub fn delete(&mut self) {
        if self.cursor_pos < self.input.len() {
            self.input.remove(self.cursor_pos);
        }
    }

    /// Move cursor left.
    pub fn cursor_left(&mut self) {
        if self.cursor_pos > 0 {
            self.cursor_pos = self.prev_char_pos();
        }
    }

    /// Move cursor right.
    pub fn cursor_right(&mut self) {
        if self.cursor_pos < self.input.len() {
            self.cursor_pos = self.next_char_pos();
        }
    }

    /// Move cursor to start.
    pub fn cursor_home(&mut self) {
        self.cursor_pos = 0;
    }

    /// Move cursor to end.
    pub fn cursor_end(&mut self) {
        self.cursor_pos = self.input.len();
    }

    /// Clear the input.
    pub fn clear(&mut self) {
        self.input.clear();
        self.cursor_pos = 0;
    }

    /// Get the current input text and clear.
    pub fn take_input(&mut self) -> String {
        let input = std::mem::take(&mut self.input);
        self.cursor_pos = 0;
        input
    }

    /// Set the agent status.
    pub fn set_status(&mut self, status: AgentStatus) {
        self.status = status;
        if status != AgentStatus::Retrying {
            self.retry_notice = None;
        }
    }

    /// Set the current working directory badge shown on the input border.
    pub fn set_cwd(&mut self, cwd: PathBuf) {
        self.cwd = Some(cwd);
    }

    /// Set or clear the Git branch label shown on the input border.
    pub fn set_git_branch(&mut self, git_branch: Option<String>) {
        self.git_branch = git_branch;
    }

    /// Show a transient retry notice in the status line.
    pub fn set_retry_notice(&mut self, notice: String) {
        self.retry_notice = Some(notice);
        self.status = AgentStatus::Retrying;
    }

    /// Set or clear a contextual title for the shared input box.
    pub fn set_input_context_label(&mut self, label: Option<String>) {
        self.input_context_label = label;
    }

    /// Set or clear a placeholder override for the shared input box.
    pub fn set_input_hint(&mut self, hint: Option<String>) {
        self.input_hint = hint;
    }

    /// Set or clear the compact model usage line.
    pub fn set_usage_line(&mut self, usage_line: Option<String>) {
        self.usage_line = usage_line;
    }

    /// Set or clear the one-line Goal status indicator surfaced
    /// above the input box. Pass `None` when the session has no
    /// active Goal; pass `Some(line)` when a Goal is active so the
    /// user can see its id-short-code and progress without invoking
    /// `/goal status`. See [`crate::internal::tui::goal_session::render_goal_status_line`]
    /// for the canonical formatter.
    pub fn set_goal_status_line(&mut self, line: Option<String>) {
        self.goal_status_line = line;
    }

    /// Read-only access to the current Goal status line. Used by
    /// the render path and by tests to assert that the App's
    /// post-mutation refresh reached the pane.
    #[cfg(test)]
    pub fn goal_status_line(&self) -> Option<&str> {
        self.goal_status_line.as_deref()
    }

    pub fn set_usage_detail_panel(&mut self, details: Option<String>) {
        self.usage_detail_panel = details;
    }

    pub fn dismiss_usage_detail_panel(&mut self) {
        self.usage_detail_panel = None;
    }

    pub fn is_usage_detail_panel_visible(&self) -> bool {
        self.usage_detail_panel.is_some()
    }

    // ── Slash-command autocomplete popup ────────────────────────────

    /// Set the known slash commands (called once at startup).
    pub fn set_command_hints(&mut self, commands: Vec<(String, String)>) {
        self.command_popup.commands = commands;
    }

    /// Whether the command popup is currently visible.
    pub fn is_command_popup_visible(&self) -> bool {
        self.command_popup.visible
    }

    /// Synchronise popup visibility after every input mutation.
    ///
    /// Shows the popup when the input starts with `/` and contains no space;
    /// hides it otherwise. Clamps the selection index to the filtered list.
    pub fn sync_command_popup(&mut self) {
        let should_show = self.input.starts_with('/')
            && !self.input.contains(' ')
            && !self.command_popup.commands.is_empty();

        self.command_popup.visible = should_show;

        if should_show {
            let count = self.filtered_commands().len();
            if count == 0 {
                self.command_popup.visible = false;
                self.command_popup.scroll_offset = 0;
            } else if self.command_popup.selected >= count {
                self.command_popup.selected = count.saturating_sub(1);
            }
            self.ensure_command_popup_selection_visible(count);
        } else {
            self.command_popup.selected = 0;
            self.command_popup.scroll_offset = 0;
        }
    }

    /// Hide the popup (Esc).
    pub fn dismiss_command_popup(&mut self) {
        self.command_popup.visible = false;
        self.command_popup.selected = 0;
        self.command_popup.scroll_offset = 0;
    }

    /// Move selection up in the popup.
    pub fn command_popup_up(&mut self) {
        if self.command_popup.selected > 0 {
            self.command_popup.selected -= 1;
            self.ensure_command_popup_selection_visible(self.filtered_commands().len());
        }
    }

    /// Move selection down in the popup.
    pub fn command_popup_down(&mut self) {
        let count = self.filtered_commands().len();
        if count > 0 && self.command_popup.selected < count - 1 {
            self.command_popup.selected += 1;
            self.ensure_command_popup_selection_visible(count);
        }
    }

    /// Complete the selected command (Tab).
    ///
    /// Replaces the input with `/<name> ` and moves cursor to end.
    /// Returns `true` if a completion was performed.
    pub fn complete_command(&mut self) -> bool {
        let filtered = self.filtered_commands();
        if let Some((name, _)) = filtered.get(self.command_popup.selected) {
            let completed = format!("/{} ", name);
            self.input = completed;
            self.cursor_pos = self.input.len();
            self.command_popup.visible = false;
            self.command_popup.selected = 0;
            self.command_popup.scroll_offset = 0;
            true
        } else {
            false
        }
    }

    /// Return the commands matching the current prefix (after `/`), case-insensitive.
    fn filtered_commands(&self) -> Vec<(String, String)> {
        let prefix = self.input.strip_prefix('/').unwrap_or("");
        let prefix_lower = prefix.to_lowercase();
        self.command_popup
            .commands
            .iter()
            .filter(|(name, _)| name.to_lowercase().starts_with(&prefix_lower))
            .cloned()
            .collect()
    }

    fn ensure_command_popup_selection_visible(&mut self, count: usize) {
        if count <= COMMAND_POPUP_MAX_VISIBLE {
            self.command_popup.scroll_offset = 0;
            return;
        }

        if self.command_popup.selected < self.command_popup.scroll_offset {
            self.command_popup.scroll_offset = self.command_popup.selected;
        } else {
            let end = self.command_popup.scroll_offset + COMMAND_POPUP_MAX_VISIBLE;
            if self.command_popup.selected >= end {
                self.command_popup.scroll_offset =
                    self.command_popup.selected + 1 - COMMAND_POPUP_MAX_VISIBLE;
            }
        }

        let max_offset = count.saturating_sub(COMMAND_POPUP_MAX_VISIBLE);
        self.command_popup.scroll_offset = self.command_popup.scroll_offset.min(max_offset);
    }

    /// Check if input is empty.
    pub fn is_empty(&self) -> bool {
        self.input.is_empty()
    }

    /// Return the height (in lines) the bottom pane needs for the current state.
    pub fn desired_height(&self) -> u16 {
        if self.status == AgentStatus::AwaitingApproval {
            let options = self
                .exec_approval
                .as_ref()
                .map(|approval| approval.options.len() as u16)
                .unwrap_or(5);
            // status(1) + summary(7) + options + help(1)
            return 1 + 7 + options + 1;
        }
        if self.status == AgentStatus::AwaitingPostPlanChoice {
            // status(1) + 3 options + 1 blank + help(1) = 6
            return 6;
        }
        if self.status == AgentStatus::AwaitingNetworkPolicyChoice {
            // status(1) + 3 options + 1 blank + help(1) = 6
            return 6;
        }
        if self.status == AgentStatus::AwaitingIntentReviewChoice {
            // status(1) + 3 options + 1 blank + help(1) = 6
            return 6;
        }
        if self.status != AgentStatus::AwaitingUserInput {
            // Normal mode: rounded input box(5, with 3-line input inner area)
            // + optional Goal status line + optional usage line + statusline(1).
            let mut height: u16 = 6;
            if self.usage_line.is_some() {
                height += 1;
            }
            if self.goal_status_line.is_some() {
                height += 1;
            }
            return height;
        }

        let questions = match &self.user_input_questions {
            Some(q) => q,
            None => return 5,
        };

        let q_idx = self.user_input_current_question;
        let question = match questions.get(q_idx) {
            Some(q) => q,
            None => return 5,
        };

        let options = question.options.as_deref().unwrap_or_default();
        let is_freeform = options.is_empty();

        let option_lines = if is_freeform {
            0u16
        } else {
            let extra = if question.is_other { 1 } else { 0 };
            options.len() as u16 + extra
        };

        let question_area = 1 + 1 + option_lines; // header + question + options
        let notes_height = if !is_freeform { 3u16 } else { 0 };

        // status(1) + question_area + input(3) + notes + help(1)
        1 + question_area + 3 + notes_height + 1
    }

    /// Render the bottom pane.
    pub fn render(&self, area: Rect, buf: &mut Buffer) -> Option<Position> {
        let area = area.intersection(*buf.area());
        if area.width == 0 || area.height == 0 {
            return None;
        }

        if self.status == AgentStatus::AwaitingUserInput {
            return self.render_user_input_mode(area, buf);
        }
        if self.status == AgentStatus::AwaitingApproval {
            return self.render_exec_approval_dialog(area, buf);
        }
        if self.status == AgentStatus::AwaitingPostPlanChoice {
            return self.render_post_plan_dialog(area, buf);
        }
        if self.status == AgentStatus::AwaitingNetworkPolicyChoice {
            return self.render_network_policy_dialog(area, buf);
        }
        if self.status == AgentStatus::AwaitingIntentReviewChoice {
            return self.render_intent_review_dialog(area, buf);
        }

        // Split area into input area, optional Goal status row,
        // optional usage row, and status bar. Goal status sits
        // directly under the input so it's the first thing the eye
        // hits after typing.
        let mut constraints: Vec<Constraint> = Vec::with_capacity(4);
        constraints.push(Constraint::Length(5)); // Rounded input box (3-line inner input)
        if self.goal_status_line.is_some() {
            constraints.push(Constraint::Length(1));
        }
        if self.usage_line.is_some() {
            constraints.push(Constraint::Length(1));
        }
        constraints.push(Constraint::Length(1)); // Status line
        let chunks = Layout::vertical(constraints).split(area);

        // Render input area
        let cursor_pos = self.render_input_area(chunks[0], buf);
        let mut next_chunk = 1usize;
        if self.goal_status_line.is_some() {
            self.render_goal_status_line(chunks[next_chunk], buf);
            next_chunk += 1;
        }
        if self.usage_line.is_some() {
            self.render_usage_line(chunks[next_chunk], buf);
            next_chunk += 1;
        }
        self.render_status_bar(chunks[next_chunk], buf);

        // Render command popup (floats above the bottom pane)
        if self.command_popup.visible && self.status == AgentStatus::Idle {
            self.render_command_popup(area, buf);
        }
        if self.usage_detail_panel.is_some() && !self.command_popup.visible {
            self.render_usage_detail_panel(area, buf);
        }

        cursor_pos
    }

    /// Return the clickable input hitbox for focus handling.
    pub fn input_hitbox(&self, area: Rect) -> Option<Rect> {
        if self.status == AgentStatus::AwaitingApproval {
            return None;
        }
        if self.status == AgentStatus::AwaitingPostPlanChoice {
            return None;
        }
        if self.status == AgentStatus::AwaitingNetworkPolicyChoice {
            return None;
        }

        if self.status != AgentStatus::AwaitingUserInput {
            let constraints = if self.usage_line.is_some() {
                vec![
                    Constraint::Length(5),
                    Constraint::Length(1),
                    Constraint::Length(1),
                ]
            } else {
                vec![Constraint::Length(5), Constraint::Length(1)]
            };
            let chunks = Layout::vertical(constraints).split(area);
            return Some(chunks[0]);
        }

        let questions = self.user_input_questions.as_ref()?;
        let question = questions.get(self.user_input_current_question)?;
        let options = question.options.as_deref().unwrap_or_default();
        let is_freeform = options.is_empty();

        let option_lines = if is_freeform {
            0u16
        } else {
            let extra = if question.is_other { 1u16 } else { 0 };
            options.len() as u16 + extra
        };
        let question_area_height = 1 + 1 + option_lines;
        let notes_height = if !is_freeform { 3u16 } else { 0 };

        let chunks = Layout::vertical([
            Constraint::Length(1),                    // Status bar
            Constraint::Length(question_area_height), // Question + options
            Constraint::Length(3),                    // Freeform input
            Constraint::Length(notes_height),         // Notes
            Constraint::Length(1),                    // Help text
        ])
        .split(area);

        let hit = if is_freeform { chunks[2] } else { chunks[3] };
        if hit.width > 0 && hit.height > 0 {
            Some(hit)
        } else {
            None
        }
    }

    /// Render the bottom pane in user-input mode (questions + options).
    fn render_user_input_mode(&self, area: Rect, buf: &mut Buffer) -> Option<Position> {
        let questions = self.user_input_questions.as_ref()?;

        let q_idx = self.user_input_current_question;
        let question = questions.get(q_idx)?;
        let total_questions = questions.len();

        let options = question.options.as_deref().unwrap_or_default();
        let is_freeform = options.is_empty();

        // Count how many lines we need for the question display
        let option_lines = if is_freeform {
            0u16
        } else {
            let extra = if question.is_other { 1u16 } else { 0 };
            options.len() as u16 + extra
        };
        let question_area_height = 1 + 1 + option_lines; // progress + question + options
        let notes_height = if !is_freeform { 3u16 } else { 0 }; // notes area (only for option questions)

        let chunks = Layout::vertical([
            Constraint::Length(1),                    // Status bar
            Constraint::Length(question_area_height), // Question + options
            Constraint::Length(3),                    // Input area (freeform or notes)
            Constraint::Length(notes_height),         // Notes area (for option questions)
            Constraint::Length(1),                    // Help text
        ])
        .split(area);

        // Status bar
        let progress = if total_questions > 1 {
            format!(
                "● Question {}/{} — Awaiting input...",
                q_idx + 1,
                total_questions
            )
        } else {
            "● Awaiting input...".to_string()
        };
        let status_line = Line::styled(progress, theme::status::pending_input());
        Paragraph::new(status_line).render(chunks[0], buf);

        // Question display
        let mut lines: Vec<Line<'static>> = Vec::new();

        // Header
        lines.push(Line::styled(
            format!("  {}", question.header),
            theme::interactive::title(),
        ));

        // Question text
        lines.push(Line::styled(
            format!("  {}", question.question),
            theme::text::primary(),
        ));

        let selected = self.user_input_selected_option;

        if !is_freeform {
            // Render predefined options
            for (i, (label, description)) in options.iter().enumerate() {
                let marker = if i == selected && !self.user_input_notes_focused {
                    "›"
                } else {
                    " "
                };
                let style = if i == selected {
                    theme::interactive::selected_option()
                } else {
                    theme::text::primary()
                };
                lines.push(Line::styled(
                    format!("  {} {}. {}  {}", marker, i + 1, label, description),
                    style,
                ));
            }

            // "None of the above" option (when is_other is set)
            if question.is_other {
                let other_idx = options.len();
                let marker = if selected == other_idx && !self.user_input_notes_focused {
                    "›"
                } else {
                    " "
                };
                let style = if selected == other_idx {
                    theme::interactive::selected_option()
                } else {
                    theme::text::muted().add_modifier(Modifier::DIM)
                };
                lines.push(Line::styled(
                    format!(
                        "  {} {}. None of the above  Optionally, add details in notes (tab).",
                        marker,
                        other_idx + 1
                    ),
                    style,
                ));
            }
        }

        // Render question area (clip to available height)
        let max_lines = chunks[1].height as usize;
        let display_lines: Vec<Line<'static>> = lines.into_iter().take(max_lines).collect();
        let text = Text::from(display_lines);
        Paragraph::new(text).render(chunks[1], buf);

        // Input area — for freeform questions, this is the primary input.
        // For option questions, this area is used for notes when Tab is pressed.
        let cursor_pos = if is_freeform {
            self.render_input_area(chunks[2], buf)
        } else {
            // Show notes input area
            self.render_notes_area(chunks[3], buf)
        };

        // Help text
        let help = if is_freeform {
            "[Enter: Submit] [Esc: Cancel]"
        } else {
            "[↑/↓: Select] [1-9: Quick select] [Tab: Notes] [Enter: Submit] [Esc: Cancel]"
        };
        let help_line = Line::styled(help, theme::text::help());
        Paragraph::new(help_line).render(chunks[4], buf);

        // Show cursor: for freeform always, for options only when notes focused
        if is_freeform || self.user_input_notes_focused {
            cursor_pos
        } else {
            None
        }
    }

    /// Render the post-plan dialog (Execute Plan / Modify Plan / Cancel).
    fn render_post_plan_dialog(&self, area: Rect, buf: &mut Buffer) -> Option<Position> {
        self.render_choice_dialog(area, buf, &Self::post_plan_options())
    }

    /// Render the network policy dialog shown after the developer approves the plan.
    fn render_network_policy_dialog(&self, area: Rect, buf: &mut Buffer) -> Option<Position> {
        self.render_choice_dialog(area, buf, &Self::network_policy_options())
    }

    /// Render the IntentSpec dialog (Confirm / Modify / Cancel).
    fn render_intent_review_dialog(&self, area: Rect, buf: &mut Buffer) -> Option<Position> {
        self.render_choice_dialog(area, buf, &Self::intent_review_options())
    }

    fn intent_review_options() -> [(&'static str, &'static str); 3] {
        [
            (
                super::workflow_baseline::INTENT_REVIEW_CHOICES[0],
                super::workflow_baseline::INTENT_REVIEW_HELP[0],
            ),
            (
                super::workflow_baseline::INTENT_REVIEW_CHOICES[1],
                super::workflow_baseline::INTENT_REVIEW_HELP[1],
            ),
            (
                super::workflow_baseline::INTENT_REVIEW_CHOICES[2],
                super::workflow_baseline::INTENT_REVIEW_HELP[2],
            ),
        ]
    }

    fn post_plan_options() -> [(&'static str, &'static str); 3] {
        [
            (
                super::workflow_baseline::POST_PLAN_CHOICES[0],
                super::workflow_baseline::POST_PLAN_HELP[0],
            ),
            (
                super::workflow_baseline::POST_PLAN_CHOICES[1],
                super::workflow_baseline::POST_PLAN_HELP[1],
            ),
            (
                super::workflow_baseline::POST_PLAN_CHOICES[2],
                super::workflow_baseline::POST_PLAN_HELP[2],
            ),
        ]
    }

    fn network_policy_options() -> [(&'static str, &'static str); 3] {
        [
            (
                super::workflow_baseline::NETWORK_POLICY_CHOICES[0],
                super::workflow_baseline::NETWORK_POLICY_HELP[0],
            ),
            (
                super::workflow_baseline::NETWORK_POLICY_CHOICES[1],
                super::workflow_baseline::NETWORK_POLICY_HELP[1],
            ),
            (
                super::workflow_baseline::NETWORK_POLICY_CHOICES[2],
                super::workflow_baseline::NETWORK_POLICY_HELP[2],
            ),
        ]
    }

    fn render_choice_dialog(
        &self,
        area: Rect,
        buf: &mut Buffer,
        options: &[(&str, &str)],
    ) -> Option<Position> {
        let chunks = Layout::vertical([
            Constraint::Length(1),                        // Status bar
            Constraint::Length(options.len() as u16 + 1), // options + 1 blank line
            Constraint::Length(1),                        // Help text
        ])
        .split(area);

        // Status bar
        self.render_status_bar(chunks[0], buf);

        let mut lines: Vec<Line<'static>> = Vec::new();
        for (i, (label, desc)) in options.iter().enumerate() {
            let marker = if i == self.post_plan_selected {
                "▸"
            } else {
                " "
            };
            let style = if i == self.post_plan_selected {
                theme::interactive::selected_option()
            } else {
                theme::text::primary()
            };
            lines.push(Line::styled(
                format!("  {} {:<18} {}", marker, label, desc),
                style,
            ));
        }
        lines.push(Line::raw(""));

        Paragraph::new(Text::from(lines)).render(chunks[1], buf);

        // Help text
        self.render_help_text(chunks[2], buf);

        None // no cursor in this mode
    }

    fn render_exec_approval_dialog(&self, area: Rect, buf: &mut Buffer) -> Option<Position> {
        let Some(approval) = &self.exec_approval else {
            return None;
        };

        let chunks = Layout::vertical([
            Constraint::Length(1),                             // Status bar
            Constraint::Length(7),                             // Command summary
            Constraint::Length(approval.options.len() as u16), // Options
            Constraint::Length(1),                             // Help text
        ])
        .split(area);

        self.render_status_bar(chunks[0], buf);

        let retry_prefix = if approval.is_retry { "Retry: " } else { "" };
        let mut summary_lines = vec![
            Line::styled(format!("  {}", approval.title), theme::interactive::title()),
            Line::styled(
                format!("  {}{}", retry_prefix, approval.command),
                theme::text::primary(),
            ),
            Line::styled(
                format!("  sandbox: {}", approval.sandbox_label),
                theme::text::muted(),
            ),
        ];
        if let Some(network_access) = &approval.network_access {
            let network_label = match network_access {
                NetworkAccess::Denied => "denied",
                NetworkAccess::Allowlist { .. } => "allowlist",
                NetworkAccess::Full => "full",
            };
            summary_lines.push(Line::styled(
                format!("  network: {network_label}"),
                theme::text::muted(),
            ));
        }
        summary_lines.push(Line::styled(
            format!("  cwd: {}", approval.cwd),
            theme::text::muted().add_modifier(Modifier::DIM),
        ));
        if let Some(label) = self.input_context_label.as_deref() {
            summary_lines.push(Line::styled(
                format!("  context: {label}"),
                theme::interactive::title(),
            ));
        }
        if !approval.writable_roots.is_empty() {
            let preview = approval
                .writable_roots
                .iter()
                .take(2)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            let suffix = if approval.writable_roots.len() > 2 {
                format!(" (+{} more)", approval.writable_roots.len() - 2)
            } else {
                String::new()
            };
            summary_lines.push(Line::styled(
                format!("  write roots: {preview}{suffix}"),
                theme::text::muted(),
            ));
        }
        if let Some(reason) = approval.reason.as_deref() {
            summary_lines.push(Line::styled(
                format!("  reason: {reason}"),
                theme::status::warning(),
            ));
        }
        Paragraph::new(Text::from(summary_lines)).render(chunks[1], buf);

        let mut option_lines: Vec<Line<'static>> = Vec::new();
        for (i, (label, desc)) in approval.options.iter().enumerate() {
            let marker = if i == self.exec_approval_selected {
                "▸"
            } else {
                " "
            };
            let style = if i == self.exec_approval_selected {
                theme::interactive::selected_option()
            } else {
                theme::text::primary()
            };
            option_lines.push(Line::styled(
                format!("  {} {:<20} {}", marker, label, desc),
                style,
            ));
        }
        Paragraph::new(Text::from(option_lines)).render(chunks[2], buf);

        self.render_help_text(chunks[3], buf);
        None
    }

    /// Render the notes input area for option questions.
    fn render_notes_area(&self, area: Rect, buf: &mut Buffer) -> Option<Position> {
        if area.height == 0 {
            return None;
        }

        let border_style = if self.user_input_notes_focused {
            theme::border::focused()
        } else {
            theme::border::idle()
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(Line::styled(" Notes ", theme::interactive::title()));

        let inner = block.inner(area);

        let display = if self.user_input_notes_text.is_empty() {
            Text::styled("Tab to add notes...", theme::text::placeholder())
        } else {
            Text::raw(&self.user_input_notes_text)
        };

        Paragraph::new(display).block(block).render(area, buf);

        if self.user_input_notes_focused && inner.width > 0 && inner.height > 0 {
            let cursor_x = self
                .user_input_notes_text
                .width()
                .min(inner.width.saturating_sub(1) as usize) as u16;
            Some(Position {
                x: inner.x.saturating_add(cursor_x),
                y: inner.y,
            })
        } else {
            None
        }
    }

    /// Render the slash-command autocomplete popup floating above the bottom pane.
    fn render_command_popup(&self, bottom_area: Rect, buf: &mut Buffer) {
        let filtered = self.filtered_commands();
        if filtered.is_empty() {
            return;
        }

        let max_visible = COMMAND_POPUP_MAX_VISIBLE as u16;
        let item_count = filtered.len() as u16;
        // +2 for border top/bottom
        let popup_height = item_count.min(max_visible) + 2;

        // Position the popup just above the bottom pane area.
        let popup_y = bottom_area.y.saturating_sub(popup_height);
        let popup_area = Rect {
            x: bottom_area.x,
            y: popup_y,
            width: bottom_area.width,
            height: popup_height,
        };

        // Ensure the popup area is within the buffer bounds.
        let buf_area = *buf.area();
        let clamped = popup_area.intersection(buf_area);
        if clamped.width == 0 || clamped.height == 0 {
            return;
        }

        // Clear the region behind the popup.
        Clear.render(clamped, buf);

        // Build the list lines.
        let inner_width = clamped.width.saturating_sub(2) as usize; // borders
        let mut lines: Vec<Line<'static>> = Vec::new();
        let start = self.command_popup.scroll_offset.min(filtered.len());
        let end = (start + COMMAND_POPUP_MAX_VISIBLE).min(filtered.len());
        for (i, (name, desc)) in filtered
            .iter()
            .enumerate()
            .skip(start)
            .take(end.saturating_sub(start))
        {
            let is_selected = i == self.command_popup.selected;
            let prefix = format!("  /{:<16}", name);
            let remaining = inner_width.saturating_sub(prefix.len());
            let truncated_desc: String = if desc.len() > remaining {
                let max_chars = remaining.saturating_sub(2);
                let truncated: String = desc.chars().take(max_chars).collect();
                format!("{truncated}..")
            } else {
                desc.clone()
            };
            let text = format!("{}{}", prefix, truncated_desc);
            let style = if is_selected {
                theme::interactive::selected_option()
            } else {
                theme::text::primary()
            };
            lines.push(Line::styled(text, style));
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border::idle())
            .title(Line::styled(" Commands ", theme::interactive::title()));

        let paragraph = Paragraph::new(Text::from(lines)).block(block);
        paragraph.render(clamped, buf);
    }

    fn render_usage_line(&self, area: Rect, buf: &mut Buffer) {
        let Some(usage_line) = self.usage_line.as_deref() else {
            return;
        };
        Paragraph::new(Line::styled(
            format!("usage {usage_line}"),
            theme::text::muted(),
        ))
        .render(area, buf);
    }

    /// Render the active Goal indicator (`goal <short_id> · Active …`)
    /// directly under the input box so the user always sees the
    /// Goal's status without invoking `/goal status`. No-op when
    /// the field is `None`; the renderer skips the row entirely via
    /// the `Constraint::Length(1)` gate in [`Self::render`].
    fn render_goal_status_line(&self, area: Rect, buf: &mut Buffer) {
        let Some(line) = self.goal_status_line.as_deref() else {
            return;
        };
        Paragraph::new(Line::styled(line.to_string(), theme::text::muted())).render(area, buf);
    }

    fn render_usage_detail_panel(&self, bottom_area: Rect, buf: &mut Buffer) {
        let Some(details) = self.usage_detail_panel.as_deref() else {
            return;
        };
        let buf_area = *buf.area();
        let width = bottom_area.width.min(buf_area.width).max(1);
        let line_count = details.lines().count() as u16;
        let height = line_count.saturating_add(2).clamp(4, 12);
        let popup_area = Rect {
            x: bottom_area.x,
            y: bottom_area.y.saturating_sub(height),
            width,
            height,
        }
        .intersection(buf_area);
        if popup_area.width == 0 || popup_area.height == 0 {
            return;
        }

        Clear.render(popup_area, buf);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border::idle())
            .title(Line::styled(" Usage ", theme::interactive::title()));
        Paragraph::new(details.to_string())
            .block(block)
            .wrap(Wrap { trim: true })
            .render(popup_area, buf);
    }

    fn render_status_bar(&self, area: Rect, buf: &mut Buffer) {
        let phase = animation_phase(120);
        let status_line = match self.status {
            AgentStatus::Idle => Line::styled("● Ready", theme::status::ready()),
            AgentStatus::Thinking => {
                gradient_line("● Thinking...", &thinking_colors(), phase, true)
            }
            AgentStatus::Retrying => gradient_line(
                self.retry_notice
                    .as_deref()
                    .unwrap_or("● Retrying model request..."),
                &executing_tool_colors(),
                phase,
                true,
            ),
            AgentStatus::ExecutingTool => {
                gradient_line("● Executing tool...", &executing_tool_colors(), phase, true)
            }
            AgentStatus::AwaitingUserInput => {
                Line::styled("● Awaiting input...", theme::status::pending_input())
            }
            AgentStatus::AwaitingApproval => Line::styled(
                "● Awaiting sandbox approval...",
                theme::status::pending_approval(),
            ),
            AgentStatus::AwaitingPostPlanChoice => Line::styled(
                "● Plan complete — choose next step",
                theme::status::pending_choice(),
            ),
            AgentStatus::AwaitingNetworkPolicyChoice => Line::styled(
                "● Plan confirmed — choose network policy",
                theme::status::pending_choice(),
            ),
            AgentStatus::AwaitingIntentReviewChoice => Line::styled(
                "● IntentSpec ready — confirm before planning",
                theme::status::pending_choice(),
            ),
        };
        Paragraph::new(status_line).render(area, buf);
    }

    fn render_input_area(&self, area: Rect, buf: &mut Buffer) -> Option<Position> {
        if area.width == 0 || area.height == 0 {
            return None;
        }

        let border_style = if self.focused {
            theme::border::focused()
        } else {
            theme::border::idle()
        };

        let mut block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(border_style);
        if let Some(label) = self.input_context_label.as_deref() {
            block = block.title(Line::styled(
                format!(" {label} "),
                theme::interactive::title(),
            ));
        }

        let inner = block.inner(area);

        let content_width = inner.width as usize;
        let content_height = inner.height as usize;
        let (display_text, cursor_x, cursor_y) = if self.input.is_empty() {
            let placeholder = if self.status == AgentStatus::AwaitingUserInput {
                "Type custom answer..."
            } else {
                self.input_hint.as_deref().unwrap_or("Type your message...")
            };
            (
                Text::from(vec![Line::styled(placeholder, theme::text::placeholder())]),
                0u16,
                0u16,
            )
        } else {
            let (lines, cursor_x, cursor_y) =
                self.visible_input_and_cursor(content_width, content_height);
            let text_lines = lines.into_iter().map(Line::raw).collect::<Vec<_>>();
            (Text::from(text_lines), cursor_x, cursor_y)
        };

        Paragraph::new(display_text).block(block).render(area, buf);
        self.render_workspace_badge(area, buf, border_style);

        if !self.focused || inner.width == 0 || inner.height == 0 {
            return None;
        }

        Some(Position {
            x: inner.x.saturating_add(cursor_x),
            y: inner.y.saturating_add(cursor_y),
        })
    }

    fn render_workspace_badge(&self, area: Rect, buf: &mut Buffer, border_style: Style) {
        if area.width < 12 || area.height == 0 {
            return;
        }

        let Some(cwd) = &self.cwd else {
            return;
        };

        let badge = format_workspace_badge(
            cwd,
            self.git_branch.as_deref(),
            area.width.saturating_sub(6) as usize,
        );
        if badge.is_empty() {
            return;
        }

        let badge_width = badge.width() as u16;
        if badge_width + 3 >= area.width {
            return;
        }

        let y = area.y.saturating_add(area.height.saturating_sub(1));
        let x = area
            .x
            .saturating_add(area.width.saturating_sub(badge_width + 3));
        let Some(max_width) = writable_line_width(buf, x, y, area) else {
            return;
        };

        let spans = vec![
            Span::styled("┤", border_style),
            Span::styled(badge, theme::badge::workspace()),
        ];
        buf.set_line(x, y, &Line::from(spans), max_width);
    }

    fn render_help_text(&self, area: Rect, buf: &mut Buffer) {
        let help = match self.status {
            AgentStatus::Idle => {
                if self.command_popup.visible {
                    "[Tab: Complete] [Up/Down: Select] [Esc: Dismiss] [Enter: Send]"
                } else {
                    "[Enter: Send] [PgUp/PgDn: Scroll] [Drag: Select] [Ctrl+C: Exit]"
                }
            }
            AgentStatus::Thinking | AgentStatus::Retrying | AgentStatus::ExecutingTool => {
                if self.input_hint.is_some() {
                    "[Tab/Shift+Tab: Switch pane] [Ctrl+O: Overview] [Ctrl+F: Focus] [Enter: Run /mux] [Esc: Clear/Interrupt]"
                } else {
                    "[Esc: Interrupt] [PgUp/PgDn: Scroll] [Drag: Select] [Ctrl+C: Exit]"
                }
            }
            AgentStatus::AwaitingUserInput => {
                "[Up/Down: Select] [1-9: Quick select] [Enter: Submit] [Esc: Cancel]"
            }
            AgentStatus::AwaitingApproval => "[Up/Down: Select] [Enter: Confirm] [Esc: Deny]",
            AgentStatus::AwaitingPostPlanChoice => {
                "[Up/Down: Select] [j/l: Scroll] [Enter: Confirm] [Esc: Cancel]"
            }
            AgentStatus::AwaitingNetworkPolicyChoice => {
                "[Up/Down: Select] [j/l: Scroll] [Enter: Confirm] [Esc: Back]"
            }
            AgentStatus::AwaitingIntentReviewChoice => {
                "[Up/Down: Select] [j/l: Scroll] [Enter: Confirm] [Esc: Cancel]"
            }
        };

        let help_line = Line::styled(help, theme::text::help());
        Paragraph::new(help_line).render(area, buf);
    }

    fn prev_char_pos(&self) -> usize {
        let s = &self.input[..self.cursor_pos];
        s.char_indices().next_back().map(|(i, _)| i).unwrap_or(0)
    }

    fn next_char_pos(&self) -> usize {
        let s = &self.input[self.cursor_pos..];
        s.char_indices()
            .nth(1)
            .map(|(i, _)| self.cursor_pos + i)
            .unwrap_or(self.input.len())
    }

    fn visible_input_and_cursor(
        &self,
        content_width: usize,
        content_height: usize,
    ) -> (Vec<String>, u16, u16) {
        if content_width == 0 || content_height == 0 {
            return (Vec::new(), 0, 0);
        }

        let mut wrapped_lines: Vec<String> = Vec::new();
        let mut current_line = String::new();
        let mut current_col = 0usize;
        let mut cursor_row = 0usize;
        let mut cursor_col = 0usize;
        let mut line_index = 0usize;

        for (idx, ch) in self.input.char_indices() {
            if ch == '\n' {
                if idx == self.cursor_pos {
                    cursor_row = line_index;
                    cursor_col = current_col;
                }
                wrapped_lines.push(std::mem::take(&mut current_line));
                line_index += 1;
                current_col = 0;
                continue;
            }

            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(1).max(1);
            if current_col + ch_width > content_width && current_col > 0 {
                wrapped_lines.push(std::mem::take(&mut current_line));
                line_index += 1;
                current_col = 0;
            }

            if idx == self.cursor_pos {
                cursor_row = line_index;
                cursor_col = current_col;
            }

            current_line.push(ch);
            current_col += ch_width;
        }

        if self.cursor_pos == self.input.len() {
            cursor_row = line_index;
            cursor_col = current_col;
        }

        wrapped_lines.push(current_line);

        let start_row = cursor_row.saturating_sub(content_height.saturating_sub(1));
        let end_row = (start_row + content_height).min(wrapped_lines.len());
        let mut visible_lines = wrapped_lines[start_row..end_row].to_vec();
        while visible_lines.len() < content_height {
            visible_lines.push(String::new());
        }

        let cursor_y = cursor_row.saturating_sub(start_row).min(u16::MAX as usize) as u16;
        let max_cursor_x = content_width.saturating_sub(1);
        let cursor_x = cursor_col.min(max_cursor_x).min(u16::MAX as usize) as u16;
        (visible_lines, cursor_x, cursor_y)
    }
}

fn approval_dialog_reason(request: &ExecApprovalRequest) -> Option<String> {
    match (&request.reason, &request.cache_disabled_reason) {
        (Some(reason), Some(cache_disabled_reason)) => {
            Some(format!("{reason}\n{cache_disabled_reason}"))
        }
        (Some(reason), None) => Some(reason.clone()),
        (None, Some(cache_disabled_reason)) => Some(cache_disabled_reason.clone()),
        (None, None) => None,
    }
}

fn animation_phase(step_ms: u128) -> usize {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    (millis / step_ms.max(1)) as usize
}

fn thinking_colors() -> [Color; 5] {
    theme::animation::active_gradient()
}

fn executing_tool_colors() -> [Color; 5] {
    theme::animation::executing_gradient()
}

fn gradient_line(text: &str, colors: &[Color], phase: usize, bold: bool) -> Line<'static> {
    let spans = text
        .chars()
        .enumerate()
        .map(|(idx, ch)| {
            let color = colors[(idx + phase) % colors.len()];
            let mut style = Style::default().fg(color);
            if bold {
                style = style.bold();
            }
            Span::styled(ch.to_string(), style)
        })
        .collect::<Vec<_>>();
    Line::from(spans)
}

fn format_workspace_badge(path: &Path, git_branch: Option<&str>, max_width: usize) -> String {
    if max_width <= 2 {
        return String::new();
    }

    let display = if let Some(home) = dirs::home_dir() {
        if let Ok(stripped) = path.strip_prefix(&home) {
            if stripped.as_os_str().is_empty() {
                "~".to_string()
            } else {
                format!("~/{}", stripped.display())
            }
        } else {
            path.display().to_string()
        }
    } else {
        path.display().to_string()
    };

    let display = match git_branch.filter(|branch| !branch.trim().is_empty()) {
        Some(branch) => format!("{display} ({branch})"),
        None => display,
    };

    format!(
        " {} ",
        truncate_from_left(&display, max_width.saturating_sub(2))
    )
}

fn writable_line_width(buf: &Buffer, x: u16, y: u16, area: Rect) -> Option<u16> {
    let buf_area = *buf.area();
    if !buf_area.contains(Position { x, y }) {
        return None;
    }

    let area_right = area.right().min(buf_area.right());
    let max_width = area_right.saturating_sub(x);
    (max_width > 0).then_some(max_width)
}

fn truncate_from_left(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }

    if text.width() <= max_width {
        return text.to_string();
    }

    if max_width == 1 {
        return "…".to_string();
    }

    let mut width = 1usize;
    let mut kept = Vec::new();
    for ch in text.chars().rev() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(1).max(1);
        if width + ch_width > max_width {
            break;
        }
        kept.push(ch);
        width += ch_width;
    }
    kept.reverse();
    format!("…{}", kept.into_iter().collect::<String>())
}

impl Default for BottomPane {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::{buffer::Buffer, layout::Rect};

    use super::BottomPane;
    use crate::internal::{
        ai::sandbox::NetworkAccess,
        tui::{app_event::AgentStatus, theme},
    };

    fn row_text(buf: &Buffer, y: u16, width: u16) -> String {
        let mut out = String::new();
        for x in 0..width {
            out.push_str(buf[(x, y)].symbol());
        }
        out
    }

    fn row_symbol_x(buf: &Buffer, y: u16, width: u16, symbol: &str) -> Option<u16> {
        (0..width).find(|&x| buf[(x, y)].symbol() == symbol)
    }

    #[test]
    fn normal_mode_height_is_six_lines() {
        let pane = BottomPane::new();
        assert_eq!(pane.desired_height(), 6);
    }

    #[test]
    fn usage_line_extends_normal_mode_without_affecting_approval_dialog() {
        let mut pane = BottomPane::new();
        pane.set_usage_line(Some("openai/gpt-test · 10 tok · 1.2s".to_string()));
        assert_eq!(pane.desired_height(), 7);

        let area = Rect::new(0, 0, 80, 7);
        let mut buf = Buffer::empty(area);
        let _ = pane.render(area, &mut buf);
        let rendered = (0..area.height)
            .map(|y| row_text(&buf, y, area.width))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("usage openai/gpt-test"));

        pane.status = AgentStatus::AwaitingApproval;
        assert_ne!(pane.desired_height(), 7);
    }

    /// `set_goal_status_line(Some(_))` grows the bottom-pane height
    /// by one row and the active Goal indicator appears directly
    /// beneath the input box. Clearing the line shrinks the pane
    /// back to its base height. Pins the layout contract the App
    /// relies on when it toggles `refresh_bottom_pane_goal_status`
    /// between `Some(line)` and `None`.
    #[test]
    fn goal_status_line_extends_height_and_renders_above_status_bar() {
        let mut pane = BottomPane::new();
        let base_height = pane.desired_height();
        pane.set_goal_status_line(Some("Goal a1a1a1a1 · Active · 0/1 criteria".to_string()));
        assert_eq!(pane.desired_height(), base_height + 1);
        assert_eq!(
            pane.goal_status_line(),
            Some("Goal a1a1a1a1 · Active · 0/1 criteria"),
        );

        let area = Rect::new(0, 0, 80, base_height + 1);
        let mut buf = Buffer::empty(area);
        let _ = pane.render(area, &mut buf);
        let rendered = (0..area.height)
            .map(|y| row_text(&buf, y, area.width))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("Goal a1a1a1a1 · Active · 0/1 criteria"));

        pane.set_goal_status_line(None);
        assert_eq!(pane.desired_height(), base_height);
        assert!(pane.goal_status_line().is_none());
    }

    /// When both a Goal indicator and a usage line are present the
    /// bottom pane needs two extra rows (one each) on top of the
    /// base height. Goal sits directly under the input box; usage
    /// sits between Goal and the status bar.
    #[test]
    fn goal_status_and_usage_line_coexist_with_correct_height() {
        let mut pane = BottomPane::new();
        let base = pane.desired_height();
        pane.set_goal_status_line(Some("Goal abcd1234 · Active · 1/2 criteria".to_string()));
        pane.set_usage_line(Some("openai/gpt · 10 tok".to_string()));
        assert_eq!(pane.desired_height(), base + 2);

        let area = Rect::new(0, 0, 80, base + 2);
        let mut buf = Buffer::empty(area);
        let _ = pane.render(area, &mut buf);
        let rendered = (0..area.height)
            .map(|y| row_text(&buf, y, area.width))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("Goal abcd1234 · Active · 1/2 criteria"));
        assert!(rendered.contains("usage openai/gpt"));
    }

    #[test]
    fn usage_detail_panel_renders_above_bottom_pane() {
        let mut pane = BottomPane::new();
        pane.set_usage_detail_panel(Some(
            "Usage Details\nCurrent: openai/gpt-test\nRows:\nopenai/gpt-test | req 1".to_string(),
        ));

        let area = Rect::new(0, 13, 80, 7);
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 20));
        let _ = pane.render(area, &mut buf);
        let rendered = (0..20)
            .map(|y| row_text(&buf, y, 80))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("Usage Details"));
        assert!(rendered.contains("openai/gpt-test | req 1"));
        assert!(pane.is_usage_detail_panel_visible());
        pane.dismiss_usage_detail_panel();
        assert!(!pane.is_usage_detail_panel_visible());
    }

    #[test]
    fn approval_mode_height_tracks_option_count() {
        let mut pane = BottomPane::new();
        pane.status = AgentStatus::AwaitingApproval;
        pane.set_approval_dialog(
            "Sandbox approval required".to_string(),
            "cargo test".to_string(),
            PathBuf::from("/tmp"),
            None,
            false,
            "workspace-write".to_string(),
            Some(NetworkAccess::Denied),
            Vec::new(),
            vec![
                ("Approve".to_string(), "Run once".to_string()),
                (
                    "Approve Session".to_string(),
                    "Allow matching commands".to_string(),
                ),
                (
                    "Allow All Commands".to_string(),
                    "Allow every command".to_string(),
                ),
                ("Deny".to_string(), "Reject".to_string()),
                ("Abort Turn".to_string(), "Interrupt".to_string()),
            ],
        );
        assert_eq!(pane.desired_height(), 14);
    }

    #[test]
    fn approval_dialog_renders_command_context_and_options() {
        let mut pane = BottomPane::new();
        pane.status = AgentStatus::AwaitingApproval;
        pane.set_approval_dialog(
            "Sandbox approval required".to_string(),
            "cargo test --all".to_string(),
            PathBuf::from("/tmp/libra"),
            Some("Writes outside the workspace need explicit approval".to_string()),
            true,
            "workspace-write".to_string(),
            Some(NetworkAccess::Allowlist {
                services: Vec::new(),
            }),
            vec![PathBuf::from("/tmp/libra"), PathBuf::from("/tmp/shared")],
            vec![
                ("Approve".to_string(), "Run once".to_string()),
                (
                    "Approve Session".to_string(),
                    "Allow matching commands".to_string(),
                ),
                (
                    "Allow All Commands".to_string(),
                    "Allow every command".to_string(),
                ),
                ("Deny".to_string(), "Reject".to_string()),
                ("Abort Turn".to_string(), "Interrupt".to_string()),
            ],
        );
        pane.exec_approval_selected = 2;

        let area = Rect::new(0, 0, 96, pane.desired_height());
        let mut buf = Buffer::empty(area);
        let _ = pane.render(area, &mut buf);
        let rendered = (0..area.height)
            .map(|y| row_text(&buf, y, area.width))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("Awaiting sandbox approval"));
        assert!(rendered.contains("Sandbox approval required"));
        assert!(rendered.contains("Retry: cargo test --all"));
        assert!(rendered.contains("sandbox: workspace-write"));
        assert!(rendered.contains("network: allowlist"));
        assert!(rendered.contains("cwd: /tmp/libra"));
        assert!(rendered.contains("write roots: /tmp/libra, /tmp/shared"));
        assert!(rendered.contains("Writes outside the workspace need explicit approval"));
        assert!(rendered.contains("▸ Allow All Commands"));
        assert!(rendered.contains("Deny"));
    }

    #[test]
    fn retry_status_line_renders_error_notice() {
        let mut pane = BottomPane::new();
        pane.set_retry_notice("● Retrying request 2/5 in 1.5s (503 upstream)".to_string());

        let area = Rect::new(0, 0, 80, pane.desired_height());
        let mut buf = Buffer::empty(area);
        let _ = pane.render(area, &mut buf);
        let rendered = (0..area.height)
            .map(|y| row_text(&buf, y, area.width))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("Retrying request 2/5"));
        assert!(rendered.contains("503 upstream"));
    }

    #[test]
    fn plan_and_intent_review_dialogs_use_phase_specific_labels() {
        let plan_area = Rect::new(0, 0, 80, 6);

        let mut plan_pane = BottomPane::new();
        plan_pane.status = AgentStatus::AwaitingPostPlanChoice;
        let mut plan_buf = Buffer::empty(plan_area);
        let _ = plan_pane.render(plan_area, &mut plan_buf);
        let plan_text = (0..plan_area.height)
            .map(|y| row_text(&plan_buf, y, plan_area.width))
            .collect::<Vec<_>>()
            .join("\n");
        for label in crate::internal::tui::workflow_baseline::POST_PLAN_CHOICES {
            assert!(
                plan_text.contains(label),
                "post-plan dialog missing baseline choice {label:?}: {plan_text}"
            );
        }
        assert!(!plan_text.contains("Network: Deny"));
        assert!(!plan_text.contains("Network: Allow"));
        assert!(!plan_text.contains("Execute Spec"));
        assert!(!plan_text.contains("Modify Spec"));

        let mut network_pane = BottomPane::new();
        network_pane.status = AgentStatus::AwaitingNetworkPolicyChoice;
        let mut network_buf = Buffer::empty(plan_area);
        let _ = network_pane.render(plan_area, &mut network_buf);
        let network_text = (0..plan_area.height)
            .map(|y| row_text(&network_buf, y, plan_area.width))
            .collect::<Vec<_>>()
            .join("\n");
        for label in crate::internal::tui::workflow_baseline::NETWORK_POLICY_CHOICES {
            assert!(
                network_text.contains(label),
                "network-policy dialog missing baseline choice {label:?}: {network_text}"
            );
        }
        assert!(!network_text.contains("Execute Plan"));
        assert!(!network_text.contains("Modify Plan"));

        let area = Rect::new(0, 0, 80, 6);
        let mut intent_pane = BottomPane::new();
        intent_pane.status = AgentStatus::AwaitingIntentReviewChoice;
        let mut intent_buf = Buffer::empty(area);
        let _ = intent_pane.render(area, &mut intent_buf);
        let intent_text = (0..area.height)
            .map(|y| row_text(&intent_buf, y, area.width))
            .collect::<Vec<_>>()
            .join("\n");
        for label in crate::internal::tui::workflow_baseline::INTENT_REVIEW_CHOICES {
            assert!(
                intent_text.contains(label),
                "intent-review dialog missing baseline choice {label:?}: {intent_text}"
            );
        }
        assert!(!intent_text.contains("Execute Plan"));
        assert!(!intent_text.contains("Modify Plan"));
    }

    #[test]
    fn statusline_renders_below_input_box() {
        let mut pane = BottomPane::new();
        pane.status = AgentStatus::Idle;

        let area = Rect::new(0, 0, 40, 6);
        let mut buf = Buffer::empty(area);
        let _ = pane.render(area, &mut buf);

        let top = row_text(&buf, 0, area.width);
        let input = row_text(&buf, 1, area.width);
        let bottom_of_box = row_text(&buf, 4, area.width);
        let status = row_text(&buf, 5, area.width);

        assert!(input.contains("Type your message"));
        assert!(top.contains("╭"));
        assert!(bottom_of_box.contains("╰"));
        assert!(status.contains("Ready"));
        assert!(!status.contains("Enter: Send"));
    }

    #[test]
    fn input_box_renders_cwd_badge_on_bottom_border() {
        let mut pane = BottomPane::new();
        pane.set_cwd(PathBuf::from("/Users/neon/Documents/Projects/libra"));
        pane.set_git_branch(Some("main".to_string()));

        let area = Rect::new(0, 0, 48, 6);
        let mut buf = Buffer::empty(area);
        let _ = pane.render(area, &mut buf);

        let bottom_of_box = row_text(&buf, 4, area.width);
        assert!(bottom_of_box.contains("libra"));
        assert!(bottom_of_box.contains("(main)"));
        assert!(bottom_of_box.contains("┤"));
        assert!(bottom_of_box.ends_with("─╯"));
    }

    #[test]
    fn render_clamps_workspace_badge_to_buffer_area() {
        let mut pane = BottomPane::new();
        pane.set_cwd(PathBuf::from("/Volumes/Data/linked"));

        let buffer_area = Rect::new(0, 0, 122, 35);
        let oversized_bottom_area = Rect::new(0, 31, 122, 6);
        let mut buf = Buffer::empty(buffer_area);

        let _ = pane.render(oversized_bottom_area, &mut buf);
    }

    #[test]
    fn focused_input_uses_shared_theme_colors() {
        let mut pane = BottomPane::new();
        pane.set_cwd(PathBuf::from("/Users/neon/Documents/Projects/libra"));
        pane.set_git_branch(Some("main".to_string()));

        let area = Rect::new(0, 0, 48, 6);
        let mut buf = Buffer::empty(area);
        let _ = pane.render(area, &mut buf);

        assert_eq!(theme::border::focused().fg, Some(buf[(0, 0)].fg));

        let badge_x = row_symbol_x(&buf, 4, area.width, "┤").expect("badge separator missing");
        assert_eq!(theme::border::focused().fg, Some(buf[(badge_x, 4)].fg));
    }

    #[test]
    fn pasted_text_inserts_at_cursor() {
        let mut pane = BottomPane::new();
        pane.insert_text("hello world");
        pane.cursor_left();
        pane.cursor_left();
        pane.insert_text("\nwide 文本");

        assert_eq!(pane.input, "hello wor\nwide 文本ld");
        assert_eq!(pane.cursor_pos, "hello wor\nwide 文本".len());
    }

    #[test]
    fn command_popup_scrolls_with_selection() {
        let mut pane = BottomPane::new();
        pane.set_command_hints(
            (0..12)
                .map(|i| (format!("cmd{i}"), format!("command {i}")))
                .collect(),
        );
        pane.input = "/".to_string();
        pane.cursor_pos = 1;
        pane.sync_command_popup();

        for _ in 0..9 {
            pane.command_popup_down();
        }

        assert_eq!(pane.command_popup.selected, 9);
        assert_eq!(pane.command_popup.scroll_offset, 2);
    }

    #[test]
    fn command_popup_scroll_offset_clamps_after_filter_change() {
        let mut pane = BottomPane::new();
        pane.set_command_hints(
            (0..12)
                .map(|i| (format!("cmd{i}"), format!("command {i}")))
                .collect(),
        );
        pane.input = "/".to_string();
        pane.cursor_pos = 1;
        pane.sync_command_popup();

        for _ in 0..10 {
            pane.command_popup_down();
        }
        assert!(pane.command_popup.scroll_offset > 0);

        pane.input = "/cmd1".to_string();
        pane.cursor_pos = pane.input.len();
        pane.sync_command_popup();

        assert_eq!(pane.filtered_commands().len(), 3);
        assert_eq!(pane.command_popup.scroll_offset, 0);
        assert_eq!(pane.command_popup.selected, 2);
    }
}
