//! `libra agent doctor [--repair]` — capture-store diagnostics and repair.
//!
//! Detection surfaces hook installation state, stuck active sessions, orphan
//! checkpoints, and — AG-20 (plan.md Task A5, `agent.md` doctor repair
//! matrix) — the three checkpoint-store inconsistency classes:
//!
//! 1. **`stale_catalog_row` / `missing_objects`** — an `agent_checkpoint`
//!    row whose `traces_commit` / `tree_oid` / `metadata_blob_oid` disagree
//!    with the object store. When the checkpoint is still reachable from
//!    `refs/libra/traces`, the row is rebuilt from the ref (idempotent
//!    UPDATE). When objects are genuinely missing, the finding is reported
//!    as `missing_objects` and requires manual action — doctor never
//!    destroys or fabricates data. For E4 (non-legacy) checkpoints the
//!    existence check covers the FULL six-entry tree, not just the row
//!    columns: `manifest.json`, `events/lifecycle.jsonl`,
//!    `transcript/<agent_kind>.jsonl` (including E5 `.jsonl.%03d` chunks),
//!    `redaction_report.json`, `content_hash.txt`, the intermediate trees,
//!    plus every blob the manifest declares — so a checkpoint whose
//!    sidecar is gone is never reported healthy while `show`/export cannot
//!    reconstruct it. A missing `manifest.json` blob is itself a
//!    `missing_objects` finding, and the tree-entry enumeration remains
//!    the primary probe so one missing manifest never hides other missing
//!    sidecars.
//! 2. **`missing_catalog_row`** — a checkpoint reachable from
//!    `refs/libra/traces` with no `agent_checkpoint` row (crash window B:
//!    ref CAS succeeded, catalog INSERT did not). Repair re-INSERTs the row
//!    via the same probe-first idempotent path the writer uses
//!    (`runtime::insert_agent_checkpoint_row_idempotent`), reconstructing
//!    the columns from the commit's `metadata.json` (both the AG-20 v2 and
//!    the legacy v1 metadata shapes parse) plus the `Libra-*` commit
//!    trailers. Checkpoints named by a LIVE traces in-flight marker are
//!    writers mid-flight, not inconsistencies, and are skipped.
//! 3. **`missing_object_index`** — a checkpoint object with no
//!    `object_index` row, i.e. invisible to `libra cloud sync`. Covers the
//!    checkpoint's full writer-enqueued set: the traces commit plus every
//!    E4 object the class-1 sweep verified, with the writer's o_type tags
//!    (trees as `tree`, transcript blobs/chunks as `agent_transcript`,
//!    JSON/text sidecars as `blob` — mirroring the
//!    `history.rs::append_checkpoint_commit` / `splice_checkpoint_tree`
//!    enqueue calls). `o_size` comes from the manifest-declared `byte_len`
//!    wherever the manifest declares one — transcript payloads are never
//!    read; only trees, the commit, and the manifest blob itself (all
//!    small) are sized by reading. Repair inserts rows directly
//!    (idempotent existence-checked INSERT mirroring
//!    `client_storage::update_object_index_once` semantics — doctor is a
//!    foreground command, so it does not go through the background queue).
//!    Only rows with an UNRECOVERABLE class-1 finding fall back to class-1
//!    reporting; an auto-repairable stale row still gets its (ref-side)
//!    class-3 check and repair in the same `--repair` run.
//!
//! **Legacy-v1 exemption**: checkpoints whose tree lacks `manifest.json`
//! (pre-AG-20 layout, `metadata.json` + `transcript/<provider>` only — see
//! `tests/fixtures/agent_checkpoints/v1_claude_code/`) are classified
//! `legacy-v1`, counted in `legacy_v1_checkpoints`, and NEVER included in
//! the three classes or touched by `--repair`.
//!
//! **Orphan rule fidelity** (`agent.md` write-sequence section):
//! session-without-checkpoint is a LEGAL intermediate state and is never
//! flagged; only checkpoint-without-session counts as an orphan.
//!
//! **Gemini**: existing `agent_session` / `agent_checkpoint` rows with
//! `agent_kind = 'gemini'` are legal read-only data and are never flagged.
//! Leftover gemini hook *configuration*, however, gets an actionable hint
//! pointing at the uninstall-only channel (`libra agent remove gemini`).
//!
//! **Observability** (`agent.md` §6): with `--repair`, one
//! `agent.doctor.repair` span is emitted per repair attempt (including
//! attempts that end `manual_required`) carrying `inconsistency_type`,
//! `repaired`, `manual_required`. Raw transcript bytes never reach the
//! span sink — doctor is metadata-first and never reads transcript blobs.
//! Detection-only runs emit no repair spans (nothing was attempted).
//!
//! Without `--repair` the command is strictly read-only; findings report
//! what `--repair` would do (`repaired` stays `false`, `manual_required`
//! is already accurate). All repairs are idempotent: a second run finds a
//! consistent store and does nothing.

use std::{
    collections::{BTreeSet, HashMap},
    str::FromStr,
    sync::Arc,
};

use chrono::Utc;
use git_internal::{
    hash::ObjectHash,
    internal::object::{
        ObjectTrait,
        commit::Commit,
        tree::{Tree, TreeItemMode},
    },
};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use serde::Serialize;

use super::DoctorArgs;
use crate::{
    internal::{
        ai::{
            history::{self, HistoryManager},
            hooks::{
                providers::{claude_provider, gemini_provider},
                runtime::{
                    AgentCheckpointRow, SubagentCheckpointRow,
                    insert_agent_checkpoint_row_idempotent,
                    insert_subagent_checkpoint_row_idempotent,
                },
            },
            observed_agents::{AgentStability, PREVIEW_SPECS, STABLE_PROMOTED_SPECS},
        },
        branch::TRACES_BRANCH,
        config::ConfigKv,
        db::get_db_conn_instance,
    },
    utils::{
        client_storage::ClientStorage,
        error::{CliError, CliResult},
        output::{OutputConfig, emit_json_data},
        util,
    },
};

/// Upper bound on the first-parent traces walk — prevents a corrupt
/// (cyclic) chain from hanging doctor. Hitting the cap truncates the walk
/// with a note; truncation only ever *under*-detects (fail-safe direction).
const MAX_TRACES_WALK_COMMITS: usize = 100_000;

/// Stable `inconsistency_type` values (also the span field values).
const CLASS_MISSING_OBJECTS: &str = "missing_objects";
const CLASS_STALE_CATALOG_ROW: &str = "stale_catalog_row";
const CLASS_MISSING_CATALOG_ROW: &str = "missing_catalog_row";
const CLASS_MISSING_OBJECT_INDEX: &str = "missing_object_index";
/// A0-06: a run manifest's `findings_oid` points at a blob missing from the
/// object store.
const CLASS_MISSING_FINDINGS_OBJECT: &str = "missing_findings_object";
/// A0-06: a run's findings blob exists but has no (or a drifted)
/// `object_index` row (invisible to cloud sync / retention).
const CLASS_MISSING_FINDINGS_OBJECT_INDEX: &str = "missing_findings_object_index";

#[derive(Debug, Serialize)]
struct ProviderHookStatus {
    name: &'static str,
    /// Adapter stability tier — `Stable` adapters carry a real
    /// `HookProvider` and report installation status; `Preview` ones
    /// (Phase 3.1) surface as "not yet installable".
    tier: AgentStability,
    installed: Option<bool>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct DoctorReport {
    schema_present: bool,
    active_sessions: i64,
    stopped_sessions: i64,
    orphan_checkpoints: i64,
    provider_hooks: Vec<ProviderHookStatus>,
    /// AG-20: leftover gemini hook configuration detected (uninstall-only
    /// channel — see the `libra agent remove gemini` hint). Captured gemini
    /// session/checkpoint *rows* are legal read-only data, never flagged.
    gemini_hooks_remnant: bool,
    /// AG-20 checkpoint-store scan (three-class detection + repair).
    checkpoint_store: CheckpointStoreReport,
    /// A0-06 review/investigate findings-object scan (detection + repair).
    findings_store: FindingsStoreReport,
}

#[derive(Debug, Serialize)]
struct FindingsStoreReport {
    scanned: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    /// Runs whose manifest carries a non-null `findings_oid`.
    runs_with_findings: usize,
    repair_applied: bool,
    repaired: usize,
    manual_required: usize,
    findings: Vec<FindingsObjectFinding>,
}

#[derive(Debug, Serialize)]
struct FindingsObjectFinding {
    /// `missing_findings_object` or `missing_findings_object_index`.
    inconsistency_type: String,
    run_id: String,
    /// Diagnosis — OIDs/reasons only, never findings content.
    detail: String,
    repaired: bool,
    manual_required: bool,
}

#[derive(Debug, Serialize)]
struct CheckpointStoreReport {
    /// Whether the checkpoint-store scan ran at all (requires the agent
    /// schema and a resolvable `.libra` directory).
    scanned: bool,
    /// Degradation notes (walk truncation, unreadable trees, …). The scan
    /// fails soft: unreadable pieces under-detect rather than erroring out.
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    catalog_rows: i64,
    ref_reachable_checkpoints: usize,
    /// Pre-AG-20 layout checkpoints (no `manifest.json`). Exempt from the
    /// three inconsistency classes and from `--repair` by contract.
    legacy_v1_checkpoints: usize,
    /// LIVE traces-writer in-flight markers (window A/B guards). Commits
    /// they name are writers mid-flight and are excluded from class 2.
    live_inflight_markers: usize,
    /// Whether `--repair` was requested for this run.
    repair_applied: bool,
    repaired: usize,
    manual_required: usize,
    findings: Vec<CheckpointFinding>,
}

#[derive(Debug, Serialize)]
struct CheckpointFinding {
    /// One of `stale_catalog_row`, `missing_objects`,
    /// `missing_catalog_row`, `missing_object_index`.
    inconsistency_type: String,
    checkpoint_id: String,
    /// Human-readable diagnosis — OIDs and reasons only, never transcript
    /// or metadata content.
    detail: String,
    repaired: bool,
    manual_required: bool,
}

/// What `--repair` would do for one finding. Built at detection time so
/// `manual_required` is accurate even without `--repair`.
#[derive(Debug)]
enum RepairPlan {
    /// Class 2: probe-first idempotent catalog INSERT.
    InsertCatalogRow {
        checkpoint_id: String,
        session_id: String,
        parent_commit: Option<String>,
        tree_oid: String,
        metadata_blob_oid: String,
        traces_commit: String,
        created_at: i64,
    },
    /// Class 2, subagent scope (A0-02): probe-first idempotent catalog
    /// INSERT rebuilding a `scope='subagent'` row with its linkage columns.
    InsertSubagentCatalogRow {
        checkpoint_id: String,
        session_id: String,
        parent_commit: Option<String>,
        parent_checkpoint_id: Option<String>,
        subagent_session_id: Option<String>,
        tool_use_id: Option<String>,
        description: Option<String>,
        tree_oid: String,
        metadata_blob_oid: String,
        traces_commit: String,
        created_at: i64,
    },
    /// Class 1 (recoverable): rebuild the row's OID columns from the ref.
    UpdateCatalogRow {
        checkpoint_id: String,
        tree_oid: String,
        metadata_blob_oid: String,
        traces_commit: String,
    },
    /// Class 3: idempotent `object_index` inserts, `(oid, o_type, o_size)`.
    InsertObjectIndex {
        entries: Vec<(String, String, i64)>,
        /// Rows that exist but drifted from writer semantics
        /// (`(o_id, expected_o_type, expected_o_size)`); repaired via
        /// in-place UPDATE instead of insert.
        updates: Vec<(String, String, i64)>,
    },
    /// No automatic action is safe; a human must restore objects/rows.
    Manual,
}

pub async fn execute_safe(args: DoctorArgs, output: &OutputConfig) -> CliResult<()> {
    let conn = get_db_conn_instance().await;
    let schema_present = table_exists(&conn, "agent_session").await?
        && table_exists(&conn, "agent_checkpoint").await?;

    let (active_sessions, stopped_sessions, orphan_checkpoints) = if schema_present {
        let active = scalar_count(
            &conn,
            "SELECT COUNT(*) AS n FROM agent_session WHERE state = 'active'",
        )
        .await?;
        let stopped = scalar_count(
            &conn,
            "SELECT COUNT(*) AS n FROM agent_session WHERE state = 'stopped'",
        )
        .await?;
        // Orphan = checkpoint rows whose session_id no longer joins (would
        // imply CASCADE failed or the row was hand-written). Should be 0
        // under normal operation; surfacing >0 is a real diagnostic.
        //
        // Direction matters (agent.md orphan rules): ONLY
        // checkpoint-without-session is illegal. The reverse —
        // session-without-checkpoint — is a legal intermediate state
        // (active session before its first TurnEnd/Stop) and must never
        // be flagged, so no symmetric query exists here.
        let orphans = scalar_count(
            &conn,
            "SELECT COUNT(*) AS n FROM agent_checkpoint cp \
             LEFT JOIN agent_session s ON s.session_id = cp.session_id \
             WHERE s.session_id IS NULL",
        )
        .await?;
        (active, stopped, orphans)
    } else {
        (0, 0, 0)
    };

    // Hook installation status across the v1 adapter matrix.
    // - claude-code and gemini carry dedicated HookProvider impls and
    //   report real install status.
    // - Stable-promoted adapters probe through their spec's AG-19
    //   `hooks` provider when they have one (codex, opencode — the A6.5
    //   smoke requires doctor to see all three first-batch chains);
    //   specs without an installable HookProvider (Cursor, Copilot,
    //   FactoryAi) stay `installed: None`.
    // - Any future preview adapters (PREVIEW_SPECS empty after Phase
    //   4.4) would surface here too.
    let mut provider_hooks = vec![
        check_provider(
            "claude-code",
            AgentStability::Stable,
            Some(claude_provider()),
        ),
        check_provider("gemini", AgentStability::Stable, Some(gemini_provider())),
    ];
    for spec in STABLE_PROMOTED_SPECS {
        provider_hooks.push(check_provider(
            spec.provider_name,
            AgentStability::Stable,
            spec.hooks,
        ));
    }
    for spec in PREVIEW_SPECS {
        provider_hooks.push(check_provider(
            spec.provider_name,
            AgentStability::Preview,
            None,
        ));
    }

    // AG-17/AG-20: gemini is uninstall-only. Fully-installed leftover hook
    // config means an old install was never removed — actionable hint.
    let gemini_hooks_remnant = provider_hooks
        .iter()
        .any(|ph| ph.name == "gemini" && ph.installed == Some(true));

    let checkpoint_store = scan_checkpoint_store(&conn, schema_present, args.repair).await?;
    let findings_store = scan_agent_findings(&conn, schema_present, args.repair).await?;

    emit_report(
        &DoctorReport {
            schema_present,
            active_sessions,
            stopped_sessions,
            orphan_checkpoints,
            provider_hooks,
            gemini_hooks_remnant,
            checkpoint_store,
            findings_store,
        },
        output,
    )
}

// ---------------------------------------------------------------------------
// AG-20 checkpoint-store scan (three classes + legacy-v1)
// ---------------------------------------------------------------------------

/// One `agent_checkpoint` row as loaded for the scan (only the columns the
/// three classes actually compare — doctor is metadata-first and never
/// loads transcript blobs).
#[derive(Debug, Clone)]
struct CatalogRow {
    checkpoint_id: String,
    tree_oid: String,
    metadata_blob_oid: String,
    traces_commit: String,
}

/// One ref-reachable checkpoint, attributed to the first-parent commit
/// that introduced it (the commit whose parent tree lacks the id).
#[derive(Debug, Clone)]
struct RefCheckpoint {
    /// The introducing commit on `refs/libra/traces` (what
    /// `agent_checkpoint.traces_commit` must equal).
    commit: String,
    /// That commit's root tree (what `agent_checkpoint.tree_oid` must equal).
    root_tree: String,
    /// `checkpoint/<p>/<rest>/metadata.json` blob OID, when the inner tree
    /// was readable and carries the entry.
    metadata_blob: Option<String>,
    /// `manifest.json` present in the inner tree ⇒ AG-20 (E4-libra) layout.
    manifest_present: bool,
    /// `metadata.json` present in the inner tree.
    metadata_present: bool,
    /// Whether the inner checkpoint tree could be read at all.
    inner_readable: bool,
    /// `Libra-Parent-Commit` trailer from the introducing commit message.
    parent_commit_trailer: Option<String>,
    /// `Libra-Scope` trailer from the introducing commit message.
    scope_trailer: Option<String>,
}

impl RefCheckpoint {
    /// Legacy-v1 layout: readable inner tree with `metadata.json` but no
    /// `manifest.json` (pre-AG-20 writer output).
    fn is_legacy_v1(&self) -> bool {
        self.inner_readable && self.metadata_present && !self.manifest_present
    }
}

/// Fields doctor needs from a checkpoint's `metadata.json`. Both external
/// schema shapes parse into this: v1 (pre-AG-20) and v2 (AG-20, adds
/// `model`) carry `session_id`, `created_at` and `scope`; unknown fields
/// are ignored.
#[derive(Debug, serde::Deserialize)]
struct CheckpointMetadataProbe {
    session_id: String,
    created_at: i64,
    #[serde(default)]
    scope: Option<String>,
    // A0-02: subagent-scope checkpoints carry these linkage fields flat in
    // `metadata.json` so a class-2 (crash-window-B) repair can rebuild a
    // first-class `scope='subagent'` catalog row instead of leaving it manual.
    #[serde(default)]
    parent_checkpoint_id: Option<String>,
    #[serde(default)]
    subagent_session_id: Option<String>,
    #[serde(default)]
    tool_use_id: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

/// `Libra-*` trailers from a traces checkpoint commit message.
#[derive(Debug, Default)]
struct LibraTrailers {
    parent_commit: Option<String>,
    scope: Option<String>,
}

fn parse_libra_trailers(message: &str) -> LibraTrailers {
    let mut trailers = LibraTrailers::default();
    for line in message.lines() {
        if let Some(value) = line.strip_prefix("Libra-Parent-Commit: ") {
            trailers.parent_commit = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("Libra-Scope: ") {
            trailers.scope = Some(value.trim().to_string());
        }
    }
    trailers
}

/// Thin typed reader over the repo object store. Uses [`ClientStorage`] so
/// packed / tiered objects resolve the same way the rest of the CLI sees
/// them (doctor must not report an object "missing" just because it was
/// synced to the durable tier).
struct ObjectReader {
    storage: Arc<ClientStorage>,
}

impl ObjectReader {
    fn exists_str(&self, oid: &str) -> bool {
        ObjectHash::from_str(oid)
            .map(|hash| self.storage.exist(&hash))
            .unwrap_or(false)
    }

    fn read_raw(&self, oid: &ObjectHash) -> anyhow::Result<Vec<u8>> {
        self.storage
            .get(oid)
            .map_err(|e| anyhow::anyhow!("failed to read object {oid}: {e}"))
    }

    fn read_commit(&self, oid: &ObjectHash) -> anyhow::Result<Commit> {
        let data = self.read_raw(oid)?;
        Commit::from_bytes(&data, *oid)
            .map_err(|e| anyhow::anyhow!("failed to parse commit {oid}: {e}"))
    }

    fn read_tree(&self, oid: &ObjectHash) -> anyhow::Result<Tree> {
        let data = self.read_raw(oid)?;
        Tree::from_bytes(&data, *oid)
            .map_err(|e| anyhow::anyhow!("failed to parse tree {oid}: {e}"))
    }
}

/// One object in an E4 checkpoint's reachability set, tagged with the
/// o_type the writer's `object_index` enqueue path stamps on it
/// (`history.rs::append_checkpoint_commit` / `splice_checkpoint_tree`):
/// every tree (root, `checkpoint/`, prefix, inner, `events/`,
/// `transcript/`) is `tree`, transcript files (including E5 chunks) are
/// `agent_transcript`, and the JSON/text sidecars plus lifecycle events
/// are `blob`.
#[derive(Debug)]
struct E4Object {
    /// Human-readable role/path (e.g. `manifest.json`,
    /// `transcript/claude_code.jsonl.001`) for findings — never content.
    path: String,
    oid: String,
    o_type: &'static str,
    /// Manifest-declared payload byte length, when the object's role
    /// declares one (`byte_len` on single entries, per-part on E5 chunk
    /// parts). The writer enqueues exactly the payload length into
    /// `object_index` and records that same number in the manifest, so
    /// class 3 can size transcript blobs WITHOUT reading their payloads
    /// (metadata-first / no-transcript-read contract). `None` — trees,
    /// the manifest blob itself, or a corrupt manifest — falls back to a
    /// payload read in class 3, which for a well-formed E4 layout only
    /// ever touches small tree/JSON objects, never a transcript.
    size: Option<i64>,
}

/// Result of sweeping one E4 checkpoint's full object set (class 1
/// detection input; the `present` list doubles as the class-3 target set).
#[derive(Debug, Default)]
struct E4Sweep {
    /// Objects verified present (existence via read for trees, store
    /// probe for blobs), with writer-matching o_type.
    present: Vec<E4Object>,
    /// `"path oid"` descriptors of missing/unreadable objects.
    missing: Vec<String>,
}

impl E4Sweep {
    /// Record one object, deduplicating by OID (the manifest re-declares
    /// blobs the tree enumeration already visited).
    fn record(
        &mut self,
        seen: &mut BTreeSet<String>,
        path: &str,
        oid: &str,
        o_type: &'static str,
        size: Option<i64>,
        exists: bool,
    ) {
        if !seen.insert(oid.to_string()) {
            return;
        }
        if exists {
            self.present.push(E4Object {
                path: path.to_string(),
                oid: oid.to_string(),
                o_type,
                size,
            });
        } else {
            self.missing.push(format!("{path} {oid}"));
        }
    }
}

/// Sweep every object belonging to one E4 checkpoint: walk
/// `root → checkpoint → <prefix> → <rest>`, enumerate the inner tree
/// (top-level sidecar blobs plus the `events/` and `transcript/`
/// subtrees), then cross-check the manifest's declared entries. The tree
/// enumeration is the primary probe — a missing `manifest.json` is
/// recorded like any other missing sidecar and never hides the rest; the
/// manifest pass only adds declared blobs the tree did not already cover
/// (defence in depth, and it contributes role/path naming for chunked
/// transcripts).
fn sweep_e4_checkpoint_objects(
    reader: &ObjectReader,
    root_tree_oid: &str,
    checkpoint_id: &str,
) -> E4Sweep {
    let mut sweep = E4Sweep::default();
    let mut seen: BTreeSet<String> = BTreeSet::new();

    let (Some(prefix), Some(rest)) = (checkpoint_id.get(..2), checkpoint_id.get(2..)) else {
        sweep.missing.push(format!(
            "checkpoint id '{checkpoint_id}' too short to resolve its tree path"
        ));
        return sweep;
    };

    // Chain walk: each level is both recorded (writer enqueues every one
    // of these trees as o_type "tree") and descended into. A missing or
    // unreadable tree ends the walk — everything below it is unreachable
    // and the finding already names the break point.
    let labels = [
        "tree_oid (root tree)".to_string(),
        "checkpoint (tree)".to_string(),
        format!("checkpoint/{prefix} (tree)"),
        format!("checkpoint/{prefix}/{rest} (tree)"),
    ];
    let mut oid = root_tree_oid.to_string();
    let mut inner_tree: Option<Tree> = None;
    for (depth, label) in labels.iter().enumerate() {
        let tree = match ObjectHash::from_str(&oid) {
            Ok(hash) => match reader.read_tree(&hash) {
                Ok(tree) => {
                    sweep.record(&mut seen, label, &oid, "tree", None, true);
                    tree
                }
                Err(_) => {
                    sweep.record(&mut seen, label, &oid, "tree", None, false);
                    return sweep;
                }
            },
            Err(_) => {
                sweep.record(&mut seen, label, &oid, "tree", None, false);
                return sweep;
            }
        };
        if depth == labels.len() - 1 {
            inner_tree = Some(tree);
            break;
        }
        let next_name = match depth {
            0 => "checkpoint",
            1 => prefix,
            _ => rest,
        };
        let Some(entry) = tree
            .tree_items
            .iter()
            .find(|item| item.name == next_name && item.mode == TreeItemMode::Tree)
        else {
            sweep.missing.push(format!(
                "{} (tree entry absent under {label})",
                labels[depth + 1]
            ));
            return sweep;
        };
        oid = entry.id.to_string();
    }
    let Some(inner) = inner_tree else {
        return sweep;
    };

    // Manifest first: read + parse it (one small JSON blob) so the tree
    // enumeration below can attach the manifest-declared `byte_len` to
    // every declared blob — class 3 must never read transcript payloads
    // just to size them. A missing/corrupt manifest degrades to
    // size-by-read for the (small) non-transcript objects and is itself
    // reported missing by the tree enumeration.
    let mut declared: Vec<(String, String, Option<i64>)> = Vec::new();
    let mut declared_sizes: HashMap<String, i64> = HashMap::new();
    let manifest_item = inner
        .tree_items
        .iter()
        .find(|item| item.name == "manifest.json" && item.mode != TreeItemMode::Tree);
    if let Some(manifest_item) = manifest_item
        && let Ok(bytes) = reader.read_raw(&manifest_item.id)
        && let Ok(manifest) = serde_json::from_slice::<serde_json::Value>(&bytes)
    {
        declared = manifest_declared_blobs(&manifest);
        for (_, declared_oid, byte_len) in &declared {
            if let Some(byte_len) = byte_len {
                declared_sizes.insert(declared_oid.clone(), *byte_len);
            }
        }
    }

    // Inner tree enumeration (the primary probe): sidecar blobs at the
    // top level, one level of subtrees below (`events/`, `transcript/`;
    // future additive dirs are treated generically). Transcript blobs —
    // including E5 chunk parts — carry the writer's distinguished
    // "agent_transcript" tag.
    for item in &inner.tree_items {
        let item_oid = item.id.to_string();
        if item.mode == TreeItemMode::Tree {
            let label = format!("{} (tree)", item.name);
            match reader.read_tree(&item.id) {
                Ok(subtree) => {
                    sweep.record(&mut seen, &label, &item_oid, "tree", None, true);
                    let leaf_type = if item.name == "transcript" {
                        "agent_transcript"
                    } else {
                        "blob"
                    };
                    for leaf in &subtree.tree_items {
                        let leaf_oid = leaf.id.to_string();
                        let leaf_path = format!("{}/{}", item.name, leaf.name);
                        let o_type = if leaf.mode == TreeItemMode::Tree {
                            "tree"
                        } else {
                            leaf_type
                        };
                        let size = declared_sizes.get(&leaf_oid).copied();
                        sweep.record(
                            &mut seen,
                            &leaf_path,
                            &leaf_oid,
                            o_type,
                            size,
                            reader.exists_str(&leaf_oid),
                        );
                    }
                }
                Err(_) => sweep.record(&mut seen, &label, &item_oid, "tree", None, false),
            }
        } else {
            let size = declared_sizes.get(&item_oid).copied();
            sweep.record(
                &mut seen,
                &item.name,
                &item_oid,
                "blob",
                size,
                reader.exists_str(&item_oid),
            );
        }
    }

    // Manifest cross-check: verify every declared entry/chunk OID. OIDs
    // already visited above dedupe out; this only adds blobs the manifest
    // names beyond the tree (corruption defence) with role-path labels.
    for (path, declared_oid, byte_len) in declared {
        let o_type = if path.starts_with("transcript/") {
            "agent_transcript"
        } else {
            "blob"
        };
        let label = format!("{path} (manifest-declared)");
        sweep.record(
            &mut seen,
            &label,
            &declared_oid,
            o_type,
            byte_len,
            reader.exists_str(&declared_oid),
        );
    }

    sweep
}

/// Extract every `(path, oid, byte_len)` triple a checkpoint manifest
/// declares: single-blob entries carry `path` + `oid` + `byte_len`; the
/// chunked transcript shape (E5) declares `parts: [{path, oid, byte_len}]`
/// instead. The `byte_len` is the payload length the writer enqueued into
/// `object_index`, letting class 3 size transcript blobs without reading
/// them (a missing `byte_len` yields `None` and a read-based fallback for
/// that object only).
fn manifest_declared_blobs(manifest: &serde_json::Value) -> Vec<(String, String, Option<i64>)> {
    let mut out = Vec::new();
    let Some(entries) = manifest.get("entries").and_then(|v| v.as_object()) else {
        return out;
    };
    for (role, entry) in entries {
        let entry_path = entry
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(role)
            .to_string();
        if let Some(oid) = entry.get("oid").and_then(|v| v.as_str()) {
            let byte_len = entry.get("byte_len").and_then(|v| v.as_i64());
            out.push((entry_path.clone(), oid.to_string(), byte_len));
        }
        if let Some(parts) = entry.get("parts").and_then(|v| v.as_array()) {
            for part in parts {
                if let Some(oid) = part.get("oid").and_then(|v| v.as_str()) {
                    let part_path = part
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or(entry_path.as_str())
                        .to_string();
                    let byte_len = part.get("byte_len").and_then(|v| v.as_i64());
                    out.push((part_path, oid.to_string(), byte_len));
                }
            }
        }
    }
    out
}

/// Run the AG-20 three-class scan (and, with `repair`, the repairs).
///
/// The scan itself fails soft: object-store degradations become `note`
/// entries and under-detect instead of erroring, so `doctor` stays usable
/// on a damaged store. Database errors still propagate — they mean the
/// diagnosis itself cannot be trusted.
async fn scan_checkpoint_store(
    conn: &DatabaseConnection,
    schema_present: bool,
    repair: bool,
) -> CliResult<CheckpointStoreReport> {
    let mut report = CheckpointStoreReport {
        scanned: false,
        note: None,
        catalog_rows: 0,
        ref_reachable_checkpoints: 0,
        legacy_v1_checkpoints: 0,
        live_inflight_markers: 0,
        repair_applied: repair,
        repaired: 0,
        manual_required: 0,
        findings: Vec::new(),
    };
    if !schema_present {
        report.note = Some("agent schema not present (run `libra init`?)".to_string());
        return Ok(report);
    }
    let repo_path = match util::try_get_storage_path(None) {
        Ok(path) => path,
        Err(err) => {
            report.note = Some(format!("failed to locate .libra directory: {err}"));
            return Ok(report);
        }
    };
    report.scanned = true;
    let mut notes: Vec<String> = Vec::new();

    let reader = ObjectReader {
        storage: Arc::new(ClientStorage::init(repo_path.join("objects"))),
    };
    let history = HistoryManager::new_with_ref(
        reader.storage.clone(),
        repo_path.clone(),
        Arc::new(conn.clone()),
        TRACES_BRANCH,
    );

    // Live in-flight markers: commits/attempts named by a live marker are
    // writers mid-flight (window B), not inconsistencies.
    let now_ms = Utc::now().timestamp_millis();
    let markers = match history::list_live_traces_inflight_markers(conn, now_ms).await {
        Ok(markers) => markers,
        Err(err) => {
            notes.push(format!("in-flight marker listing unavailable: {err:#}"));
            Vec::new()
        }
    };
    report.live_inflight_markers = markers.len();
    let mut inflight_ids: BTreeSet<String> = BTreeSet::new();
    let mut inflight_commits: BTreeSet<String> = BTreeSet::new();
    for marker in &markers {
        inflight_ids.insert(marker.attempt_id.clone());
        if let Some(commit) = &marker.commit {
            inflight_commits.insert(commit.clone());
        }
    }

    // Walk refs/libra/traces first-parent and attribute each checkpoint to
    // its introducing commit.
    let head = match history.resolve_history_head().await {
        Ok(head) => head,
        Err(err) => {
            notes.push(format!("traces ref unresolvable: {err:#}"));
            None
        }
    };
    let ref_map = walk_traces_checkpoints(&reader, head, &mut notes);
    report.ref_reachable_checkpoints = ref_map.len();

    let rows = load_catalog_rows(conn).await?;
    report.catalog_rows = rows.len() as i64;
    let row_ids: BTreeSet<String> = rows.iter().map(|r| r.checkpoint_id.clone()).collect();

    // Legacy-v1 classification first — legacy checkpoints are exempt from
    // all three classes. Ref-reachable checkpoints classify via their
    // introducing commit's tree; catalog-only rows via their own tree_oid.
    let mut legacy_ids: BTreeSet<String> = BTreeSet::new();
    for (id, rc) in &ref_map {
        if rc.is_legacy_v1() {
            legacy_ids.insert(id.clone());
        }
    }
    for row in &rows {
        if !ref_map.contains_key(&row.checkpoint_id)
            && row_layout_is_legacy_v1(&reader, &row.tree_oid, &row.checkpoint_id)
        {
            legacy_ids.insert(row.checkpoint_id.clone());
        }
    }
    report.legacy_v1_checkpoints = legacy_ids.len();

    let mut findings: Vec<CheckpointFinding> = Vec::new();
    let mut plans: Vec<RepairPlan> = Vec::new();
    // Only rows with an UNRECOVERABLE class-1 finding (`missing_objects`)
    // are excluded from class 3 ("unrecoverable falls back to class-1
    // reporting"). An auto-repairable `stale_catalog_row` must NOT
    // suppress class 3: its ref-side objects are intact, and both repairs
    // must land in the same `--repair` run (otherwise cloud-sync
    // visibility would stay broken until a second invocation).
    let mut class1_manual_ids: BTreeSet<String> = BTreeSet::new();

    // ---- Class 1: catalog rows vs object store / ref truth -------------
    // Per-checkpoint E4 sweeps are kept for class 3 (the `present` list is
    // exactly the writer-enqueued object set minus the commit).
    let mut e4_sweeps: HashMap<String, E4Sweep> = HashMap::new();
    for row in &rows {
        if legacy_ids.contains(&row.checkpoint_id) {
            continue;
        }
        let commit_ok = reader.exists_str(&row.traces_commit);
        let tree_ok = reader.exists_str(&row.tree_oid);
        let meta_ok = reader.exists_str(&row.metadata_blob_oid);
        let rc = ref_map.get(&row.checkpoint_id);

        // Full E4 sidecar sweep (six-entry tree + E5 chunks + manifest
        // declarations) — against ref truth when available, else the
        // row's own tree.
        let sweep = match rc {
            Some(rc) => sweep_e4_checkpoint_objects(&reader, &rc.root_tree, &row.checkpoint_id),
            None if tree_ok => {
                sweep_e4_checkpoint_objects(&reader, &row.tree_oid, &row.checkpoint_id)
            }
            None => E4Sweep::default(),
        };

        // Which objects count as "missing" depends on where truth lives:
        // - Ref-reachable rows: the ref-side sweep governs. It already
        //   covers the root tree, metadata.json, and every sidecar, and
        //   the introducing commit was readable during the walk. Corrupt
        //   row COLUMNS pointing at nonexistent OIDs are the stale-repair
        //   branch's business (the repair replaces exactly those values),
        //   not lost objects.
        // - Catalog-only rows: the row's columns are the only truth, so
        //   probe them directly and extend with the row-tree sweep
        //   (dropping sweep lines whose OID a column line already names).
        let missing_parts: Vec<String> = if rc.is_some() {
            sweep.missing.clone()
        } else {
            let mut parts: Vec<String> = Vec::new();
            if !commit_ok {
                parts.push(format!("traces_commit {}", row.traces_commit));
            }
            if !tree_ok {
                parts.push(format!("tree_oid {}", row.tree_oid));
            }
            if !meta_ok {
                parts.push(format!("metadata_blob_oid {}", row.metadata_blob_oid));
            }
            for part in &sweep.missing {
                let dup = (!commit_ok && part.contains(&row.traces_commit))
                    || (!tree_ok && part.contains(&row.tree_oid))
                    || (!meta_ok && part.contains(&row.metadata_blob_oid));
                if !dup {
                    parts.push(part.clone());
                }
            }
            parts
        };

        if let Some(rc) = rc {
            let expected_meta = rc.metadata_blob.as_deref();
            let stale = row.traces_commit != rc.commit
                || row.tree_oid != rc.root_tree
                || expected_meta.is_some_and(|meta| row.metadata_blob_oid != meta);
            // The ref-derived replacement values must themselves verify
            // before we call the row auto-repairable (the commit and root
            // tree were read during the walk; the metadata blob still
            // needs an existence probe).
            let replacement_ok = expected_meta.is_some_and(|meta| reader.exists_str(meta));
            if stale && replacement_ok {
                // Auto-repairable: deliberately NOT in class1_manual_ids —
                // the same run's class-3 pass still checks/repairs this
                // checkpoint's (ref-side) object_index rows.
                // INVARIANT: replacement_ok proved expected_meta is Some.
                let metadata_blob_oid = expected_meta.unwrap_or_default().to_string();
                findings.push(CheckpointFinding {
                    inconsistency_type: CLASS_STALE_CATALOG_ROW.to_string(),
                    checkpoint_id: row.checkpoint_id.clone(),
                    detail: format!(
                        "row OIDs disagree with refs/libra/traces (ref: commit {}, tree {}); \
                         rebuild from ref",
                        rc.commit, rc.root_tree
                    ),
                    repaired: false,
                    manual_required: false,
                });
                plans.push(RepairPlan::UpdateCatalogRow {
                    checkpoint_id: row.checkpoint_id.clone(),
                    tree_oid: rc.root_tree.clone(),
                    metadata_blob_oid,
                    traces_commit: rc.commit.clone(),
                });
            }
            // Missing objects are reported independently of staleness —
            // rebuilding row columns cannot resurrect a lost sidecar blob.
            if !missing_parts.is_empty() || (stale && !replacement_ok) {
                class1_manual_ids.insert(row.checkpoint_id.clone());
                findings.push(CheckpointFinding {
                    inconsistency_type: CLASS_MISSING_OBJECTS.to_string(),
                    checkpoint_id: row.checkpoint_id.clone(),
                    detail: missing_objects_detail(&missing_parts),
                    repaired: false,
                    manual_required: true,
                });
                plans.push(RepairPlan::Manual);
            }
        } else if !missing_parts.is_empty() {
            class1_manual_ids.insert(row.checkpoint_id.clone());
            findings.push(CheckpointFinding {
                inconsistency_type: CLASS_MISSING_OBJECTS.to_string(),
                checkpoint_id: row.checkpoint_id.clone(),
                detail: format!(
                    "{} (checkpoint is not reachable from refs/libra/traces, so it \
                     cannot be rebuilt automatically)",
                    missing_objects_detail(&missing_parts)
                ),
                repaired: false,
                manual_required: true,
            });
            plans.push(RepairPlan::Manual);
        }
        e4_sweeps.insert(row.checkpoint_id.clone(), sweep);
    }

    // ---- Class 2: ref-reachable checkpoints without a catalog row ------
    for (id, rc) in &ref_map {
        if legacy_ids.contains(id) || row_ids.contains(id) {
            continue;
        }
        // Writers mid-flight (window B) are not inconsistencies.
        if inflight_ids.contains(id) || inflight_commits.contains(&rc.commit) {
            continue;
        }
        // A row may exist under a different checkpoint_id for the same
        // commit (e.g. an earlier repair raced a crash retry) — the same
        // probe the writer uses keeps this idempotent.
        match history::agent_checkpoint_id_for_traces_commit(conn, &rc.commit).await {
            Ok(Some(_)) => continue,
            Ok(None) => {}
            Err(err) => {
                return Err(CliError::fatal(format!(
                    "doctor failed to probe agent_checkpoint by traces_commit: {err:#}"
                )));
            }
        }
        let (detail, plan) = build_class2_plan(conn, &reader, id, rc).await?;
        let manual = matches!(plan, RepairPlan::Manual);
        findings.push(CheckpointFinding {
            inconsistency_type: CLASS_MISSING_CATALOG_ROW.to_string(),
            checkpoint_id: id.clone(),
            detail,
            repaired: false,
            manual_required: manual,
        });
        plans.push(plan);
    }

    // ---- Class 3: checkpoint objects missing from object_index ---------
    let repo_id = resolve_repo_id(conn).await;
    for row in &rows {
        if legacy_ids.contains(&row.checkpoint_id) || class1_manual_ids.contains(&row.checkpoint_id)
        {
            continue;
        }
        // Full writer-enqueued object set: the traces commit plus every
        // object the E4 sweep verified (all trees, sidecar blobs, and
        // transcript chunks) with the writer's o_type tags. Truth is
        // ref-side when available: for a stale row (repaired in this same
        // run) the row columns are corrupt, so the commit target must be
        // the ref's introducing commit, and the sweep already ran on the
        // ref-side root.
        let commit_truth = ref_map
            .get(&row.checkpoint_id)
            .map(|rc| rc.commit.clone())
            .unwrap_or_else(|| row.traces_commit.clone());
        let mut targets: Vec<(String, &'static str, String, Option<i64>)> = vec![(
            commit_truth.clone(),
            "commit",
            format!("traces_commit {commit_truth}"),
            None,
        )];
        if let Some(sweep) = e4_sweeps.get(&row.checkpoint_id) {
            for object in &sweep.present {
                targets.push((
                    object.oid.clone(),
                    object.o_type,
                    format!("{} {}", object.path, object.oid),
                    object.size,
                ));
            }
        }
        let mut missing: Vec<(String, String, i64)> = Vec::new();
        let mut missing_names: Vec<String> = Vec::new();
        let mut drifted: Vec<(String, String, i64)> = Vec::new();
        let mut drifted_names: Vec<String> = Vec::new();
        let mut seen_oids: BTreeSet<String> = BTreeSet::new();
        for (oid, o_type, label, declared_size) in targets {
            if !seen_oids.insert(oid.clone()) {
                continue;
            }
            if let Some((existing_type, existing_size)) =
                object_index_row_shape(conn, &oid, &repo_id).await?
            {
                // A row that exists but drifted from the writer's
                // semantics (e.g. a transcript blob indexed as a generic
                // `blob`, or a wrong size) breaks cloud-sync classification
                // just like a missing row. Size is only verifiable when
                // the manifest declares it — never read payloads here.
                let type_drift = existing_type != o_type;
                let size_drift = declared_size.is_some_and(|d| d != existing_size);
                if type_drift || size_drift {
                    drifted.push((
                        oid.clone(),
                        o_type.to_string(),
                        declared_size.unwrap_or(existing_size),
                    ));
                    drifted_names.push(format!("{label} (was {existing_type}/{existing_size})"));
                }
                continue;
            }
            // o_size mirrors the writer's enqueue semantics: payload byte
            // length. Prefer the manifest-declared byte_len (identical to
            // what the writer enqueued) so transcript payloads are NEVER
            // read here (metadata-first contract); the read-based fallback
            // only fires for objects the manifest cannot declare — trees,
            // the commit, and the manifest blob itself, all small (a tree
            // / commit read returns exactly the payload the writer sized:
            // `write_tree_with_size` / `commit_data.len()`). Class 1
            // already verified these objects exist; a racing deletion
            // degrades to a note rather than a hard error.
            //
            // Transcript payloads are exempt from the fallback entirely:
            // when the manifest is unreadable/corrupt or omits byte_len,
            // reading the blob to size it would violate the no-transcript-
            // read contract, so the row is skipped with an actionable
            // note instead (re-running doctor after restoring the
            // manifest repairs it without ever loading the payload).
            let size = match declared_size {
                Some(size) => size,
                None if o_type == "agent_transcript" => {
                    notes.push(format!(
                        "object_index repair for checkpoint {} skipped {label}: the \
                         manifest declares no byte_len for this transcript object and \
                         doctor never reads transcript payloads; restore or repair \
                         manifest.json, then re-run 'libra agent doctor --repair'",
                        row.checkpoint_id
                    ));
                    continue;
                }
                None => {
                    match ObjectHash::from_str(&oid)
                        .map_err(|e| anyhow::anyhow!("invalid OID '{oid}': {e}"))
                        .and_then(|hash| reader.read_raw(&hash))
                    {
                        Ok(bytes) => bytes.len() as i64,
                        Err(err) => {
                            notes.push(format!(
                                "object_index repair for checkpoint {} skipped {label}: {err:#}",
                                row.checkpoint_id
                            ));
                            continue;
                        }
                    }
                }
            };
            missing.push((oid.clone(), o_type.to_string(), size));
            missing_names.push(label);
        }
        if missing.is_empty() && drifted.is_empty() {
            continue;
        }
        let mut detail_parts: Vec<String> = Vec::new();
        if !missing_names.is_empty() {
            detail_parts.push(format!(
                "object_index rows missing for {}",
                missing_names.join(", ")
            ));
        }
        if !drifted_names.is_empty() {
            detail_parts.push(format!(
                "object_index rows drifted from writer semantics for {}",
                drifted_names.join(", ")
            ));
        }
        findings.push(CheckpointFinding {
            inconsistency_type: CLASS_MISSING_OBJECT_INDEX.to_string(),
            checkpoint_id: row.checkpoint_id.clone(),
            detail: format!(
                "{} (objects would not reach `libra cloud sync` correctly)",
                detail_parts.join("; ")
            ),
            repaired: false,
            manual_required: false,
        });
        plans.push(RepairPlan::InsertObjectIndex {
            entries: missing,
            updates: drifted,
        });
    }

    // ---- Repair execution (idempotent; spans per attempt) ---------------
    if repair {
        for (finding, plan) in findings.iter_mut().zip(plans.iter()) {
            execute_repair(conn, finding, plan, &repo_id).await;
            emit_repair_span(finding);
        }
    }

    report.repaired = findings.iter().filter(|f| f.repaired).count();
    report.manual_required = findings.iter().filter(|f| f.manual_required).count();
    report.findings = findings;
    if !notes.is_empty() {
        report.note = Some(notes.join("; "));
    }
    Ok(report)
}

/// One `agent.doctor.repair` span per repair attempt (`agent.md` §6).
/// Required fields: `inconsistency_type`, `repaired`, `manual_required`.
/// Forbidden: raw transcript — only the checkpoint id rides along on the
/// inner event for correlation.
fn emit_repair_span(finding: &CheckpointFinding) {
    let span = tracing::info_span!(
        "agent.doctor.repair",
        inconsistency_type = %finding.inconsistency_type,
        repaired = finding.repaired,
        manual_required = finding.manual_required,
    );
    let _guard = span.enter();
    tracing::info!(
        checkpoint_id = %finding.checkpoint_id,
        "agent doctor repair attempt"
    );
}

/// Execute one repair plan, updating the finding in place. Repair failures
/// never abort the run — they annotate the finding and leave it
/// unrepaired so the operator sees exactly what happened.
async fn execute_repair(
    conn: &DatabaseConnection,
    finding: &mut CheckpointFinding,
    plan: &RepairPlan,
    repo_id: &str,
) {
    match plan {
        RepairPlan::Manual => {}
        RepairPlan::InsertCatalogRow {
            checkpoint_id,
            session_id,
            parent_commit,
            tree_oid,
            metadata_blob_oid,
            traces_commit,
            created_at,
        } => {
            let row = AgentCheckpointRow {
                checkpoint_id,
                session_id,
                parent_commit: parent_commit.as_deref(),
                tree_oid,
                metadata_blob_oid,
                traces_commit,
                created_at: *created_at,
            };
            match insert_agent_checkpoint_row_idempotent(conn, &row).await {
                // `false` means another writer (or an earlier repair)
                // already landed the row — the inconsistency is gone
                // either way.
                Ok(_) => finding.repaired = true,
                Err(err) => {
                    finding
                        .detail
                        .push_str(&format!("; repair failed: {err:#}"));
                }
            }
        }
        RepairPlan::InsertSubagentCatalogRow {
            checkpoint_id,
            session_id,
            parent_commit,
            parent_checkpoint_id,
            subagent_session_id,
            tool_use_id,
            description,
            tree_oid,
            metadata_blob_oid,
            traces_commit,
            created_at,
        } => {
            let row = SubagentCheckpointRow {
                checkpoint_id,
                session_id,
                parent_commit: parent_commit.as_deref(),
                parent_checkpoint_id: parent_checkpoint_id.as_deref(),
                subagent_session_id: subagent_session_id.as_deref(),
                tool_use_id: tool_use_id.as_deref(),
                description: description.as_deref(),
                tree_oid,
                metadata_blob_oid,
                traces_commit,
                created_at: *created_at,
            };
            match insert_subagent_checkpoint_row_idempotent(conn, &row).await {
                Ok(_) => finding.repaired = true,
                Err(err) => {
                    finding
                        .detail
                        .push_str(&format!("; repair failed: {err:#}"));
                }
            }
        }
        RepairPlan::UpdateCatalogRow {
            checkpoint_id,
            tree_oid,
            metadata_blob_oid,
            traces_commit,
        } => {
            let backend = conn.get_database_backend();
            let result = conn
                .execute(Statement::from_sql_and_values(
                    backend,
                    "UPDATE agent_checkpoint \
                     SET tree_oid = ?, metadata_blob_oid = ?, traces_commit = ? \
                     WHERE checkpoint_id = ?",
                    [
                        tree_oid.as_str().into(),
                        metadata_blob_oid.as_str().into(),
                        traces_commit.as_str().into(),
                        checkpoint_id.as_str().into(),
                    ],
                ))
                .await;
            match result {
                Ok(_) => finding.repaired = true,
                Err(err) => {
                    finding.detail.push_str(&format!("; repair failed: {err}"));
                }
            }
        }
        RepairPlan::InsertObjectIndex { entries, updates } => {
            let mut all_ok = true;
            for (oid, o_type, o_size) in entries {
                if let Err(err) = insert_object_index_row(conn, oid, o_type, *o_size, repo_id).await
                {
                    all_ok = false;
                    finding
                        .detail
                        .push_str(&format!("; repair failed for {oid}: {err}"));
                }
            }
            for (oid, o_type, o_size) in updates {
                if let Err(err) =
                    update_object_index_row_shape(conn, oid, o_type, *o_size, repo_id).await
                {
                    all_ok = false;
                    finding
                        .detail
                        .push_str(&format!("; drift repair failed for {oid}: {err}"));
                }
            }
            finding.repaired = all_ok;
        }
    }
}

/// Class-2 repair plan: reconstruct the catalog row from the introducing
/// commit's `metadata.json` (v1 and v2 shapes both parse) and `Libra-*`
/// trailers. Anything unparsable / unsatisfiable degrades to `Manual` with
/// the reason in the detail string (never transcript content).
async fn build_class2_plan(
    conn: &DatabaseConnection,
    reader: &ObjectReader,
    checkpoint_id: &str,
    rc: &RefCheckpoint,
) -> CliResult<(String, RepairPlan)> {
    let base = format!(
        "commit {} on refs/libra/traces has no agent_checkpoint row (crash window B)",
        rc.commit
    );
    let Some(metadata_blob) = rc.metadata_blob.as_deref() else {
        return Ok((
            format!("{base}; metadata.json missing from the checkpoint tree — manual review"),
            RepairPlan::Manual,
        ));
    };
    let metadata_bytes = match ObjectHash::from_str(metadata_blob)
        .map_err(|e| anyhow::anyhow!("invalid metadata blob OID '{metadata_blob}': {e}"))
        .and_then(|hash| reader.read_raw(&hash))
    {
        Ok(bytes) => bytes,
        Err(err) => {
            return Ok((
                format!("{base}; metadata.json unreadable ({err:#}) — manual review"),
                RepairPlan::Manual,
            ));
        }
    };
    let metadata: CheckpointMetadataProbe = match serde_json::from_slice(&metadata_bytes) {
        Ok(metadata) => metadata,
        Err(err) => {
            return Ok((
                format!("{base}; metadata.json unparsable ({err}) — manual review"),
                RepairPlan::Manual,
            ));
        }
    };
    // Class-2 rows come from either the committed writer (scope='committed')
    // or, since A0-02, the subagent writer (scope='subagent'); both stamp
    // `metadata.json` with the fields needed to rebuild their catalog row.
    // Any other scope is unexpected enough that a human should look.
    let scope = metadata
        .scope
        .clone()
        .or_else(|| rc.scope_trailer.clone())
        .unwrap_or_else(|| "committed".to_string());
    if scope != "committed" && scope != "subagent" {
        return Ok((
            format!("{base}; scope '{scope}' is not auto-repairable — manual review"),
            RepairPlan::Manual,
        ));
    }
    if !session_exists(conn, &metadata.session_id).await? {
        return Ok((
            format!(
                "{base}; agent_session row '{}' is missing (FK target) — manual review",
                metadata.session_id
            ),
            RepairPlan::Manual,
        ));
    }
    if scope == "subagent" {
        return Ok((
            format!("{base}; scope='subagent' row can be rebuilt from the commit's metadata.json"),
            RepairPlan::InsertSubagentCatalogRow {
                checkpoint_id: checkpoint_id.to_string(),
                session_id: metadata.session_id,
                parent_commit: rc.parent_commit_trailer.clone(),
                parent_checkpoint_id: metadata.parent_checkpoint_id,
                subagent_session_id: metadata.subagent_session_id,
                tool_use_id: metadata.tool_use_id,
                description: metadata.description,
                tree_oid: rc.root_tree.clone(),
                metadata_blob_oid: metadata_blob.to_string(),
                traces_commit: rc.commit.clone(),
                created_at: metadata.created_at,
            },
        ));
    }
    Ok((
        format!("{base}; row can be rebuilt from the commit's metadata.json"),
        RepairPlan::InsertCatalogRow {
            checkpoint_id: checkpoint_id.to_string(),
            session_id: metadata.session_id,
            parent_commit: rc.parent_commit_trailer.clone(),
            tree_oid: rc.root_tree.clone(),
            metadata_blob_oid: metadata_blob.to_string(),
            traces_commit: rc.commit.clone(),
            created_at: metadata.created_at,
        },
    ))
}

/// Walk `refs/libra/traces` first-parent and return checkpoint_id →
/// [`RefCheckpoint`] for every reachable checkpoint, attributed to its
/// introducing commit (newest introduction wins after prune rewrites).
/// Failures truncate with a note and under-detect — never over-report.
fn walk_traces_checkpoints(
    reader: &ObjectReader,
    head: Option<ObjectHash>,
    notes: &mut Vec<String>,
) -> HashMap<String, RefCheckpoint> {
    let mut chain: Vec<Commit> = Vec::new();
    let mut cursor = head;
    while let Some(oid) = cursor {
        if chain.len() >= MAX_TRACES_WALK_COMMITS {
            notes.push(format!(
                "traces walk stopped after {MAX_TRACES_WALK_COMMITS} commits (cycle guard)"
            ));
            break;
        }
        match reader.read_commit(&oid) {
            Ok(commit) => {
                cursor = commit.parent_commit_ids.first().copied();
                chain.push(commit);
            }
            Err(err) => {
                notes.push(format!("traces walk truncated: {err:#}"));
                break;
            }
        }
    }

    let mut per_commit: Vec<HashMap<String, ObjectHash>> = Vec::with_capacity(chain.len());
    for commit in &chain {
        match checkpoint_ids_in_commit(reader, commit) {
            Ok(ids) => per_commit.push(ids),
            Err(err) => {
                notes.push(format!(
                    "checkpoint tree of commit {} unreadable: {err:#}",
                    commit.id
                ));
                per_commit.push(HashMap::new());
            }
        }
    }

    let empty: HashMap<String, ObjectHash> = HashMap::new();
    let mut out: HashMap<String, RefCheckpoint> = HashMap::new();
    for (index, commit) in chain.iter().enumerate() {
        let parent_ids = per_commit.get(index + 1).unwrap_or(&empty);
        for (id, inner_oid) in &per_commit[index] {
            if parent_ids.contains_key(id) || out.contains_key(id) {
                continue;
            }
            let trailers = parse_libra_trailers(&commit.message);
            let mut rc = RefCheckpoint {
                commit: commit.id.to_string(),
                root_tree: commit.tree_id.to_string(),
                metadata_blob: None,
                manifest_present: false,
                metadata_present: false,
                inner_readable: false,
                parent_commit_trailer: trailers.parent_commit,
                scope_trailer: trailers.scope,
            };
            match reader.read_tree(inner_oid) {
                Ok(inner) => {
                    rc.inner_readable = true;
                    for item in &inner.tree_items {
                        match item.name.as_str() {
                            "manifest.json" => rc.manifest_present = true,
                            "metadata.json" => {
                                rc.metadata_present = true;
                                rc.metadata_blob = Some(item.id.to_string());
                            }
                            _ => {}
                        }
                    }
                }
                Err(err) => {
                    notes.push(format!("checkpoint {id} inner tree unreadable: {err:#}"));
                }
            }
            out.insert(id.clone(), rc);
        }
    }
    out
}

/// Enumerate `checkpoint/<prefix>/<rest>` ids (and their inner tree OIDs)
/// in one commit's root tree.
fn checkpoint_ids_in_commit(
    reader: &ObjectReader,
    commit: &Commit,
) -> anyhow::Result<HashMap<String, ObjectHash>> {
    let root = reader.read_tree(&commit.tree_id)?;
    let Some(checkpoint_entry) = root
        .tree_items
        .iter()
        .find(|item| item.name == "checkpoint" && item.mode == TreeItemMode::Tree)
    else {
        return Ok(HashMap::new());
    };
    let checkpoint_tree = reader.read_tree(&checkpoint_entry.id)?;
    let mut out = HashMap::new();
    for prefix in &checkpoint_tree.tree_items {
        if prefix.mode != TreeItemMode::Tree {
            continue;
        }
        let prefix_tree = reader.read_tree(&prefix.id)?;
        for rest in &prefix_tree.tree_items {
            if rest.mode != TreeItemMode::Tree {
                continue;
            }
            out.insert(format!("{}{}", prefix.name, rest.name), rest.id);
        }
    }
    Ok(out)
}

/// Determine whether a catalog-only row (not ref-reachable) points at a
/// legacy-v1 checkpoint tree. Any read failure returns `false` — a row
/// whose layout cannot be proven legacy stays subject to class-1 checks.
fn row_layout_is_legacy_v1(reader: &ObjectReader, tree_oid: &str, checkpoint_id: &str) -> bool {
    let (Some(prefix), Some(rest)) = (checkpoint_id.get(..2), checkpoint_id.get(2..)) else {
        return false;
    };
    let Ok(root_oid) = ObjectHash::from_str(tree_oid) else {
        return false;
    };
    let Ok(root) = reader.read_tree(&root_oid) else {
        return false;
    };
    let mut current = root;
    for segment in ["checkpoint", prefix, rest] {
        let Some(entry) = current
            .tree_items
            .iter()
            .find(|item| item.name == segment && item.mode == TreeItemMode::Tree)
        else {
            return false;
        };
        let Ok(next) = reader.read_tree(&entry.id) else {
            return false;
        };
        current = next;
    }
    let manifest_present = current
        .tree_items
        .iter()
        .any(|item| item.name == "manifest.json");
    let metadata_present = current
        .tree_items
        .iter()
        .any(|item| item.name == "metadata.json");
    metadata_present && !manifest_present
}

/// Render the class-1 `missing_objects` detail from the collected
/// "path oid" descriptors (row columns first, then E4 sidecars).
fn missing_objects_detail(missing: &[String]) -> String {
    if missing.is_empty() {
        // Reached only via the stale-but-unverifiable branch.
        return "row disagrees with refs/libra/traces but the ref-side objects \
                could not be verified"
            .to_string();
    }
    format!(
        "objects missing from the store: {} — no destructive action taken",
        missing.join(", ")
    )
}

async fn load_catalog_rows(conn: &DatabaseConnection) -> CliResult<Vec<CatalogRow>> {
    let backend = conn.get_database_backend();
    let rows = conn
        .query_all(Statement::from_sql_and_values(
            backend,
            "SELECT checkpoint_id, tree_oid, metadata_blob_oid, traces_commit \
             FROM agent_checkpoint ORDER BY created_at ASC, checkpoint_id ASC",
            [],
        ))
        .await
        .map_err(|e| CliError::fatal(format!("failed to query agent_checkpoint: {e}")))?;
    Ok(rows
        .into_iter()
        .map(|row| CatalogRow {
            checkpoint_id: row.try_get_by("checkpoint_id").unwrap_or_default(),
            tree_oid: row.try_get_by("tree_oid").unwrap_or_default(),
            metadata_blob_oid: row.try_get_by("metadata_blob_oid").unwrap_or_default(),
            traces_commit: row.try_get_by("traces_commit").unwrap_or_default(),
        })
        .collect())
}

async fn session_exists(conn: &DatabaseConnection, session_id: &str) -> CliResult<bool> {
    let backend = conn.get_database_backend();
    conn.query_one(Statement::from_sql_and_values(
        backend,
        "SELECT 1 FROM agent_session WHERE session_id = ? LIMIT 1",
        [session_id.into()],
    ))
    .await
    .map(|row| row.is_some())
    .map_err(|e| CliError::fatal(format!("failed to query agent_session: {e}")))
}

/// Same repo-id resolution as the background indexer
/// (`client_storage::resolve_repo_id_for_index`): the `libra.repoid`
/// config key, falling back to `unknown-repo`.
/// Blob OID (hex) of `bytes` WITHOUT writing — content addressing lets us
/// test whether an on-disk `findings.md` would restore the exact missing
/// object before touching the store.
fn blob_oid_hex(bytes: &[u8]) -> String {
    let header = format!("blob {}\0", bytes.len());
    let mut content = header.into_bytes();
    content.extend_from_slice(bytes);
    ObjectHash::new(&content).to_string()
}

/// Minimal parse of a run manifest's non-null `findings_oid` (avoids pulling
/// the review/investigate manifest structs into doctor).
fn manifest_findings_oid(manifest_path: &std::path::Path) -> Option<String> {
    let bytes = std::fs::read(manifest_path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value.get("findings_oid")?.as_str().map(str::to_string)
}

/// A0-06: scan review/investigate run manifests for findings-object
/// inconsistencies, mirroring the checkpoint-store scan scoped to the
/// `findings_oid` blob:
/// - `missing_findings_object`: the blob is absent — AUTO-repairable when an
///   on-disk `findings.md` re-hashes to the same OID (content-addressed, so
///   the rewrite is exact + idempotent); MANUAL when `findings.md` is gone or
///   changed.
/// - `missing_findings_object_index`: the blob is present but has no/drifted
///   `object_index` row — re-inserted so cloud sync / retention see it.
///
/// Doctor is foreground: repairs write the blob + insert the `object_index`
/// row directly (idempotent), never the background enqueue.
async fn scan_agent_findings(
    conn: &DatabaseConnection,
    schema_present: bool,
    repair: bool,
) -> CliResult<FindingsStoreReport> {
    use crate::internal::ai::review::store::{AGENT_FINDINGS_OTYPE, is_valid_run_id};

    let mut report = FindingsStoreReport {
        scanned: false,
        note: None,
        runs_with_findings: 0,
        repair_applied: repair,
        repaired: 0,
        manual_required: 0,
        findings: Vec::new(),
    };
    if !schema_present {
        report.note = Some("agent schema not present (run `libra init`?)".to_string());
        return Ok(report);
    }
    let repo_path = match util::try_get_storage_path(None) {
        Ok(path) => path,
        Err(err) => {
            report.note = Some(format!("failed to locate .libra directory: {err}"));
            return Ok(report);
        }
    };
    report.scanned = true;
    let reader = ObjectReader {
        storage: Arc::new(ClientStorage::init(repo_path.join("objects"))),
    };
    let repo_id = resolve_repo_id(conn).await;
    let runs_root = repo_path.join("sessions").join("agent-runs");
    let entries = match std::fs::read_dir(&runs_root) {
        Ok(read_dir) => read_dir,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(err) => {
            report.note = Some(format!("failed to read agent-runs directory: {err}"));
            return Ok(report);
        }
    };
    for entry in entries.flatten() {
        let run_dir = entry.path();
        if !run_dir.is_dir() {
            continue;
        }
        let Some(run_id) = run_dir
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        // Skip `.admission` and any foreign directory — same validity gate the
        // run stores use.
        if !is_valid_run_id(&run_id) {
            continue;
        }
        let Some(findings_oid) = manifest_findings_oid(&run_dir.join("manifest.json")) else {
            continue;
        };
        report.runs_with_findings += 1;

        let on_disk = std::fs::read(run_dir.join("findings.md"))
            .ok()
            .filter(|bytes| !bytes.is_empty());

        if !reader.exists_str(&findings_oid) {
            // Auto-repairable only when findings.md is present AND re-hashes to
            // the missing OID (content-addressed → the rewrite is exact and
            // idempotent). `filter` binds the bytes without any fallible unwrap.
            let recoverable = on_disk
                .as_ref()
                .filter(|bytes| blob_oid_hex(bytes) == findings_oid);
            if let Some(bytes) = recoverable {
                let (repaired, detail) = if repair {
                    crate::utils::object::write_git_object(&repo_path, "blob", bytes).map_err(
                        |e| {
                            CliError::fatal(format!(
                                "failed to rewrite findings blob for run '{run_id}': {e}"
                            ))
                        },
                    )?;
                    insert_object_index_row(
                        conn,
                        &findings_oid,
                        AGENT_FINDINGS_OTYPE,
                        bytes.len() as i64,
                        &repo_id,
                    )
                    .await
                    .map_err(|e| {
                        CliError::fatal(format!(
                            "failed to reinsert findings object_index for run '{run_id}': {e}"
                        ))
                    })?;
                    report.repaired += 1;
                    (
                        true,
                        format!(
                            "findings object {findings_oid} was missing; rewrote it from findings.md"
                        ),
                    )
                } else {
                    (
                        false,
                        format!(
                            "findings object {findings_oid} is missing but findings.md matches — repairable"
                        ),
                    )
                };
                report.findings.push(FindingsObjectFinding {
                    inconsistency_type: CLASS_MISSING_FINDINGS_OBJECT.to_string(),
                    run_id,
                    detail,
                    repaired,
                    manual_required: false,
                });
            } else {
                report.manual_required += 1;
                report.findings.push(FindingsObjectFinding {
                    inconsistency_type: CLASS_MISSING_FINDINGS_OBJECT.to_string(),
                    run_id,
                    detail: format!(
                        "findings object {findings_oid} is missing and findings.md is absent or changed — manual review"
                    ),
                    repaired: false,
                    manual_required: true,
                });
            }
            continue;
        }

        // Blob present — ensure a correctly-shaped object_index row.
        let Ok(hash) = ObjectHash::from_str(&findings_oid) else {
            continue;
        };
        let o_size = reader.read_raw(&hash).map(|b| b.len() as i64).unwrap_or(0);
        let shape = object_index_row_shape(conn, &findings_oid, &repo_id).await?;
        // The o_type is cosmetic here: doctor enumerates findings from the
        // MANIFEST (`findings_oid`), not from `object_index`, and cloud sync
        // does not filter by o_type. The shared index consumer also refuses to
        // retag an already-`agent_*` row, so a findings blob whose bytes were
        // first indexed as `agent_transcript` legitimately keeps that tag.
        // Flag only a genuinely-untracked (absent) or wrong-SIZE row; never a
        // benign tag difference — that would start a doctor↔writer tag-war.
        let needs_repair = match &shape {
            None => true,
            Some((_o_type, size)) => *size != o_size,
        };
        if needs_repair {
            let repaired = if repair {
                if shape.is_some() {
                    update_object_index_row_shape(
                        conn,
                        &findings_oid,
                        AGENT_FINDINGS_OTYPE,
                        o_size,
                        &repo_id,
                    )
                    .await
                    .map_err(|e| {
                        CliError::fatal(format!(
                            "failed to update findings object_index for run '{run_id}': {e}"
                        ))
                    })?;
                } else {
                    insert_object_index_row(
                        conn,
                        &findings_oid,
                        AGENT_FINDINGS_OTYPE,
                        o_size,
                        &repo_id,
                    )
                    .await
                    .map_err(|e| {
                        CliError::fatal(format!(
                            "failed to insert findings object_index for run '{run_id}': {e}"
                        ))
                    })?;
                }
                report.repaired += 1;
                true
            } else {
                false
            };
            report.findings.push(FindingsObjectFinding {
                inconsistency_type: CLASS_MISSING_FINDINGS_OBJECT_INDEX.to_string(),
                run_id,
                detail: format!("findings object {findings_oid} has no/drifted object_index row"),
                repaired,
                manual_required: false,
            });
        }
    }
    Ok(report)
}

async fn resolve_repo_id(conn: &DatabaseConnection) -> String {
    match ConfigKv::get_with_conn(conn, "libra.repoid").await {
        Ok(Some(entry)) if !entry.value.trim().is_empty() => entry.value,
        _ => "unknown-repo".to_string(),
    }
}

/// Existing `object_index` row shape for `(o_id, repo_id)`, if any.
/// Class-3 compares it against the writer's expected `o_type`/`o_size` —
/// a row that exists but drifted (e.g. a transcript blob indexed as a
/// generic `blob`) is as broken for cloud-sync semantics as a missing
/// one and must be repairable in place.
async fn object_index_row_shape(
    conn: &DatabaseConnection,
    o_id: &str,
    repo_id: &str,
) -> CliResult<Option<(String, i64)>> {
    let backend = conn.get_database_backend();
    let row = conn
        .query_one(Statement::from_sql_and_values(
            backend,
            "SELECT o_type, o_size FROM object_index WHERE o_id = ? AND repo_id = ? LIMIT 1",
            [o_id.into(), repo_id.into()],
        ))
        .await
        .map_err(|e| CliError::fatal(format!("failed to query object_index: {e}")))?;
    row.map(|r| {
        let o_type: String = r
            .try_get("", "o_type")
            .map_err(|e| CliError::fatal(format!("failed to read object_index.o_type: {e}")))?;
        let o_size: i64 = r
            .try_get("", "o_size")
            .map_err(|e| CliError::fatal(format!("failed to read object_index.o_size: {e}")))?;
        Ok::<_, CliError>((o_type, o_size))
    })
    .transpose()
}

/// Update a drifted `object_index` row in place to the writer-expected
/// shape (idempotent; matched on `(o_id, repo_id)`).
async fn update_object_index_row_shape(
    conn: &DatabaseConnection,
    o_id: &str,
    o_type: &str,
    o_size: i64,
    repo_id: &str,
) -> Result<(), sea_orm::DbErr> {
    let backend = conn.get_database_backend();
    conn.execute(Statement::from_sql_and_values(
        backend,
        "UPDATE object_index SET o_type = ?, o_size = ? WHERE o_id = ? AND repo_id = ?",
        [o_type.into(), o_size.into(), o_id.into(), repo_id.into()],
    ))
    .await
    .map(|_| ())
}

/// Idempotent `object_index` insert mirroring the row shape written by
/// `client_storage::update_object_index_once` (`is_synced = 0` so the next
/// `libra cloud sync` picks the object up). The `WHERE NOT EXISTS` guard
/// makes a doctor re-run (or a race with the background indexer) a no-op.
async fn insert_object_index_row(
    conn: &DatabaseConnection,
    o_id: &str,
    o_type: &str,
    o_size: i64,
    repo_id: &str,
) -> Result<(), sea_orm::DbErr> {
    let backend = conn.get_database_backend();
    conn.execute(Statement::from_sql_and_values(
        backend,
        "INSERT INTO object_index (o_id, o_type, o_size, repo_id, created_at, is_synced) \
         SELECT ?, ?, ?, ?, ?, 0 \
         WHERE NOT EXISTS (SELECT 1 FROM object_index WHERE o_id = ? AND repo_id = ?)",
        [
            o_id.into(),
            o_type.into(),
            o_size.into(),
            repo_id.into(),
            Utc::now().timestamp().into(),
            o_id.into(),
            repo_id.into(),
        ],
    ))
    .await
    .map(|_| ())
}

// ---------------------------------------------------------------------------
// Provider hooks + report rendering
// ---------------------------------------------------------------------------

fn check_provider(
    name: &'static str,
    tier: AgentStability,
    provider: Option<&dyn crate::internal::ai::hooks::provider::HookProvider>,
) -> ProviderHookStatus {
    let Some(provider) = provider else {
        // Preview adapters don't carry a HookProvider yet. Surface them
        // explicitly as preview/unknown so the report is still complete.
        return ProviderHookStatus {
            name,
            tier,
            installed: None,
            error: None,
        };
    };
    match provider.hooks_are_installed() {
        Ok(installed) => ProviderHookStatus {
            name,
            tier,
            installed: Some(installed),
            error: None,
        },
        Err(err) => ProviderHookStatus {
            name,
            tier,
            installed: None,
            error: Some(err.to_string()),
        },
    }
}

fn emit_report(report: &DoctorReport, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("agent_doctor", report, output);
    }
    if output.quiet {
        return Ok(());
    }
    println!(
        "Schema present       : {}",
        if report.schema_present { "yes" } else { "no" }
    );
    println!("Active sessions      : {}", report.active_sessions);
    println!("Stopped sessions     : {}", report.stopped_sessions);
    println!("Orphan checkpoints   : {}", report.orphan_checkpoints);

    println!("Provider hooks:");
    for ph in &report.provider_hooks {
        let tier_tag = match ph.tier {
            AgentStability::Preview => " [preview]",
            AgentStability::Stable => "",
        };
        match (ph.installed, &ph.error) {
            (Some(true), _) => println!("  {}{tier_tag}: installed", ph.name),
            (Some(false), _) => println!("  {}{tier_tag}: NOT installed", ph.name),
            (None, Some(err)) => println!("  {}{tier_tag}: error — {err}", ph.name),
            (None, None) => println!("  {}{tier_tag}: not yet installable", ph.name),
        }
    }

    let store = &report.checkpoint_store;
    println!("Checkpoint store:");
    if !store.scanned {
        println!(
            "  not scanned ({})",
            store.note.as_deref().unwrap_or("unavailable")
        );
    } else {
        println!("  Catalog rows          : {}", store.catalog_rows);
        println!(
            "  Ref-reachable         : {}",
            store.ref_reachable_checkpoints
        );
        println!("  Legacy-v1 checkpoints : {}", store.legacy_v1_checkpoints);
        println!("  Live in-flight markers: {}", store.live_inflight_markers);
        println!("  Inconsistencies       : {}", store.findings.len());
        for finding in &store.findings {
            let status = if finding.repaired {
                "repaired"
            } else if finding.manual_required {
                "manual action required"
            } else if store.repair_applied {
                "NOT repaired"
            } else {
                "detected (run --repair)"
            };
            println!(
                "    [{}] {}: {} — {status}",
                finding.inconsistency_type, finding.checkpoint_id, finding.detail
            );
        }
        if let Some(note) = &store.note {
            println!("  Note: {note}");
        }
    }

    let findings = &report.findings_store;
    println!("Findings store:");
    if !findings.scanned {
        println!(
            "  not scanned ({})",
            findings.note.as_deref().unwrap_or("unavailable")
        );
    } else {
        println!("  Runs with findings    : {}", findings.runs_with_findings);
        println!("  Inconsistencies       : {}", findings.findings.len());
        for finding in &findings.findings {
            let status = if finding.repaired {
                "repaired"
            } else if finding.manual_required {
                "manual action required"
            } else if findings.repair_applied {
                "NOT repaired"
            } else {
                "detected (run --repair)"
            };
            println!(
                "    [{}] {}: {} — {status}",
                finding.inconsistency_type, finding.run_id, finding.detail
            );
        }
        if let Some(note) = &findings.note {
            println!("  Note: {note}");
        }
    }

    if report.orphan_checkpoints > 0 {
        println!(
            "Hint: orphan checkpoints indicate broken FK cascade — \
             consider `libra agent clean --all`."
        );
    }
    let auto_repairable = store
        .findings
        .iter()
        .filter(|f| !f.repaired && !f.manual_required)
        .count();
    if !store.repair_applied && auto_repairable > 0 {
        println!(
            "Hint: run `libra agent doctor --repair` to repair {auto_repairable} \
             inconsistency(ies) automatically."
        );
    }
    if store.manual_required > 0 {
        println!(
            "Hint: {} inconsistency(ies) need manual action — objects are missing \
             from the store (try `libra fsck --heal` or restore them from a \
             cloud/backup remote before re-running doctor).",
            store.manual_required
        );
    }
    if report.gemini_hooks_remnant {
        println!(
            "Hint: leftover gemini hook configuration detected — gemini capture is \
             uninstall-only; run `libra agent remove gemini` to remove it. Captured \
             gemini sessions stay readable."
        );
    }
    if !report.schema_present {
        println!("Hint: run `libra init` to apply pending migrations.");
    }
    Ok(())
}

async fn table_exists(conn: &(impl ConnectionTrait + ?Sized), name: &str) -> CliResult<bool> {
    let backend = conn.get_database_backend();
    conn.query_one(Statement::from_sql_and_values(
        backend,
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ? LIMIT 1",
        [name.into()],
    ))
    .await
    .map(|row| row.is_some())
    .map_err(|e| CliError::fatal(format!("failed to query sqlite_master: {e}")))
}

async fn scalar_count(conn: &(impl ConnectionTrait + ?Sized), sql: &str) -> CliResult<i64> {
    let backend = conn.get_database_backend();
    let row = conn
        .query_one(Statement::from_sql_and_values(backend, sql, []))
        .await
        .map_err(|e| CliError::fatal(format!("doctor query failed: {e}")))?
        .ok_or_else(|| CliError::fatal("doctor count returned no rows".to_string()))?;
    row.try_get_by::<i64, _>("n")
        .map_err(|e| CliError::fatal(format!("failed to decode doctor count: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn libra_trailers_parse_parent_commit_and_scope() {
        let message = "traces: committed checkpoint abc\n\n\
                       Libra-Session: claude__s1\n\
                       Libra-Agent: claude_code\n\
                       Libra-Parent-Commit: 0123456789012345678901234567890123456789\n\
                       Libra-Checkpoint-ID: abc\n\
                       Libra-Scope: committed\n";
        let trailers = parse_libra_trailers(message);
        assert_eq!(
            trailers.parent_commit.as_deref(),
            Some("0123456789012345678901234567890123456789")
        );
        assert_eq!(trailers.scope.as_deref(), Some("committed"));
    }

    #[test]
    fn libra_trailers_tolerate_missing_parent_commit() {
        let message = "traces: committed checkpoint abc\n\n\
                       Libra-Session: claude__s1\n\
                       Libra-Scope: committed\n";
        let trailers = parse_libra_trailers(message);
        assert_eq!(trailers.parent_commit, None);
        assert_eq!(trailers.scope.as_deref(), Some("committed"));
    }

    /// Both metadata generations parse into the doctor probe: v1
    /// (pre-AG-20, no `model`) and v2 (AG-20, adds `model`).
    #[test]
    fn metadata_probe_parses_v1_and_v2_shapes() {
        let v1 = serde_json::json!({
            "schema_version": 1,
            "checkpoint_id": "85ae75d2-4c53-465a-b890-a9f861a50cc7",
            "session_id": "claude__fixture-v1-claude",
            "agent_kind": "claude_code",
            "scope": "committed",
            "provider_session_id": "fixture-v1-claude",
            "working_dir": "/tmp/x",
            "redaction_report": {},
            "created_at": 1783206712
        });
        let parsed: CheckpointMetadataProbe =
            serde_json::from_value(v1).expect("v1 metadata parses");
        assert_eq!(parsed.session_id, "claude__fixture-v1-claude");
        assert_eq!(parsed.created_at, 1783206712);
        assert_eq!(parsed.scope.as_deref(), Some("committed"));

        let v2 = serde_json::json!({
            "schema_version": 2,
            "checkpoint_id": "b",
            "session_id": "claude__s2",
            "agent_kind": "claude_code",
            "scope": "committed",
            "provider_session_id": "s2",
            "working_dir": "/tmp/x",
            "model": "unknown",
            "redaction_report": {},
            "created_at": 42
        });
        let parsed: CheckpointMetadataProbe =
            serde_json::from_value(v2).expect("v2 metadata parses");
        assert_eq!(parsed.session_id, "claude__s2");
        assert_eq!(parsed.created_at, 42);
    }

    /// The manifest sweep understands both transcript shapes: a single
    /// `oid` entry and the E5 chunked `parts` array; single-blob roles
    /// contribute their `path` + `oid`.
    #[test]
    fn manifest_declared_blobs_covers_single_and_chunked_entries() {
        let manifest = serde_json::json!({
            "schema_version": 1,
            "checkpoint_id": "abc",
            "entries": {
                "metadata": { "path": "metadata.json", "oid": "aa1", "byte_len": 100 },
                "lifecycle_events": {
                    "path": "events/lifecycle.jsonl", "oid": "bb2", "byte_len": 200
                },
                "transcript": {
                    "path": "transcript/claude_code.jsonl",
                    "byte_len": 700,
                    "chunked": true,
                    "parts": [
                        {
                            "path": "transcript/claude_code.jsonl.001",
                            "oid": "cc3",
                            "byte_len": 400
                        },
                        {
                            "path": "transcript/claude_code.jsonl.002",
                            "oid": "dd4",
                            "byte_len": 300
                        }
                    ]
                },
                "redaction_report": {
                    "path": "redaction_report.json", "oid": "ee5", "byte_len": 50
                },
                "content_hash": { "path": "content_hash.txt", "oid": "ff6", "byte_len": 71 }
            }
        });
        let mut declared = manifest_declared_blobs(&manifest);
        declared.sort();
        // Chunked transcript: the top-level entry has NO `oid` (only
        // `parts`), so exactly the per-chunk OIDs surface — each with the
        // per-chunk byte_len class 3 uses as o_size, guaranteeing the
        // repair path never reads a transcript payload to size it.
        assert_eq!(
            declared,
            vec![
                ("content_hash.txt".to_string(), "ff6".to_string(), Some(71)),
                (
                    "events/lifecycle.jsonl".to_string(),
                    "bb2".to_string(),
                    Some(200)
                ),
                ("metadata.json".to_string(), "aa1".to_string(), Some(100)),
                (
                    "redaction_report.json".to_string(),
                    "ee5".to_string(),
                    Some(50)
                ),
                (
                    "transcript/claude_code.jsonl.001".to_string(),
                    "cc3".to_string(),
                    Some(400)
                ),
                (
                    "transcript/claude_code.jsonl.002".to_string(),
                    "dd4".to_string(),
                    Some(300)
                ),
            ]
        );

        // Single-file transcript (small): plain `oid` + `byte_len`, no
        // parts — the declared byte_len is what class 3 enqueues.
        let single = serde_json::json!({
            "entries": {
                "transcript": {
                    "path": "transcript/claude_code.jsonl", "oid": "cc9", "byte_len": 42
                }
            }
        });
        assert_eq!(
            manifest_declared_blobs(&single),
            vec![(
                "transcript/claude_code.jsonl".to_string(),
                "cc9".to_string(),
                Some(42)
            )]
        );

        // Legacy/corrupt manifests without byte_len still yield the OIDs
        // (size falls back to a payload read for those objects only).
        let no_len = serde_json::json!({
            "entries": {
                "metadata": { "path": "metadata.json", "oid": "aa7" }
            }
        });
        assert_eq!(
            manifest_declared_blobs(&no_len),
            vec![("metadata.json".to_string(), "aa7".to_string(), None)]
        );
    }
}
