//! Session persistence for saving and restoring conversation state.
//!
//! Sessions capture conversation history, working directory, context mode,
//! and metadata. They are stored as append-only JSONL event streams in
//! `.libra/sessions/{session_id}/events.jsonl`.
//!
//! ## Usage
//!
//! ```no_run
//! use libra::internal::ai::session::{SessionState, SessionStore};
//!
//! // Create and populate a session
//! let mut session = SessionState::new("/path/to/project");
//! session.add_user_message("implement auth");
//! session.add_assistant_message("I'll implement authentication...");
//!
//! // Save to disk
//! let store = SessionStore::new(std::path::Path::new("/path/to/project"));
//! store.save(&session).unwrap();
//!
//! // Restore later
//! let restored = store.load_latest().unwrap();
//! ```

pub mod file_history;
pub mod jsonl;
pub mod migration;
pub mod state;
pub mod store;

pub use jsonl::{
    CodeCommandAdmission, CodeCommandIdentity, CodeCommandIntent, CodeCommandRecovery,
    CodeCommandStatus, CodeCommandStoreError, CodeWorkflowCursor, CodeWorkflowEvent,
    CodeWorkflowEventKind, CodeWorkflowReplay, CodeWorkflowSequenceGap,
    INTENT_REVISION_CONSUMER_COMMAND_KIND, INTENT_REVISION_CONSUMPTION_SCHEMA_VERSION,
    IntentRevisionConsumption, IntentRevisionConsumptionClaim, IntentRevisionRecovery,
    MAX_INTENT_REVISION_NOTE_BYTES, Phase1RetryIntentReview, SessionEvent, SessionJsonlStore,
    code_workflow_replay_parse_visits, reset_code_workflow_replay_parse_visits,
};
pub use state::{SessionId, SessionMessage, SessionState};
pub use store::{SessionInfo, SessionStore};
