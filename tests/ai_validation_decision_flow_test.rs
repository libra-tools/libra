//! Phase D validation and decision derived-record tests.
//!
//! Verifies the read/write contract for the two latest-derived runtime tables:
//! - `ai_validation_report` — the `ValidationReportStore` keeps exactly one
//!   `is_latest = 1` row per thread, replacing any prior latest report.
//! - `ai_decision_proposal` — `DecisionProposalStore` writes proposals derived
//!   from a validator report and a risk score, and routes/verdicts respect
//!   blocking-failure semantics.
//! - When neither a thread row nor a runtime anchor exists, the stores create the
//!   anchor (`owner_kind = "system"`, `owner_id = "libra-runtime"`) on demand.
//!
//! **Layer:** L1 — uses in-memory SQLite, no external dependencies.

use chrono::Utc;
use libra::internal::{
    ai::{
        runtime::{
            DecisionPolicy, DecisionProposalRoute, DecisionProposalStore, FinalDecision,
            FinalDecisionStore, ValidationOutcome, ValidationReportStore, ValidationStage,
            ValidationStageResult, ValidatorEngine, aggregate_risk_score, build_decision_proposal,
            contracts::{EvidenceKind, FinalDecisionVerdict},
            phase3::ValidationStatus,
        },
        session::jsonl::SessionJsonlStore,
    },
    model::{ai_thread, ai_validation_report},
};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, Database, EntityTrait,
    QueryFilter, Statement,
};
use uuid::Uuid;

const BOOTSTRAP_SQL: &str = include_str!("../sql/sqlite_20260309_init.sql");

/// Spin up an in-memory SQLite and run the canonical bootstrap SQL. Returns the
/// connection ready for tests that only need schema (no on-disk Libra repo).
async fn setup_db() -> sea_orm::DatabaseConnection {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    db.execute_raw(Statement::from_string(
        db.get_database_backend(),
        BOOTSTRAP_SQL,
    ))
    .await
    .unwrap();
    db
}

/// Scenario: sequence two validation outcomes for the same thread (a Passed run that
/// auto-accepts, then a BlockingFailed run that requests changes). Asserts:
/// - The first proposal routes to `AutoAccept` with verdict `Accepted` and no human
///   review required.
/// - The second proposal routes to `RequestChanges` with verdict `Rejected` and
///   `requires_human_review = true`.
/// - The second write supersedes the first: only one row remains marked
///   `is_latest = 1` for the thread, and `load_latest_proposal` returns the new one.
///
/// Pins the latest-derived semantics that downstream UIs and webhooks rely on.
#[tokio::test]
async fn validation_reports_and_decision_proposals_are_latest_derived_records() {
    let db = setup_db().await;
    let thread_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    ai_thread::ActiveModel {
        thread_id: Set(thread_id.to_string()),
        title: Set(Some("validation decision flow".to_string())),
        owner_kind: Set("human".to_string()),
        owner_id: Set("tester".to_string()),
        owner_display_name: Set(None),
        current_intent_id: Set(None),
        latest_intent_id: Set(None),
        metadata_json: Set(None),
        archived: Set(false),
        version: Set(1),
        created_at: Set(1_700_000_000),
        updated_at: Set(1_700_000_000),
    }
    .insert(&db)
    .await
    .unwrap();
    let validator = ValidatorEngine::default_policy();
    let validation_store = ValidationReportStore::new(db.clone());
    let decision_store = DecisionProposalStore::new(db.clone());
    let policy = DecisionPolicy::default();

    let passed = validator.build_report(
        thread_id,
        Some(run_id),
        vec![
            ValidationStageResult {
                stage: ValidationStage::Integration,
                outcome: ValidationOutcome::Passed,
                evidence: vec![EvidenceKind::Test],
                summary: Some("cargo test passed".to_string()),
            },
            ValidationStageResult {
                stage: ValidationStage::Security,
                outcome: ValidationOutcome::Passed,
                evidence: vec![EvidenceKind::Security],
                summary: Some("no security blockers".to_string()),
            },
            ValidationStageResult {
                stage: ValidationStage::Release,
                outcome: ValidationOutcome::Passed,
                evidence: vec![EvidenceKind::Build],
                summary: Some("release checks passed".to_string()),
            },
        ],
    );
    validation_store.write_latest(&passed).await.unwrap();

    let passed_risk = aggregate_risk_score(&passed, &policy);
    let passed_proposal = build_decision_proposal(&passed, &passed_risk, &policy);
    decision_store
        .write_latest(&passed_risk, &passed_proposal)
        .await
        .unwrap();

    let loaded_proposal = decision_store
        .load_latest_proposal(thread_id)
        .await
        .unwrap()
        .expect("decision proposal");
    assert_eq!(
        loaded_proposal.summary.route,
        DecisionProposalRoute::AutoAccept
    );
    assert_eq!(
        loaded_proposal.summary.proposed_verdict,
        FinalDecisionVerdict::Accepted
    );
    assert!(!loaded_proposal.summary.requires_human_review);

    let blocking = validator.build_report(
        thread_id,
        Some(run_id),
        vec![ValidationStageResult {
            stage: ValidationStage::Integration,
            outcome: ValidationOutcome::BlockingFailed,
            evidence: vec![EvidenceKind::ValidationBlockingFailed],
            summary: Some("required test failed".to_string()),
        }],
    );
    validation_store.write_latest(&blocking).await.unwrap();
    let blocking_risk = aggregate_risk_score(&blocking, &policy);
    let blocking_proposal = build_decision_proposal(&blocking, &blocking_risk, &policy);
    decision_store
        .write_latest(&blocking_risk, &blocking_proposal)
        .await
        .unwrap();

    let latest_report = validation_store
        .load_latest(thread_id)
        .await
        .unwrap()
        .expect("latest validation report");
    assert_eq!(latest_report.report_id, blocking.report_id);
    assert_eq!(
        latest_report.summary.status,
        ValidationStatus::BlockingFailed
    );

    let latest_proposal = decision_store
        .load_latest_proposal(thread_id)
        .await
        .unwrap()
        .expect("latest decision proposal");
    assert_eq!(
        latest_proposal.summary.route,
        DecisionProposalRoute::RequestChanges
    );
    assert_eq!(
        latest_proposal.summary.proposed_verdict,
        FinalDecisionVerdict::Rejected
    );
    assert!(latest_proposal.summary.requires_human_review);

    let latest_rows = ai_validation_report::Entity::find()
        .filter(ai_validation_report::Column::ThreadId.eq(thread_id.to_string()))
        .filter(ai_validation_report::Column::IsLatest.eq(1))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(latest_rows.len(), 1);
}

/// Scenario: write derived records for a thread_id that has no `ai_thread` row yet
/// and confirm the stores create a runtime-owned anchor (`owner_kind = "system"`,
/// `owner_id = "libra-runtime"`). This is the auto-bootstrap path the runtime uses
/// when a validation/decision arrives before a thread is otherwise materialized.
#[tokio::test]
async fn derived_records_create_missing_runtime_thread_anchor() {
    let db = setup_db().await;
    let thread_id = Uuid::new_v4();
    let validator = ValidatorEngine::default_policy();
    let validation_store = ValidationReportStore::new(db.clone());
    let decision_store = DecisionProposalStore::new(db.clone());
    let policy = DecisionPolicy::default();

    let report = validator.build_report(
        thread_id,
        None,
        vec![ValidationStageResult {
            stage: ValidationStage::Integration,
            outcome: ValidationOutcome::Passed,
            evidence: vec![EvidenceKind::Test],
            summary: Some("integration passed".to_string()),
        }],
    );

    validation_store.write_latest(&report).await.unwrap();
    let risk = aggregate_risk_score(&report, &policy);
    let proposal = build_decision_proposal(&report, &risk, &policy);
    decision_store.write_latest(&risk, &proposal).await.unwrap();

    let anchor = ai_thread::Entity::find_by_id(thread_id.to_string())
        .one(&db)
        .await
        .unwrap()
        .expect("runtime thread anchor");
    assert_eq!(anchor.owner_kind, "system");
    assert_eq!(anchor.owner_id, "libra-runtime");

    let loaded_report = validation_store
        .load_latest(thread_id)
        .await
        .unwrap()
        .expect("latest validation report");
    assert_eq!(loaded_report.report_id, report.report_id);

    let loaded_proposal = decision_store
        .load_latest_proposal(thread_id)
        .await
        .unwrap()
        .expect("latest decision proposal");
    assert_eq!(loaded_proposal.proposal_id, proposal.proposal_id);
}

/// Scenario: Phase 3/4 derived records are written through the session mirror
/// path as well as the SQLite latest-derived tables. Asserts that a single
/// operator tail of `events.jsonl` can consume the validation report, risk score,
/// and decision proposal without joining the SeaORM tables.
#[tokio::test]
async fn validation_and_decision_writes_emit_ai_artifact_jsonl_mirror() {
    let db = setup_db().await;
    let thread_id = Uuid::new_v4();
    let session_dir = tempfile::tempdir().unwrap();
    let session_store = SessionJsonlStore::new(session_dir.path().join("session"));
    let validator = ValidatorEngine::default_policy();
    let validation_store = ValidationReportStore::new(db.clone());
    let decision_store = DecisionProposalStore::new(db);
    let policy = DecisionPolicy::default();

    let report = validator.build_report(
        thread_id,
        None,
        vec![ValidationStageResult {
            stage: ValidationStage::Integration,
            outcome: ValidationOutcome::Passed,
            evidence: vec![EvidenceKind::Test],
            summary: Some("integration passed".to_string()),
        }],
    );
    validation_store
        .write_latest_with_session_mirror(&report, &session_store)
        .await
        .unwrap();

    let risk = aggregate_risk_score(&report, &policy);
    let proposal = build_decision_proposal(&report, &risk, &policy);
    decision_store
        .write_latest_with_session_mirror(&risk, &proposal, &session_store)
        .await
        .unwrap();

    let artifacts = session_store.load_ai_artifacts().unwrap();
    let kinds = artifacts
        .iter()
        .map(|event| event.artifact_kind.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        vec![
            "validation_report",
            "risk_score_breakdown",
            "decision_proposal"
        ]
    );
    assert!(artifacts.iter().all(|event| event.thread_id == thread_id));
    assert_eq!(artifacts[0].artifact_id, Some(report.report_id.to_string()));
    assert_eq!(
        artifacts[1].artifact_id,
        Some(risk.breakdown_id.to_string())
    );
    assert_eq!(
        artifacts[2].artifact_id,
        Some(proposal.proposal_id.to_string())
    );
    assert_eq!(
        artifacts[2]
            .payload
            .get("summary")
            .and_then(|summary| summary.get("route"))
            .and_then(serde_json::Value::as_str),
        Some("auto_accept")
    );
}

/// Phase 4 completion: an AutoAccept proposal finalises into a persisted
/// `ai_final_decision` row recording the resolved verdict, and `load_latest`
/// round-trips it (verdict + linkage + latest flag). `write_latest`'s
/// `ensure_runtime_thread` creates the thread anchor on demand, so no manual
/// `ai_thread` insert is needed.
#[tokio::test]
async fn final_decision_store_round_trips_auto_accept_verdict() {
    let db = setup_db().await;
    let thread_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();

    let validator = ValidatorEngine::default_policy();
    let report = validator.build_report(
        thread_id,
        Some(run_id),
        vec![
            ValidationStageResult {
                stage: ValidationStage::Integration,
                outcome: ValidationOutcome::Passed,
                evidence: vec![EvidenceKind::Test],
                summary: Some("cargo test passed".to_string()),
            },
            ValidationStageResult {
                stage: ValidationStage::Release,
                outcome: ValidationOutcome::Passed,
                evidence: vec![EvidenceKind::Build],
                summary: Some("release checks passed".to_string()),
            },
        ],
    );
    let policy = DecisionPolicy::default();
    let risk = aggregate_risk_score(&report, &policy);
    let proposal = build_decision_proposal(&report, &risk, &policy);
    assert_eq!(proposal.summary.route, DecisionProposalRoute::AutoAccept);

    let decision = FinalDecision::finalize_auto_accept(&proposal, Utc::now())
        .expect("an AutoAccept proposal must finalise into a Decision");
    let store = FinalDecisionStore::new(db.clone());
    store
        .write_latest(&decision)
        .await
        .expect("final decision write_latest must succeed");

    let loaded = store
        .load_latest(thread_id)
        .await
        .expect("load_latest query must succeed")
        .expect("a final decision row must exist for the thread");
    assert_eq!(loaded.decision_id, decision.decision_id);
    assert_eq!(loaded.verdict, FinalDecisionVerdict::Accepted);
    assert_eq!(loaded.decision_proposal_id, Some(proposal.proposal_id));
    assert_eq!(loaded.validation_report_id, proposal.validation_report_id);
    assert!(loaded.is_latest);
    assert!(!loaded.stale);
    assert_eq!(loaded.summary.route, DecisionProposalRoute::AutoAccept);
}
