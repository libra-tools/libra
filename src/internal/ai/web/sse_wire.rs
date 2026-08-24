//! Code UI SSE wire version negotiation and v2 delta/cursor envelopes (W3-06).
//!
//! Transport backlog / resync / slow-consumer backpressure live here (W3-08).
//! v1 remains the full-snapshot [`super::code_ui::CodeUiEventEnvelope`] stream.
//! v2 emits minimal payloads keyed by the durable W1-06
//! [`CodeWorkflowEvent`] sequence — never a second live sequencer.

use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use axum::http::{HeaderMap, header};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::internal::ai::session::{CodeWorkflowEvent, CodeWorkflowEventKind, SessionJsonlStore};

/// Transport backlog event-count cap for SSE wire v2 (GC-CODE-12 / W3-08).
///
/// Projection hot-window naming/quotas stay in [`super::code_ui_projection`]
/// (W3-14); this constant is the transport-only fact source.
pub const MAX_CODE_UI_TRANSPORT_BACKLOG_EVENTS: usize = 1024;

/// Transport backlog byte cap for SSE wire v2 catch-up/bootstrap windows
/// (GC-CODE-12 / W3-08). Whichever of count or bytes is reached first wins.
pub const MAX_CODE_UI_TRANSPORT_BACKLOG_BYTES: u64 = 8 * 1024 * 1024;

/// Live broadcast ring capacity — the event-count half of the transport budget.
/// Slow consumers that lag past this capacity share the same resync/disconnect
/// policy as over-budget durable catch-up.
pub const CODE_UI_TRANSPORT_BROADCAST_CAPACITY: usize = MAX_CODE_UI_TRANSPORT_BACKLOG_EVENTS;

/// Wire code for recoverable transport-capacity exits (bootstrap or lag).
pub const WIRE_V2_RESYNC_REQUIRED: &str = "WIRE_V2_RESYNC_REQUIRED";

/// Default SSE wire when the client omits a version (until W3-09 flips default).
pub const DEFAULT_CODE_UI_SSE_WIRE_VERSION: CodeUiSseWireVersion = CodeUiSseWireVersion::V1;

/// Negotiated Code UI SSE wire version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeUiSseWireVersion {
    V1,
    V2,
}

impl CodeUiSseWireVersion {
    pub fn as_u8(self) -> u8 {
        match self {
            Self::V1 => 1,
            Self::V2 => 2,
        }
    }
}

/// Query parameters for `GET /api/code/events`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CodeEventsQuery {
    /// Wire version: `1`/`v1` or `2`/`v2`. Omitted → [`DEFAULT_CODE_UI_SSE_WIRE_VERSION`].
    pub wire: Option<String>,
    /// v2 only: last-seen durable workflow sequence; replay emits events with
    /// `sequence > cursor`.
    pub cursor: Option<String>,
}

/// Parse the negotiated wire version from query + optional Accept header.
///
/// Precedence: explicit `?wire=` wins over `Accept: text/event-stream;libra-wire=N`.
/// Illegal values fail closed.
pub fn parse_code_events_wire_version(
    query: &CodeEventsQuery,
    headers: &HeaderMap,
) -> Result<CodeUiSseWireVersion, String> {
    if let Some(raw) = query.wire.as_deref() {
        return parse_wire_token(raw);
    }
    if let Some(from_accept) = libra_wire_from_accept(headers) {
        return from_accept;
    }
    Ok(DEFAULT_CODE_UI_SSE_WIRE_VERSION)
}

fn parse_wire_token(raw: &str) -> Result<CodeUiSseWireVersion, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "v1" => Ok(CodeUiSseWireVersion::V1),
        "2" | "v2" => Ok(CodeUiSseWireVersion::V2),
        other => Err(format!(
            "query parameter `wire` must be 1/v1 or 2/v2 (got '{other}')"
        )),
    }
}

fn libra_wire_from_accept(headers: &HeaderMap) -> Option<Result<CodeUiSseWireVersion, String>> {
    // HTTP allows multiple Accept field lines; scan all of them.
    for accept_value in headers.get_all(header::ACCEPT) {
        let Ok(accept) = accept_value.to_str() else {
            continue;
        };
        for part in accept.split(',') {
            let part = part.trim();
            let media_type = part
                .split(';')
                .next()
                .unwrap_or(part)
                .trim()
                .to_ascii_lowercase();
            if media_type != "text/event-stream" {
                continue;
            }
            for param in part.split(';').skip(1) {
                let param = param.trim();
                let Some((name, value)) = param.split_once('=') else {
                    continue;
                };
                if name.trim().eq_ignore_ascii_case("libra-wire") {
                    return Some(parse_wire_token(value.trim().trim_matches('"')));
                }
            }
        }
    }
    None
}

pub fn parse_code_events_cursor(query: &CodeEventsQuery) -> Result<u64, String> {
    let Some(raw) = query.cursor.as_deref() else {
        return Ok(0);
    };
    raw.trim().parse::<u64>().map_err(|_| {
        format!("query parameter `cursor` must be a non-negative integer (got '{raw}')")
    })
}

/// Minimal v2 SSE payload (camelCase). Cursor is the durable workflow sequence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CodeUiWireV2Event {
    pub cursor: u64,
    pub event_id: Uuid,
    pub kind: String,
    pub at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub payload: serde_json::Value,
}

impl CodeUiWireV2Event {
    pub fn from_workflow_event(event: &CodeWorkflowEvent) -> Self {
        let (kind, payload) = match &event.event {
            CodeWorkflowEventKind::InteractionResolved {
                intent_revision_consumption: Some(consumption),
                ..
            } => (
                "intent_revision_consumed".to_string(),
                serde_json::json!({ "consumption": consumption }),
            ),
            CodeWorkflowEventKind::CodeUiProjectionDelta {
                projection,
                summary,
                payload,
            } => (
                format!("code_ui_projection_delta:{projection}"),
                serde_json::json!({
                    "projection": projection,
                    "summary": summary,
                    "payload": payload,
                }),
            ),
            other => (
                workflow_kind_name(other).to_string(),
                serde_json::to_value(other).unwrap_or(serde_json::Value::Null),
            ),
        };
        Self {
            cursor: event.sequence,
            event_id: event.event_id,
            kind,
            at: event.recorded_at,
            payload,
        }
    }
}

fn workflow_kind_name(kind: &CodeWorkflowEventKind) -> &'static str {
    match kind {
        CodeWorkflowEventKind::CommandAccepted { .. } => "command_accepted",
        CodeWorkflowEventKind::IntentReviewRequested { .. } => "intent_review_requested",
        CodeWorkflowEventKind::PlanReviewRequested { .. } => "plan_review_requested",
        CodeWorkflowEventKind::Phase1FormalWriteStarted { .. } => "phase1_formal_write_started",
        CodeWorkflowEventKind::NetworkPolicyRequested { .. } => "network_policy_requested",
        CodeWorkflowEventKind::PlanExecutionRepairRequested { .. } => {
            "plan_execution_repair_requested"
        }
        CodeWorkflowEventKind::InteractionResolved {
            intent_revision_consumption: Some(_),
            ..
        } => "intent_revision_consumed",
        CodeWorkflowEventKind::InteractionResolved { .. } => "interaction_resolved",
        CodeWorkflowEventKind::CodeUiProjectionDelta { .. } => "code_ui_projection_delta",
        CodeWorkflowEventKind::TerminalSuccess { .. } => "terminal_success",
        CodeWorkflowEventKind::TerminalFailure { .. } => "terminal_failure",
        CodeWorkflowEventKind::IndeterminateSideEffect { .. } => "indeterminate_side_effect",
        CodeWorkflowEventKind::CommandIntentPersisted { .. } => "command_intent_persisted",
        CodeWorkflowEventKind::CommandTerminalSuccess { .. } => "command_terminal_success",
        CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved { .. } => {
            "command_terminal_success_with_interaction_resolved"
        }
        CodeWorkflowEventKind::CommandTerminalFailure { .. } => "command_terminal_failure",
        CodeWorkflowEventKind::CommandIndeterminateSideEffect { .. } => {
            "command_indeterminate_side_effect"
        }
    }
}

/// Recoverable transport-capacity exit for SSE wire v2 (W3-08).
///
/// Emitted as `event: resync` then the stream ends. Clients must fetch a
/// snapshot and reconnect with `cursor` at [`Self::durable_tail`] (or the
/// snapshot tip) — not invent a new sequencer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodeUiWireV2ResyncEvent {
    pub code: String,
    pub reason: String,
    pub last_cursor: u64,
    pub durable_tail: u64,
    pub action: String,
}

impl CodeUiWireV2ResyncEvent {
    pub fn transport_backlog(reason: &str, last_cursor: u64, durable_tail: u64) -> Self {
        Self {
            code: WIRE_V2_RESYNC_REQUIRED.to_string(),
            reason: reason.to_string(),
            last_cursor,
            durable_tail,
            action: "fetch_snapshot".to_string(),
        }
    }
}

/// True when a durable replay/catch-up failure is a transport capacity exit
/// (resync + disconnect), not an opaque I/O failure.
///
/// Includes count/byte bound errors, unprovable truncated tails, mid-record
/// truncation, and sequence gaps that appear when the 8 MiB window starts
/// inside an older oversized row (Codex W3-08 P1).
pub fn transport_backlog_exceeded(error: &io::Error) -> bool {
    let message = error.to_string();
    message.contains("exceeding the bounded limit")
        || message.contains("cannot prove the retained tail")
        || message.contains("contains no complete JSONL record")
        || message.contains("transport backlog window omitted workflow events")
}

/// Live fan-out notify for SSE wire v2.
///
/// Full events are only broadcast when the publisher-side transport byte/count
/// budget still has room. Oversized or over-budget publishes send a tip-only
/// notify so slow consumers cannot retain unbounded JSON payloads in the ring
/// (W3-08 / GC-CODE-12).
#[derive(Debug, Clone)]
pub enum CodeUiWorkflowLiveNotify {
    Event(Box<CodeWorkflowEvent>),
    Tip { sequence: u64 },
}

#[derive(Default)]
struct TransportPublishBudget {
    /// Serialized sizes of the last `CODE_UI_TRANSPORT_BROADCAST_CAPACITY`
    /// notifies actually sent (0 = tip-only). Mirrors tokio `broadcast`
    /// occupancy: a new send drops the oldest slot when the ring is full.
    sizes: std::collections::VecDeque<u64>,
    total_bytes: u64,
}

impl TransportPublishBudget {
    fn evict_oldest_if_full(&mut self) {
        if self.sizes.len() >= CODE_UI_TRANSPORT_BROADCAST_CAPACITY {
            let oldest = self.sizes.pop_front().unwrap_or(0);
            self.total_bytes = self.total_bytes.saturating_sub(oldest);
        }
    }

    /// Record a send and return whether the new slot may hold a full event.
    ///
    /// Eviction happens only when this send would overwrite a tokio ring
    /// slot (ring already at capacity) — never on paper before send.
    fn reserve_send(&mut self, size: u64) -> bool {
        let mut projected_bytes = self.total_bytes;
        let mut projected_len = self.sizes.len();
        if projected_len >= CODE_UI_TRANSPORT_BROADCAST_CAPACITY {
            projected_bytes =
                projected_bytes.saturating_sub(self.sizes.front().copied().unwrap_or(0));
            projected_len = projected_len.saturating_sub(1);
        }
        let enqueue_full = size > 0
            && size <= MAX_CODE_UI_TRANSPORT_BACKLOG_BYTES
            && projected_len < CODE_UI_TRANSPORT_BROADCAST_CAPACITY
            && projected_bytes.saturating_add(size) <= MAX_CODE_UI_TRANSPORT_BACKLOG_BYTES;
        self.evict_oldest_if_full();
        let recorded = if enqueue_full { size } else { 0 };
        self.sizes.push_back(recorded);
        self.total_bytes = self.total_bytes.saturating_add(recorded);
        enqueue_full
    }
}

/// Approximate serialized size of a workflow event for transport budgeting.
///
/// Counts JSON bytes with a capped writer so payloads larger than the 8 MiB
/// transport window do not allocate a full serialized copy on the append path.
pub fn approx_workflow_event_transport_bytes(event: &CodeWorkflowEvent) -> u64 {
    struct CapWriter {
        count: u64,
        cap: u64,
    }
    impl io::Write for CapWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let n = buf.len() as u64;
            if self.count.saturating_add(n) > self.cap {
                self.count = self.cap.saturating_add(1);
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "transport byte budget exceeded",
                ));
            }
            self.count += n;
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut writer = CapWriter {
        count: 0,
        cap: MAX_CODE_UI_TRANSPORT_BACKLOG_BYTES,
    };
    match serde_json::to_writer(&mut writer, event) {
        Ok(()) => writer.count,
        Err(_) => writer
            .count
            .max(MAX_CODE_UI_TRANSPORT_BACKLOG_BYTES.saturating_add(1)),
    }
}

/// Durable workflow fan-out for SSE wire v2 (same sequence space as W1-06).
#[derive(Clone)]
pub struct CodeUiWorkflowHub {
    store: SessionJsonlStore,
    tx: broadcast::Sender<CodeUiWorkflowLiveNotify>,
    /// In-process durable tail (updated on every append hook). Connect-time
    /// ahead-cursor checks must not re-read the full workflow log.
    last_published: Arc<AtomicU64>,
}

impl CodeUiWorkflowHub {
    /// Attach live fan-out to `store` so every successful Code workflow append
    /// (projection, goal, command durability) publishes on this hub.
    ///
    /// Callers must use the mutated `store` (or clones taken after attach) for
    /// all writers; a pre-attach clone will not carry the hook.
    ///
    /// Reads the durable tail once at attach time; subsequent connect checks
    /// use [`Self::durable_tail_sequence`] (O(1) atomic).
    pub fn attach(store: &mut SessionJsonlStore) -> io::Result<Self> {
        let (tx, _) = broadcast::channel(CODE_UI_TRANSPORT_BROADCAST_CAPACITY);
        let tail = durable_workflow_tail_sequence(store)?;
        let last_published = Arc::new(AtomicU64::new(tail));
        let tx_hook = tx.clone();
        let last_hook = last_published.clone();
        let publish_budget = Arc::new(std::sync::Mutex::new(TransportPublishBudget::default()));
        store.set_on_code_workflow_append(Some(Arc::new(move |event: &CodeWorkflowEvent| {
            last_hook.fetch_max(event.sequence, Ordering::Release);
            let size = approx_workflow_event_transport_bytes(event);
            let enqueue_full = publish_budget
                .lock()
                .map(|mut budget| budget.reserve_send(size))
                .unwrap_or(false);
            let notify = if enqueue_full {
                CodeUiWorkflowLiveNotify::Event(Box::new(event.clone()))
            } else {
                CodeUiWorkflowLiveNotify::Tip {
                    sequence: event.sequence,
                }
            };
            let _ = tx_hook.send(notify);
        })));
        Ok(Self {
            store: store.clone(),
            tx,
            last_published,
        })
    }

    /// Convenience for tests: attach fan-out to a fresh store clone.
    pub fn new(mut store: SessionJsonlStore) -> io::Result<Self> {
        Self::attach(&mut store)
    }

    pub fn store(&self) -> &SessionJsonlStore {
        &self.store
    }

    /// Highest durable workflow sequence known to this hub (`0` if empty).
    ///
    /// O(1): maintained by the append hook after a one-time attach read.
    pub fn durable_tail_sequence(&self) -> u64 {
        self.last_published.load(Ordering::Acquire)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CodeUiWorkflowLiveNotify> {
        self.tx.subscribe()
    }

    /// Replay durable workflow events with `sequence > after_sequence`.
    ///
    /// Bounds use the **transport** backlog (1024 events / 8 MiB), not the
    /// projection hot-window constants owned by W3-14.
    pub fn replay_after(&self, after_sequence: u64) -> io::Result<Vec<CodeWorkflowEvent>> {
        match self.store.load_code_workflow_replay_since_committed(
            after_sequence,
            MAX_CODE_UI_TRANSPORT_BACKLOG_EVENTS,
            MAX_CODE_UI_TRANSPORT_BACKLOG_BYTES,
        ) {
            Ok(replay) => {
                if let Some(gap) = replay.gaps.first() {
                    if replay.window_cut_mid_record
                        && replay.events.first().is_some_and(|first| {
                            gap.before == first.sequence && gap.after < first.sequence
                        })
                    {
                        // Bounded reader discarded an incomplete leading record.
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "Code UI wire v2 transport backlog window omitted workflow events between sequences {} and {}; fetch a snapshot and reconnect at the durable tip",
                                gap.after, gap.before
                            ),
                        ));
                    }
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "Code UI wire v2 cannot resume across missing workflow events between sequences {} and {}",
                            gap.after, gap.before
                        ),
                    ));
                }
                if let Some(last) = replay.events.last() {
                    self.last_published
                        .fetch_max(last.sequence, Ordering::Release);
                }
                Ok(replay.events)
            }
            // Idle reconnect at the process-local durable tip: the 8 MiB
            // suffix may contain only non-workflow rows, or a single
            // oversized tip record with no complete JSONL line in-window.
            // Reconnecting at that cursor must not resync-loop.
            Err(error)
                if after_sequence > 0
                    && after_sequence == self.durable_tail_sequence()
                    && idle_tip_window_error(&error) =>
            {
                Ok(Vec::new())
            }
            Err(error) => Err(error),
        }
    }
}

fn idle_tip_window_error(error: &io::Error) -> bool {
    let message = error.to_string();
    message.contains("cannot prove the retained tail")
        || message.contains("contains no complete JSONL record")
}

fn durable_workflow_tail_sequence(store: &SessionJsonlStore) -> io::Result<u64> {
    Ok(store.next_code_workflow_sequence()?.saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    #[test]
    fn parse_wire_defaults_to_v1_when_unspecified() {
        let query = CodeEventsQuery::default();
        let headers = HeaderMap::new();
        assert_eq!(
            parse_code_events_wire_version(&query, &headers).unwrap(),
            CodeUiSseWireVersion::V1
        );
    }

    #[test]
    fn parse_wire_accepts_explicit_v1_and_v2() {
        let headers = HeaderMap::new();
        for (raw, expected) in [
            ("1", CodeUiSseWireVersion::V1),
            ("v1", CodeUiSseWireVersion::V1),
            ("2", CodeUiSseWireVersion::V2),
            ("V2", CodeUiSseWireVersion::V2),
        ] {
            let query = CodeEventsQuery {
                wire: Some(raw.to_string()),
                cursor: None,
            };
            assert_eq!(
                parse_code_events_wire_version(&query, &headers).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn parse_wire_rejects_illegal_values() {
        let query = CodeEventsQuery {
            wire: Some("3".into()),
            cursor: None,
        };
        assert!(parse_code_events_wire_version(&query, &HeaderMap::new()).is_err());
    }

    #[test]
    fn query_wire_wins_over_accept_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("text/event-stream;libra-wire=2"),
        );
        let query = CodeEventsQuery {
            wire: Some("1".into()),
            cursor: None,
        };
        assert_eq!(
            parse_code_events_wire_version(&query, &headers).unwrap(),
            CodeUiSseWireVersion::V1
        );
    }

    #[test]
    fn accept_header_selects_v2_when_query_omitted() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("text/event-stream; libra-wire=2"),
        );
        assert_eq!(
            parse_code_events_wire_version(&CodeEventsQuery::default(), &headers).unwrap(),
            CodeUiSseWireVersion::V2
        );
    }

    #[test]
    fn accept_header_ignores_prefix_lookalike_media_types() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("text/event-streaming;libra-wire=2"),
        );
        assert_eq!(
            parse_code_events_wire_version(&CodeEventsQuery::default(), &headers).unwrap(),
            CodeUiSseWireVersion::V1
        );
    }

    #[test]
    fn intent_revision_consumption_uses_dedicated_non_resolution_payload() {
        use crate::internal::ai::session::{
            CodeCommandIdentity, CodeCommandIntent, INTENT_REVISION_CONSUMER_COMMAND_KIND,
            INTENT_REVISION_CONSUMPTION_SCHEMA_VERSION, IntentRevisionConsumption,
            IntentRevisionConsumptionClaim,
        };

        let source = CodeCommandIdentity::new("repo", "session", "principal", "source");
        let consumer = CodeCommandIntent::new(
            CodeCommandIdentity::new("repo", "session", "principal", "consumer"),
            INTENT_REVISION_CONSUMER_COMMAND_KIND,
            "sha256:consumer",
            true,
        );
        let consumption = IntentRevisionConsumption {
            claim: IntentRevisionConsumptionClaim {
                schema_version: INTENT_REVISION_CONSUMPTION_SCHEMA_VERSION,
                interaction_id: "intent-review".to_string(),
                source_command: source,
                consumer_intent: consumer,
                terminal_event_id: Uuid::new_v4(),
                terminal_sequence: 2,
                intent_id: "intent-1".to_string(),
                sidecar_digest: Some(format!("hmac-sha256:{}", "a".repeat(64))),
            },
            consumer_intent_event_id: Uuid::new_v4(),
            consumer_intent_sequence: 3,
        };
        let event = CodeWorkflowEvent::new(
            4,
            CodeWorkflowEventKind::InteractionResolved {
                interaction_id: "intent-review".to_string(),
                resolution: "modify".to_string(),
                command: None,
                prior_interaction_resolutions: Vec::new(),
                intent_revision_consumption: Some(consumption.clone()),
            },
        );

        let wire = CodeUiWireV2Event::from_workflow_event(&event);
        assert_eq!(wire.kind, "intent_revision_consumed");
        assert_eq!(
            wire.payload,
            serde_json::json!({ "consumption": consumption })
        );
        assert!(wire.payload.get("event").is_none());
        assert!(wire.payload.get("resolution").is_none());
        assert!(wire.payload.get("priorInteractionResolutions").is_none());
    }

    #[test]
    fn transport_backlog_classifier_matches_bound_errors() {
        let over_count = io::Error::new(
            io::ErrorKind::InvalidData,
            "Code workflow replay after sequence 0 has 1025 events, exceeding the bounded limit of 1024; create a projection checkpoint before resuming",
        );
        let unprovable = io::Error::new(
            io::ErrorKind::InvalidData,
            "bounded Code workflow replay after sequence 1 cannot prove the retained tail of 'x' contains no omitted workflow events; create a projection checkpoint before resuming",
        );
        let truncated_gap = io::Error::new(
            io::ErrorKind::InvalidData,
            "Code UI wire v2 transport backlog window omitted workflow events between sequences 0 and 2; fetch a snapshot and reconnect at the durable tip",
        );
        let integrity_gap = io::Error::new(
            io::ErrorKind::InvalidData,
            "Code UI wire v2 cannot resume across missing workflow events between sequences 1 and 3",
        );
        let other = io::Error::new(io::ErrorKind::NotFound, "missing workflow log");
        assert!(transport_backlog_exceeded(&over_count));
        assert!(transport_backlog_exceeded(&unprovable));
        assert!(transport_backlog_exceeded(&truncated_gap));
        assert!(!transport_backlog_exceeded(&integrity_gap));
        assert!(!transport_backlog_exceeded(&other));
    }

    #[test]
    fn transport_byte_window_gap_is_resync_not_opaque_failure() {
        use tempfile::tempdir;

        use crate::internal::ai::session::{CodeWorkflowEventKind, SessionJsonlStore};

        let dir = tempdir().expect("tempdir");
        let mut store = SessionJsonlStore::new(dir.path().to_path_buf());
        let hub = CodeUiWorkflowHub::attach(&mut store).expect("attach");
        // Two ~5 MiB payloads: the 8 MiB transport window cannot cover both,
        // so bootstrap from 0 must classify as transport backlog (resync).
        let big = "x".repeat(5 * 1024 * 1024);
        for summary in ["byte-a", "byte-b"] {
            store
                .append_code_workflow(CodeWorkflowEventKind::CodeUiProjectionDelta {
                    projection: "status".to_string(),
                    summary: summary.to_string(),
                    payload: serde_json::json!({ "blob": big }),
                })
                .expect("append oversized");
        }
        let err = hub
            .replay_after(0)
            .expect_err("two 5MiB rows must exceed the 8MiB transport window");
        assert!(
            transport_backlog_exceeded(&err),
            "byte-window truncation/gap must be resync-classed, got: {err}"
        );
    }

    #[test]
    fn oversized_publish_sends_tip_not_full_event() {
        use tempfile::tempdir;

        use crate::internal::ai::session::{CodeWorkflowEventKind, SessionJsonlStore};

        let dir = tempdir().expect("tempdir");
        let mut store = SessionJsonlStore::new(dir.path().to_path_buf());
        let hub = CodeUiWorkflowHub::attach(&mut store).expect("attach");
        let mut rx = hub.subscribe();
        let big = "z".repeat(5 * 1024 * 1024);
        for summary in ["huge-a", "huge-b"] {
            store
                .append_code_workflow(CodeWorkflowEventKind::CodeUiProjectionDelta {
                    projection: "status".to_string(),
                    summary: summary.to_string(),
                    payload: serde_json::json!({ "blob": big }),
                })
                .expect("append oversized");
        }
        // First ~5 MiB row fits the 8 MiB ring budget as a full event.
        match rx.try_recv().expect("first notify") {
            CodeUiWorkflowLiveNotify::Event(event) => assert_eq!(event.sequence, 1),
            CodeUiWorkflowLiveNotify::Tip { sequence } => {
                panic!("first in-budget row should be a full event, got tip {sequence}")
            }
        }
        // Second ~5 MiB row would push the retained ring past 8 MiB → tip-only.
        match rx.try_recv().expect("second notify") {
            CodeUiWorkflowLiveNotify::Tip { sequence } => assert_eq!(sequence, 2),
            CodeUiWorkflowLiveNotify::Event(_) => {
                panic!("over-budget publish must not retain another full payload in the ring")
            }
        }
        store
            .append_code_workflow(CodeWorkflowEventKind::CodeUiProjectionDelta {
                projection: "status".to_string(),
                summary: "huge-c".to_string(),
                payload: serde_json::json!({ "blob": big }),
            })
            .expect("append third oversized");
        match rx.try_recv().expect("third notify") {
            CodeUiWorkflowLiveNotify::Tip { sequence } => assert_eq!(sequence, 3),
            CodeUiWorkflowLiveNotify::Event(_) => {
                panic!(
                    "while the 5 MiB slot remains in the ring, later 5 MiB publishes must be tips"
                )
            }
        }
    }

    #[test]
    fn count_rollover_restores_full_event_fast_path() {
        use tempfile::tempdir;

        use crate::internal::ai::session::{CodeWorkflowEventKind, SessionJsonlStore};

        let dir = tempdir().expect("tempdir");
        let mut store = SessionJsonlStore::new(dir.path().to_path_buf());
        let hub = CodeUiWorkflowHub::attach(&mut store).expect("attach");
        let mut rx = hub.subscribe();
        let n = CODE_UI_TRANSPORT_BROADCAST_CAPACITY + 1;
        let kinds: Vec<_> = (0..n)
            .map(|i| CodeWorkflowEventKind::CodeUiProjectionDelta {
                projection: "status".to_string(),
                summary: format!("roll-{i}"),
                payload: serde_json::json!({}),
            })
            .collect();
        store
            .append_code_workflow_batch(&kinds)
            .expect("fill then roll the ring");
        let mut last = None;
        loop {
            match rx.try_recv() {
                Ok(notify) => last = Some(notify),
                Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
        match last.expect("at least one notify after draining lag") {
            CodeUiWorkflowLiveNotify::Event(event) => {
                assert_eq!(
                    event.sequence, n as u64,
                    "rolled slot should be a full event"
                )
            }
            CodeUiWorkflowLiveNotify::Tip { sequence } => {
                panic!("small in-budget rollover must restore Event fan-out, got tip {sequence}")
            }
        }
    }

    #[test]
    fn oversized_tip_record_idle_resume_does_not_resync_loop() {
        use tempfile::tempdir;

        use crate::internal::ai::session::{CodeWorkflowEventKind, SessionJsonlStore};

        let dir = tempdir().expect("tempdir");
        let mut store = SessionJsonlStore::new(dir.path().to_path_buf());
        let hub = CodeUiWorkflowHub::attach(&mut store).expect("attach");
        let huge = "w".repeat(9 * 1024 * 1024);
        store
            .append_code_workflow(CodeWorkflowEventKind::CodeUiProjectionDelta {
                projection: "status".to_string(),
                summary: "single-oversize".to_string(),
                payload: serde_json::json!({ "blob": huge }),
            })
            .expect("append >8MiB tip");
        let tail = hub.durable_tail_sequence();
        assert_eq!(tail, 1);
        let boot = hub
            .replay_after(0)
            .expect_err("bootstrap cannot cover a >8MiB tip record");
        assert!(transport_backlog_exceeded(&boot));
        let idle = hub
            .replay_after(tail)
            .expect("reconnect at oversized durable tip must be empty, not a resync loop");
        assert!(idle.is_empty());
    }
}
