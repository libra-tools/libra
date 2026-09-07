use std::sync::Arc;

use chrono::{TimeZone, Utc};
use git_internal::internal::object::{
    intent::Intent,
    intent_event::{IntentEvent, IntentEventKind},
    types::ActorRef,
};
use libra::{
    internal::{
        ai::history::HistoryManager,
        db,
        publish::{
            ai_export::{
                AiExportError, AiExportRequest, HistoryAiExportRequest,
                ai_history_object_type_specs, ai_object_model_type_specs, build_ai_export_plan,
                collect_publish_ai_objects_from_history, publish_ai_bundle_key,
                publish_ai_bundle_relative_key, publish_ai_graph_key,
                publish_ai_graph_relative_key, publish_ai_index_key, publish_ai_index_relative_key,
                publish_ai_object_key, publish_ai_object_relative_key,
            },
            contract::{
                AiBundleAssociatedIds, AiBundleIndexes, AiObjectLayer, AiObjectRedaction,
                AiObjectRelationship, PUBLISH_SCHEMA_VERSION, PublishAiObject, RedactionMode,
            },
            snapshot::sha256_hex,
        },
    },
    utils::{storage::local::LocalStorage, storage_ext::StorageExt, test},
};
use serial_test::serial;

const REPO_ID: &str = "11111111-2222-3333-4444-555555555555";
const SITE_ID: &str = "00000000-0000-0000-0000-0000publish01";
const REVISION_OID: &str = "abcdef0123456789abcdef0123456789abcdef01";
const AI_VERSION_ID: &str = "ai-version-2026-05-13-001";
const RULES_VERSION: &str = "2026.05.13-1";

/// The relative R2 keys (used as the per-site object paths) must keep
/// a fixed `revisions/<oid>/ai/...` layout, and every absolute key must
/// be exactly `<repo>/publish/sites/<site>/<relative_key>`. If an
/// absolute builder and its relative builder ever diverge, a published
/// AI object would be uploaded under one key but referenced in the
/// index under another — silently unreachable.
///
/// `publish_upload_test.rs` hard-codes the relative path *strings* when
/// fetching uploaded artifacts, but never calls the relative-key
/// *builder functions* nor pins the absolute = prefix + relative
/// relationship — so a builder that diverged from those hard-coded
/// strings would slip through there. This test closes that gap: it
/// exercises the builder functions directly and pins the composition
/// invariant.
#[test]
fn publish_ai_relative_keys_compose_into_absolute_keys() {
    // Literal relative-key shapes (the per-revision site-relative paths).
    assert_eq!(
        publish_ai_index_relative_key(REVISION_OID),
        format!("revisions/{REVISION_OID}/ai/index.json"),
    );
    assert_eq!(
        publish_ai_graph_relative_key(REVISION_OID),
        format!("revisions/{REVISION_OID}/ai/graph.json"),
    );
    assert_eq!(
        publish_ai_bundle_relative_key(REVISION_OID, AI_VERSION_ID),
        format!("revisions/{REVISION_OID}/ai/bundles/{AI_VERSION_ID}.json"),
    );
    assert_eq!(
        publish_ai_object_relative_key(REVISION_OID, "snapshot", "Run", "run-1"),
        format!("revisions/{REVISION_OID}/ai/objects/snapshot/Run/run-1.json"),
    );

    // Absolute = `<repo>/publish/sites/<site>/<relative>` for every key
    // family — the consistency that keeps uploaded objects reachable
    // through the index.
    let prefix = format!("{REPO_ID}/publish/sites/{SITE_ID}/");
    assert_eq!(
        publish_ai_index_key(REPO_ID, SITE_ID, REVISION_OID),
        format!("{prefix}{}", publish_ai_index_relative_key(REVISION_OID)),
    );
    assert_eq!(
        publish_ai_graph_key(REPO_ID, SITE_ID, REVISION_OID),
        format!("{prefix}{}", publish_ai_graph_relative_key(REVISION_OID)),
    );
    assert_eq!(
        publish_ai_bundle_key(REPO_ID, SITE_ID, REVISION_OID, AI_VERSION_ID),
        format!(
            "{prefix}{}",
            publish_ai_bundle_relative_key(REVISION_OID, AI_VERSION_ID)
        ),
    );
    assert_eq!(
        publish_ai_object_key(
            REPO_ID,
            SITE_ID,
            REVISION_OID,
            AiObjectLayer::Snapshot,
            "Run",
            "run-1",
        ),
        format!(
            "{prefix}{}",
            publish_ai_object_relative_key(REVISION_OID, "snapshot", "Run", "run-1")
        ),
    );
}

/// The object-key layer segment must map each `AiObjectLayer` to its
/// stable lowercase directory (`snapshot` / `event` / `projection`).
/// A wrong mapping would scatter objects across layer dirs that the
/// index doesn't reference. Pin all three.
#[test]
fn publish_ai_object_key_maps_each_layer_to_its_directory() {
    for (layer, dir) in [
        (AiObjectLayer::Snapshot, "snapshot"),
        (AiObjectLayer::Event, "event"),
        (AiObjectLayer::Projection, "projection"),
    ] {
        let key = publish_ai_object_key(REPO_ID, SITE_ID, REVISION_OID, layer, "Run", "run-1");
        assert_eq!(
            key,
            format!(
                "{REPO_ID}/publish/sites/{SITE_ID}/revisions/{REVISION_OID}/ai/objects/{dir}/Run/run-1.json"
            ),
            "layer {layer:?} must map to the `{dir}` object directory",
        );
    }
}

#[test]
fn publish_ai_export_test_builds_index_graph_bundle_and_storage_keys() {
    let generated_at = timestamp();
    let plan = build_ai_export_plan(AiExportRequest {
        repo_id: REPO_ID.to_string(),
        site_id: SITE_ID.to_string(),
        revision_oid: REVISION_OID.to_string(),
        ai_version_id: AI_VERSION_ID.to_string(),
        generated_at,
        ai_object_model_reference: "docs/agent/ai-object-model-reference.md".to_string(),
        redaction_mode: RedactionMode::Default,
        redaction_rules_version: RULES_VERSION.to_string(),
        associated_ids: AiBundleAssociatedIds {
            thread_id: Some("thread-1".to_string()),
            ..AiBundleAssociatedIds::default()
        },
        objects: vec![intent_object(), plan_object(), run_object()],
    })
    .expect("fixture objects should export");

    assert_eq!(
        plan.index_key,
        publish_ai_index_key(REPO_ID, SITE_ID, REVISION_OID)
    );
    assert_eq!(
        plan.graph_key,
        publish_ai_graph_key(REPO_ID, SITE_ID, REVISION_OID)
    );
    assert_eq!(
        plan.bundle_key,
        publish_ai_bundle_key(REPO_ID, SITE_ID, REVISION_OID, AI_VERSION_ID)
    );
    assert_eq!(plan.objects.len(), 3);
    assert_eq!(plan.index.schema_version, PUBLISH_SCHEMA_VERSION);
    assert_eq!(plan.index.objects.len(), 3);
    assert_eq!(plan.index.bundles.len(), 1);
    assert_eq!(plan.index.bundles[0].object_count, 3);
    assert_eq!(plan.index.bundles[0].bundle_key, plan.bundle_key);
    assert_eq!(
        plan.index.bundles[0].bundle_sha256,
        sha256_hex(&serde_json::to_vec(&plan.bundle).expect("bundle must serialize"))
    );

    let run_export = plan
        .objects
        .iter()
        .find(|entry| entry.object.object_type == "Run")
        .expect("run object must be exported");
    assert_eq!(
        run_export.r2_key,
        publish_ai_object_key(
            REPO_ID,
            SITE_ID,
            REVISION_OID,
            AiObjectLayer::Snapshot,
            "Run",
            "run-1"
        )
    );
    assert_eq!(
        run_export.payload_sha256,
        sha256_hex(&serde_json::to_vec(&run_export.object).expect("object must serialize"))
    );

    assert_eq!(plan.graph.nodes.len(), 3);
    assert!(
        plan.graph
            .edges
            .iter()
            .any(|edge| edge.from_object_type == "Run" && edge.to_object_type == "Plan")
    );
    assert_eq!(
        plan.bundle.indexes.by_plan.get("plan-1"),
        Some(&vec![
            "Intent/intent-1".to_string(),
            "Run/run-1".to_string()
        ])
    );
    assert_eq!(
        plan.bundle.indexes.by_run.get("run-1"),
        Some(&vec!["Plan/plan-1".to_string()])
    );
    assert_eq!(plan.bundle.redaction.removed_field_count, 2);
    assert_eq!(
        plan.bundle.redaction.removed_fields_by_type.get("Run"),
        Some(&vec![
            "payload.absoluteWorkspacePath".to_string(),
            "payload.providerRawResponse".to_string()
        ])
    );
    assert_eq!(
        plan.bundle.redaction.object_counts_by_type.get("Run"),
        Some(&1)
    );
}

#[test]
fn publish_ai_export_test_accepts_every_reference_object_type() {
    let generated_at = timestamp();
    let objects = ai_object_model_type_specs()
        .iter()
        .map(|spec| {
            let object_id = format!("{}-1", spec.object_type);
            object(
                spec.object_type,
                &object_id,
                spec.layer,
                serde_json::json!({
                    "objectType": spec.object_type,
                    "objectId": object_id,
                    "layer": spec.layer,
                }),
                vec![],
                vec![edge(
                    spec.object_type,
                    &object_id,
                    "self",
                    spec.object_type,
                    &object_id,
                )],
            )
        })
        .collect::<Vec<_>>();

    let plan = build_ai_export_plan(AiExportRequest {
        repo_id: REPO_ID.to_string(),
        site_id: SITE_ID.to_string(),
        revision_oid: REVISION_OID.to_string(),
        ai_version_id: AI_VERSION_ID.to_string(),
        generated_at,
        ai_object_model_reference: "docs/agent/ai-object-model-reference.md".to_string(),
        redaction_mode: RedactionMode::Default,
        redaction_rules_version: RULES_VERSION.to_string(),
        associated_ids: AiBundleAssociatedIds::default(),
        objects,
    })
    .expect("all reference AI object types should export");

    assert_eq!(plan.index.objects.len(), ai_object_model_type_specs().len());
    for spec in ai_object_model_type_specs() {
        let object_id = format!("{}-1", spec.object_type);
        assert_eq!(
            plan.bundle
                .redaction
                .object_counts_by_type
                .get(spec.object_type),
            Some(&1),
            "{} should be counted in redaction summary",
            spec.object_type
        );
        let indexed = plan
            .index
            .objects
            .iter()
            .find(|entry| entry.object_type == spec.object_type)
            .unwrap_or_else(|| panic!("{} should be listed in ai/index", spec.object_type));
        assert_eq!(
            indexed.layer, spec.layer,
            "{} should use the reference layer",
            spec.object_type
        );
        assert!(
            any_index_bucket_contains(&plan.bundle.indexes, &object_id),
            "{} should have a relationship index bucket",
            spec.object_type
        );
    }
}

#[test]
fn publish_ai_export_test_maps_history_storage_types_to_reference_types() {
    let specs = ai_history_object_type_specs();

    let snapshot = specs
        .iter()
        .find(|spec| spec.history_type == "snapshot")
        .expect("legacy context snapshot storage name should be mapped");
    assert_eq!(snapshot.object_type, "ContextSnapshot");
    assert_eq!(snapshot.layer, AiObjectLayer::Snapshot);

    let invocation = specs
        .iter()
        .find(|spec| spec.history_type == "invocation")
        .expect("tool invocation storage name should be mapped");
    assert_eq!(invocation.object_type, "ToolInvocation");
    assert_eq!(invocation.layer, AiObjectLayer::Event);
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn publish_ai_export_test_collects_snapshot_and_event_objects_from_history() {
    let (temp, storage, history) = setup_history_repo().await;
    let _keep_temp = temp;

    let actor = ActorRef::human("publish-test").expect("actor");
    let intent = Intent::new(actor.clone(), "Publish AI object model").expect("intent");
    let intent_hash = storage
        .put_tracked(&intent, &history)
        .await
        .expect("intent should be tracked");
    let mut event = IntentEvent::new(
        actor,
        intent.header().object_id(),
        IntentEventKind::Analyzed,
    )
    .expect("intent event");
    event.set_reason(Some("analysis complete".to_string()));
    let event_hash = storage
        .put_tracked(&event, &history)
        .await
        .expect("intent event should be tracked");

    let objects = collect_publish_ai_objects_from_history(
        &history,
        storage.as_ref(),
        HistoryAiExportRequest {
            site_id: SITE_ID.to_string(),
            revision_oid: REVISION_OID.to_string(),
            source_ref: "refs/heads/main".to_string(),
            redaction_mode: RedactionMode::Default,
            redaction_rules_version: RULES_VERSION.to_string(),
        },
    )
    .await
    .expect("history objects should convert into publish objects");

    assert_eq!(objects.len(), 2);
    let intent_object = objects
        .iter()
        .find(|object| object.object_type == "Intent")
        .expect("intent should export");
    assert_eq!(
        intent_object.object_id,
        intent.header().object_id().to_string()
    );
    assert_eq!(intent_object.layer, AiObjectLayer::Snapshot);
    assert_eq!(intent_object.redaction.mode, RedactionMode::Default);
    assert!(
        intent_object
            .source_refs
            .contains(&"refs/heads/main".to_string())
    );
    assert!(
        intent_object.source_refs.contains(&format!(
            "history/intent/{}@{}",
            intent.header().object_id(),
            intent_hash
        )),
        "intent source refs should preserve the history blob pointer"
    );

    let event_object = objects
        .iter()
        .find(|object| object.object_type == "IntentEvent")
        .expect("intent event should export");
    assert_eq!(
        event_object.object_id,
        event.header().object_id().to_string()
    );
    assert_eq!(event_object.layer, AiObjectLayer::Event);
    assert!(event_object.source_refs.contains(&format!(
        "history/intent_event/{}@{}",
        event.header().object_id(),
        event_hash
    )));
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn publish_ai_export_test_redacts_sensitive_history_payload_fields() {
    let (temp, storage, history) = setup_history_repo().await;
    let _keep_temp = temp;
    let raw_run = serde_json::json!({
        "safe": "visible",
        "providerRawResponse": "secret provider transcript",
        "nested": {
            "absoluteWorkspacePath": "/tmp/secret-workspace"
        }
    });
    let hash = storage
        .put_json(&raw_run)
        .await
        .expect("raw history fixture should store");
    history
        .append("run", "run-redacted", hash)
        .await
        .expect("raw history fixture should be tracked");

    let objects = collect_publish_ai_objects_from_history(
        &history,
        storage.as_ref(),
        HistoryAiExportRequest {
            site_id: SITE_ID.to_string(),
            revision_oid: REVISION_OID.to_string(),
            source_ref: "refs/heads/main".to_string(),
            redaction_mode: RedactionMode::Default,
            redaction_rules_version: RULES_VERSION.to_string(),
        },
    )
    .await
    .expect("history object should convert");

    let run_object = objects
        .iter()
        .find(|object| object.object_type == "Run")
        .expect("run should export");
    assert_eq!(run_object.payload["safe"], "visible");
    assert!(run_object.payload.get("providerRawResponse").is_none());
    assert!(
        run_object.payload["nested"]
            .get("absoluteWorkspacePath")
            .is_none()
    );
    assert_eq!(
        run_object.removed_fields,
        vec![
            "payload.nested.absoluteWorkspacePath".to_string(),
            "payload.providerRawResponse".to_string()
        ]
    );
}

#[test]
fn publish_ai_export_test_rejects_relationships_with_missing_endpoint() {
    let err = build_ai_export_plan(AiExportRequest {
        repo_id: REPO_ID.to_string(),
        site_id: SITE_ID.to_string(),
        revision_oid: REVISION_OID.to_string(),
        ai_version_id: AI_VERSION_ID.to_string(),
        generated_at: timestamp(),
        ai_object_model_reference: "docs/agent/ai-object-model-reference.md".to_string(),
        redaction_mode: RedactionMode::Default,
        redaction_rules_version: RULES_VERSION.to_string(),
        associated_ids: AiBundleAssociatedIds::default(),
        objects: vec![run_object()],
    })
    .expect_err("missing Plan endpoint must fail");

    assert!(matches!(
        err,
        AiExportError::MissingRelationshipEndpoint {
            from_object_type,
            to_object_type,
            ..
        } if from_object_type == "Run" && to_object_type == "Plan"
    ));
}

fn any_index_bucket_contains(indexes: &AiBundleIndexes, object_id: &str) -> bool {
    [
        &indexes.by_thread,
        &indexes.by_intent,
        &indexes.by_plan,
        &indexes.by_task,
        &indexes.by_run,
        &indexes.by_patchset,
        &indexes.by_event,
        &indexes.by_context,
    ]
    .into_iter()
    .any(|bucket| bucket.contains_key(object_id))
}

#[test]
fn publish_ai_export_test_rejects_redaction_policy_mismatch() {
    let mut object = run_object();
    object.redaction.rules_version = "older-rules".to_string();

    let err = build_ai_export_plan(AiExportRequest {
        repo_id: REPO_ID.to_string(),
        site_id: SITE_ID.to_string(),
        revision_oid: REVISION_OID.to_string(),
        ai_version_id: AI_VERSION_ID.to_string(),
        generated_at: timestamp(),
        ai_object_model_reference: "docs/agent/ai-object-model-reference.md".to_string(),
        redaction_mode: RedactionMode::Default,
        redaction_rules_version: RULES_VERSION.to_string(),
        associated_ids: AiBundleAssociatedIds::default(),
        objects: vec![object],
    })
    .expect_err("redaction rules must match the export policy");

    assert!(matches!(
        err,
        AiExportError::RedactionMismatch {
            object_type,
            actual_rules,
            expected_rules,
            ..
        } if object_type == "Run" && actual_rules == "older-rules" && expected_rules == RULES_VERSION
    ));
}

fn timestamp() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0)
        .single()
        .expect("test timestamp must be valid")
}

async fn setup_history_repo() -> (tempfile::TempDir, Arc<LocalStorage>, HistoryManager) {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = test::ChangeDirGuard::new(dir.path());
    test::setup_with_new_libra_in(dir.path()).await;

    let libra_dir = dir.path().join(".libra");
    let storage = Arc::new(LocalStorage::new(libra_dir.join("objects")));
    let db_conn = Arc::new(
        db::establish_connection(
            libra_dir
                .join("libra.db")
                .to_str()
                .expect("db path should be UTF-8"),
        )
        .await
        .expect("db should open"),
    );
    let history = HistoryManager::new(storage.clone(), libra_dir, db_conn);
    (dir, storage, history)
}

fn intent_object() -> PublishAiObject {
    object(
        "Intent",
        "intent-1",
        AiObjectLayer::Snapshot,
        serde_json::json!({ "intentId": "intent-1", "title": "Publish" }),
        vec![],
        vec![edge("Plan", "plan-1", "isPartOf", "Intent", "intent-1")],
    )
}

fn plan_object() -> PublishAiObject {
    object(
        "Plan",
        "plan-1",
        AiObjectLayer::Snapshot,
        serde_json::json!({ "planId": "plan-1", "intentId": "intent-1" }),
        vec![],
        vec![],
    )
}

fn run_object() -> PublishAiObject {
    object(
        "Run",
        "run-1",
        AiObjectLayer::Snapshot,
        serde_json::json!({ "runId": "run-1", "planId": "plan-1" }),
        vec![
            "payload.providerRawResponse".to_string(),
            "payload.absoluteWorkspacePath".to_string(),
        ],
        vec![edge("Run", "run-1", "appliesTo", "Plan", "plan-1")],
    )
}

fn object(
    object_type: &str,
    object_id: &str,
    layer: AiObjectLayer,
    payload: serde_json::Value,
    removed_fields: Vec<String>,
    relationships: Vec<AiObjectRelationship>,
) -> PublishAiObject {
    PublishAiObject {
        schema_version: PUBLISH_SCHEMA_VERSION,
        site_id: SITE_ID.to_string(),
        revision_oid: REVISION_OID.to_string(),
        object_type: object_type.to_string(),
        object_id: object_id.to_string(),
        layer,
        source_refs: vec![format!("test/{object_id}")],
        relationships,
        payload,
        redaction: AiObjectRedaction {
            mode: RedactionMode::Default,
            rules_version: RULES_VERSION.to_string(),
        },
        removed_fields,
    }
}

fn edge(
    from_object_type: &str,
    from_object_id: &str,
    kind: &str,
    to_object_type: &str,
    to_object_id: &str,
) -> AiObjectRelationship {
    AiObjectRelationship {
        kind: kind.to_string(),
        from_object_type: from_object_type.to_string(),
        from_object_id: from_object_id.to_string(),
        to_object_type: to_object_type.to_string(),
        to_object_id: to_object_id.to_string(),
    }
}
