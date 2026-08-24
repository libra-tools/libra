//! `libra agent bridge --stdio` — repository-scoped JSON-RPC 2.0 NDJSON
//! ingress for the DeepSeek Harness plugin (plan-20260818 LB-01).
//!
//! This module is the **protocol authority** for the bridge (GC-LB-02): the
//! protocol version, capability matrix, method allowlist, frame/batch limits,
//! error catalogue and retryability live here and nowhere else. The
//! TypeScript plugin (`deepseek-harness-libra-plugin`) consumes the fixture
//! published from this definition; it must never invent a second schema.
//!
//! ## Non-goals (ADR-LB-01 / ADR-LB-02)
//!
//! - This is NOT `libra code --control stdio` (a client controlling a live
//!   Code session) and NOT an MCP server. It is the only standard inbound
//!   write transport for Harness.
//! - `deepseek-harness` is NOT added to the `AgentKind` provider roster. The
//!   bridge uses its own source identity and durable tables (LB-02).

pub mod authorization;
pub mod ingress;
pub mod methods;
pub mod mutations;
pub mod protocol;
pub mod provenance;
pub mod redaction;
pub mod storage;
pub mod transport;
pub mod vcs;
pub mod workspace;

pub use protocol::{
    BridgeCapability, BridgeError, BridgeRequest, BridgeResponse, MethodAllowlist, ProtocolVersion,
    SOURCE_DEEPSEEK_HARNESS,
};

/// Default request deadline (seconds) for a single bridge request. Requests
/// that do not complete within this window are failed with a retryable error.
pub const REQUEST_DEADLINE_SECS: u64 = 30;
