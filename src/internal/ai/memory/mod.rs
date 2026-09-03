#![allow(
    dead_code,
    reason = "M2-01 freezes I/O-free contracts before the M2-04 writer consumes them"
)]

//! Versioned Agent Memory domain contracts.
//!
//! This module intentionally exposes only validated, I/O-free domain values.
//! Storage, projection, compilation, and command adapters are implemented by
//! later plan slices and must not become alternate write seams.

mod admission;
mod applicability;
mod canonical;
mod compiler;
mod diagnostics;
mod domain;
mod error;
mod evidence;
mod fts_sql;
mod job;
mod job_sql;
mod job_state;
mod limits;
mod observer;
mod policy;
mod projection;
mod query;
mod reader;
mod replay;
mod runner;
mod runtime;
mod selector;
mod source;
mod store;
mod tree;
mod validation;
mod view;
mod writer;

pub(crate) use applicability::CodeApplicability;
pub(crate) use diagnostics::{
    MemoryDiagnostics, MemoryJobStatus, MemoryRebuildReport, MemoryStatusReport,
};
pub(crate) use domain::{
    CodeChangeStatus, CompletionStatus, EpisodeRootKind, EvidenceKind, EvidenceRefV1,
    EvidenceSourcePlane, MemoryNoteV1, MemoryScopeV1, MemorySensitivity, MemoryTrust,
};
#[cfg(test)]
pub(crate) use error::MemoryDamagePoint;
pub(crate) use error::{MemoryWriterError, MemoryWriterErrorKind};
pub(crate) use evidence::EvidenceOmissionReason;
pub(crate) use fts_sql::validate_plain_text_query;
pub(crate) use job::schedule_observer_repair;
pub(crate) use policy::AuthenticatedMemoryContext;
pub(crate) use query::{EpisodePathFilter, EpisodeQueryV1, MAX_CANDIDATES, MAX_RESULT_LIMIT};
#[cfg(test)]
pub(crate) use reader::tests::{
    commit_injectable_episode as memory_test_commit_injectable_episode,
    history as memory_test_history, seed_code_head as memory_test_seed_code_head,
};
pub(crate) use reader::{
    EpisodeReadItemV1, EpisodeReader, EpisodeReaderError, EpisodeReaderErrorKind,
};
pub(crate) use runtime::{MemoryRuntime, MemoryRuntimeErrorKind};
pub(crate) use view::ResolvedMemoryViewV1;
#[cfg(test)]
pub(crate) use writer::tests::fixture as memory_test_fixture;
