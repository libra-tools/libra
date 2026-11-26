//! Shared patch-mode hunk engine (ADR-HF-15).
//!
//! Parses a Git-compatible unified diff into [`FileDiff`] / [`Hunk`], counts
//! and performs `s` splits, reassembles a selected subset, and applies the
//! result to an index blob. The module never writes a worktree; callers
//! (`add -p`, `reset -p`) persist the blob and index entry.

mod apply;
mod edit;
mod model;
pub mod session;

pub use apply::{AppliedIndexBlob, PatchApplyError, PatchApplyMode, apply_selected_hunks_to_blob};
pub use edit::{
    CANNOT_EDIT, EDIT_RETRY_PROMPT, EditedHunk, MANUAL_HUNK_HEADER, edited_hunk_applies,
    format_edit_buffer, is_editable, parse_edited_buffer, run_editor,
};
pub use model::{
    FileDiff, Hunk, HunkLine, HunkUse, ParseError, parse_unified_diff, reassemble_file_patch,
    split_hunk,
};
pub use session::{PatchSessionKind, SessionAction, SessionOptions, run_session, run_session_with};
