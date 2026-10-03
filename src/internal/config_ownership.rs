//! plan-20260714 §C.4.1.1: the centralized ownership inventory for Code/Agent
//! configuration and approval surfaces (W0 deliverable).
//!
//! Every configuration file, directory, database table, or process cache that
//! a Code/Agent runtime reads MUST be registered here with its owner and its
//! CURRENT read-resolution truth (pre-W4, not aspiration). The register guard
//! below fails when a new `.libra/*.toml|*.json` config read appears in the
//! Code/Agent namespaces without a registry row — the "新增未登记则测试失败"
//! clause of §C.4.1.1. The guard is a literal-scan tripwire: it sees quoted
//! single-segment `*.toml`/`*.json` literals on non-comment production lines
//! (direct joins and const-assigned names alike), but a read assembled at
//! runtime or hidden behind a multi-segment literal is invisible to it —
//! reviewers, not the scan, are the backstop for those.
//!
//! Why this exists: W4-06..W4-12 migrated these surfaces onto
//! [`ReadResolution::UnifiedResolver`]. Linked worktrees now read repository
//! defaults plus optional overlays (W4-08 enablement); remaining
//! `WorkdirDotLibra` rows (if any) are inventory debt, not a launch guard.
//! Damaged/unreadable scope still fail-closes. W4-11 security and W4-12
//! extension loaders resolve through the registry consumer kind.

/// Which consumer family migrates the surface onto the W4-06 resolver.
///
/// W4-06 registers the owner; W4-11 migrates Security loaders and W4-12
/// migrates Extension/automation loaders. The field is inventory-only until
/// those cards rewrite the call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigConsumerKind {
    /// Sandbox / hooks / approval-adjacent security configuration.
    Security,
    /// Agents, automations, prompt rules/contexts, skills, commands.
    Extension,
}

/// Who owns a configuration surface across worktrees (§C.4.1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigOwner {
    /// Repository default layer, with an optional per-worktree overlay once
    /// the W4 unified resolver lands. Pre-W4 there is NO overlay surface.
    RepositoryWithOptionalOverlay,
    /// Repository, shared identically across every worktree by design
    /// (e.g. Always-approvals express trust in the whole project).
    Repository,
    /// Repository rows additionally scoped by workspace/session keys at W4
    /// (plan line 2272: owner claim/upsert/queries carry
    /// `repo_id/worktree_id/workspace_id`, conflicts fail closed). Kept as a
    /// distinct typed owner so consumers cannot mistake these tables for
    /// plain repository-shared state.
    RepositoryWithWorkspaceSessionScope,
}

/// Where the surface's reads resolve TODAY. This column records the truth,
/// including the pre-W4 brain-split form — it is what the W4 resolver work
/// migrates, not a description of the end state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadResolution {
    /// `<working_dir>/.libra/<location>`: correct in the main worktree
    /// (local == common), WRONG in a linked one — which is why the W0
    /// linked preflight refuses to start the readers there.
    WorkdirDotLibra,
    /// Via the fail-closed common-storage resolver (§C.4.1).
    CommonStorage,
    /// Via the W4-06 [`crate::internal::ai::sources::resolver`] (RequestScope
    /// + provenance). Security loaders (W4-11) use tighten-only overlay merge.
    UnifiedResolver,
    /// SQLite table(s) in the shared repository database.
    RepositoryDatabase,
    /// In-process cache; subject to the §C.4.1.1 process-cache key rules.
    ProcessCache,
}

/// What kind of thing `location` names (drives the register guard).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceKind {
    /// A single file under the resolution root; the register guard scans the
    /// Code/Agent sources for `join("<location>")` reads of it.
    File,
    /// A directory tree under the resolution root (name too generic for a
    /// reliable source scan; registry row only).
    Directory,
    /// Database table(s) or an in-memory store.
    Store,
}

pub struct ConfigSurface {
    /// Human name used in diagnostics and the plan.
    pub surface: &'static str,
    /// File/dir name under `.libra/`, or the table/store name.
    pub location: &'static str,
    pub kind: SurfaceKind,
    pub owner: ConfigOwner,
    pub resolution: ReadResolution,
    /// W4-06 consumer family (Security → W4-11, Extension → W4-12).
    pub consumer: ConfigConsumerKind,
}

/// One process-local synchronization/cache cell covered by the Code/Agent
/// ownership inventory.
///
/// A Rust static name is only unique within its module, so its repository-
/// relative source path is part of the identity. This prevents independent
/// test controls such as `CONTROLS` and `PAUSE` from hiding each other in the
/// forward/reverse inventory guard.
pub struct ProcessCacheSurface {
    /// Repository-relative Rust source path, using `/` separators.
    pub source_path: &'static str,
    /// Rust static identifier declared in [`Self::source_path`].
    pub static_name: &'static str,
    /// Why this process-local surface is safe and how it is scoped.
    pub discipline: &'static str,
}

/// The §C.4.1.1 registry. Rows mirror plan-20260714 lines 2268/2272/2274.
pub const CODE_AGENT_CONFIG_OWNERSHIP: &[ConfigSurface] = &[
    ConfigSurface {
        surface: "code/provider configuration ([approval] is security-sensitive — W4-06 treats the whole \
                  file as Security so overlays cannot wholesale weaken approval;
                  W4-11/W4-12 section-merge approval)",
        location: "config.toml",
        kind: SurfaceKind::File,
        owner: ConfigOwner::RepositoryWithOptionalOverlay,
        resolution: ReadResolution::UnifiedResolver,
        consumer: ConfigConsumerKind::Security,
    },
    ConfigSurface {
        surface: "sandbox policy (security: repository layer must never be \
                  weakened by an overlay)",
        location: "sandbox.toml",
        kind: SurfaceKind::File,
        owner: ConfigOwner::RepositoryWithOptionalOverlay,
        resolution: ReadResolution::UnifiedResolver,
        consumer: ConfigConsumerKind::Security,
    },
    ConfigSurface {
        surface: "hook runner policy (security: repository PreToolUse Block \
                  must hold in every worktree and sub-agent workdir)",
        location: "hooks.json",
        kind: SurfaceKind::File,
        owner: ConfigOwner::RepositoryWithOptionalOverlay,
        resolution: ReadResolution::UnifiedResolver,
        consumer: ConfigConsumerKind::Security,
    },
    ConfigSurface {
        surface: "automation rules (W4-08: healthy linked worktrees dispatch \
                  via resolver; damaged/unregistered scope still fail-closes)",
        location: "automations.toml",
        kind: SurfaceKind::File,
        owner: ConfigOwner::RepositoryWithOptionalOverlay,
        resolution: ReadResolution::UnifiedResolver,
        consumer: ConfigConsumerKind::Extension,
    },
    ConfigSurface {
        surface: "persisted Always approvals (repo-wide visibility is the \
                  intended semantic; W4 adds provenance columns)",
        location: "approved_permission",
        kind: SurfaceKind::Store,
        owner: ConfigOwner::Repository,
        resolution: ReadResolution::RepositoryDatabase,
        consumer: ConfigConsumerKind::Security,
    },
    ConfigSurface {
        surface: "in-memory approval cache (W4: keyed by canonical repo id)",
        location: "ApprovalStore",
        kind: SurfaceKind::Store,
        owner: ConfigOwner::Repository,
        resolution: ReadResolution::ProcessCache,
        consumer: ConfigConsumerKind::Security,
    },
    ConfigSurface {
        surface: "agent capture/export state (W4 adds workspace scope keys)",
        location: "agent_session/agent_export_job/agent_import_identity",
        kind: SurfaceKind::Store,
        owner: ConfigOwner::RepositoryWithWorkspaceSessionScope,
        resolution: ReadResolution::RepositoryDatabase,
        consumer: ConfigConsumerKind::Extension,
    },
];

/// §C.4.1.1 / plan line 2272: EVERY Code/Agent database table, classified.
/// The forward guard extracts `CREATE TABLE` names with the Code/Agent
/// prefixes (`agent_`, `automation_`, `approved_`, `ai_`) from the SQL
/// corpus and fails when one is missing here — a new Code/Agent table
/// cannot ship unclassified.
pub const CODE_AGENT_TABLE_OWNERSHIP: &[(&str, ConfigOwner)] = &[
    // Capture/export/import group (plan line 2272): Repository rows that
    // gain workspace/session scope keys at W4.
    (
        "agent_session",
        ConfigOwner::RepositoryWithWorkspaceSessionScope,
    ),
    (
        "agent_export_job",
        ConfigOwner::RepositoryWithWorkspaceSessionScope,
    ),
    (
        "agent_import_identity",
        ConfigOwner::RepositoryWithWorkspaceSessionScope,
    ),
    (
        "agent_import_tombstone",
        ConfigOwner::RepositoryWithWorkspaceSessionScope,
    ),
    (
        "agent_coverage_claim",
        ConfigOwner::RepositoryWithWorkspaceSessionScope,
    ),
    (
        "agent_coverage_conflict",
        ConfigOwner::RepositoryWithWorkspaceSessionScope,
    ),
    (
        "agent_coverage_revision",
        ConfigOwner::RepositoryWithWorkspaceSessionScope,
    ),
    (
        "agent_capture_cloud_base",
        ConfigOwner::RepositoryWithWorkspaceSessionScope,
    ),
    (
        "agent_capture_incarnation",
        ConfigOwner::RepositoryWithWorkspaceSessionScope,
    ),
    (
        "agent_capture_scope_down_guard",
        ConfigOwner::RepositoryWithWorkspaceSessionScope,
    ),
    (
        "agent_checkpoint",
        ConfigOwner::RepositoryWithWorkspaceSessionScope,
    ),
    (
        "agent_checkpoint_prune_tombstone",
        ConfigOwner::RepositoryWithWorkspaceSessionScope,
    ),
    // Bridge durable projection (plan-20260818 LB-02): repository-scoped rows
    // carrying repository_id/worktree_id/workspace_id scope (bridge session,
    // event, operation, checkpoint, link and the migration guard).
    (
        "agent_bridge_session",
        ConfigOwner::RepositoryWithWorkspaceSessionScope,
    ),
    (
        "agent_bridge_event",
        ConfigOwner::RepositoryWithWorkspaceSessionScope,
    ),
    (
        "agent_bridge_operation",
        ConfigOwner::RepositoryWithWorkspaceSessionScope,
    ),
    (
        "agent_bridge_checkpoint",
        ConfigOwner::RepositoryWithWorkspaceSessionScope,
    ),
    (
        "agent_bridge_link",
        ConfigOwner::RepositoryWithWorkspaceSessionScope,
    ),
    (
        "agent_bridge_capture_down_guard",
        ConfigOwner::RepositoryWithWorkspaceSessionScope,
    ),
    (
        "agent_bridge_link_relations_down_guard",
        ConfigOwner::RepositoryWithWorkspaceSessionScope,
    ),
    (
        "agent_subagent_content_claim",
        ConfigOwner::RepositoryWithWorkspaceSessionScope,
    ),
    (
        "agent_subagent_content_revision",
        ConfigOwner::RepositoryWithWorkspaceSessionScope,
    ),
    (
        "agent_subagent_link",
        ConfigOwner::RepositoryWithWorkspaceSessionScope,
    ),
    // Append/aggregate + audit tables: Repository (UUID/append keyed; the
    // writers are constrained by the W0 linked code preflight).
    ("source_call_log", ConfigOwner::Repository),
    ("source_call_log__rebuild", ConfigOwner::Repository),
    ("agent_audit_log", ConfigOwner::Repository),
    ("agent_usage_stats", ConfigOwner::Repository),
    ("agent_usage_stats__rebuild", ConfigOwner::Repository),
    ("agent_workspace_scope_audit", ConfigOwner::Repository),
    ("automation_log", ConfigOwner::Repository),
    ("approved_permission", ConfigOwner::Repository),
    // Migration 2026081301 bookkeeping: blocks the provenance-backfill
    // down-migration; repo-wide guard row, dropped by the migration itself.
    (
        "approved_permission_provenance_down_guard",
        ConfigOwner::Repository,
    ),
    // AI thread/scheduler/index/runtime-contract tables: Repository
    // (UUID-keyed rows, no cross-worktree row conflicts).
    ("ai_thread", ConfigOwner::Repository),
    ("ai_thread_intent", ConfigOwner::Repository),
    ("ai_thread_participant", ConfigOwner::Repository),
    ("ai_thread_provider_metadata", ConfigOwner::Repository),
    ("ai_scheduler_state", ConfigOwner::Repository),
    ("ai_scheduler_plan_head", ConfigOwner::Repository),
    ("ai_scheduler_selected_plan", ConfigOwner::Repository),
    ("ai_index_intent_context_frame", ConfigOwner::Repository),
    ("ai_index_intent_plan", ConfigOwner::Repository),
    ("ai_index_intent_task", ConfigOwner::Repository),
    ("ai_index_plan_step_task", ConfigOwner::Repository),
    ("ai_index_run_event", ConfigOwner::Repository),
    ("ai_index_run_patchset", ConfigOwner::Repository),
    ("ai_index_task_run", ConfigOwner::Repository),
    // B3-16 protected-down rebuild staging (created and dropped in the same
    // migration; never durable).
    ("ai_index_task_run__rebuild", ConfigOwner::Repository),
    ("ai_live_context_window", ConfigOwner::Repository),
    ("ai_decision_proposal", ConfigOwner::Repository),
    ("ai_final_decision", ConfigOwner::Repository),
    ("ai_risk_score_breakdown", ConfigOwner::Repository),
    ("ai_validation_report", ConfigOwner::Repository),
    // V2 operation records carry optional AI/session provenance but remain
    // repository-owned; the operation store is not a configuration surface.
    ("ai_operation_link", ConfigOwner::Repository),
];

/// §C.4.1.1 process-cache inventory: every `static` synchronization/cache
/// cell in the Code/Agent namespaces, keyed by its repository-relative source
/// path and static name. The forward guard scans for
/// `static NAME: …OnceLock|LazyLock|Mutex|RwLock` and fails when a new cell is
/// missing here.
pub const CODE_AGENT_PROCESS_CACHES: &[ProcessCacheSurface] = &[
    ProcessCacheSurface {
        source_path: "src/internal/ai/history.rs",
        static_name: "CLEANUP_HELPER_REAPER",
        discipline: "process-lifetime reaper thread handle; holds no repository state",
    },
    ProcessCacheSurface {
        source_path: "src/internal/ai/authorized_read.rs",
        static_name: "RUNNING_LIBRA_PROGRAM",
        discipline: "process-lifetime executable path registered only by Libra's main entrypoint; \
                     identifies the fixed-argument authorized-read helper and holds no repository state",
    },
    ProcessCacheSurface {
        source_path: "src/internal/ai/observed_agents/redaction.rs",
        static_name: "DEFAULT_RULES",
        discipline: "compiled built-in redaction rule set; input-independent constant",
    },
    ProcessCacheSurface {
        source_path: "src/internal/ai/session/jsonl.rs",
        static_name: "CURRENT_PROCESS_OWNER_IDENTITY",
        discipline: "process-lifetime own pid/starttime/boot_id identity for the session \
                     writer-lease liveness probe; holds no repository state",
    },
    ProcessCacheSurface {
        source_path: "src/internal/ai/agent_bridge/vcs.rs",
        static_name: "BRIDGE_LIVE_REVIEW_RUNS",
        discipline: "not a cache: the live-run registry `libra agent bridge --stdio` uses to \
                     cancel and drain the review runs it started when the stdio loop ends \
                     (GC-LB-10). Keyed by run id, holds only cancel/join handles for this \
                     process's own runs, and is emptied by the shutdown drain",
    },
    ProcessCacheSurface {
        source_path: "src/main.rs",
        static_name: "CONTROLS",
        discipline: "test-only private-helper fault controls; process-local and restored by \
                     ControlsReset",
    },
    ProcessCacheSurface {
        source_path: "src/command/agent/import.rs",
        static_name: "CONTROLS",
        discipline: "test-only agent-import fault controls; process-local and restored by \
                     ControlsReset",
    },
    ProcessCacheSurface {
        source_path: "src/internal/ai/capture/test_support.rs",
        static_name: "DELETE_RESERVED_SESSIONS",
        discipline: "test-only capture reservation-erase fault control; keyed by full session id and \
                     consumed by the matching in-process capture test",
    },
    ProcessCacheSurface {
        source_path: "src/command/agent/doctor.rs",
        static_name: "PAUSE",
        discipline: "test-only agent-doctor index-repair rendezvous; process-local and consumed \
                     or restored by its test guard",
    },
    ProcessCacheSurface {
        source_path: "src/internal/ai/observed_agents/builtin/stable_promoted.rs",
        static_name: "PAUSE",
        discipline: "test-only observed-agent discovery rendezvous; process-local and consumed \
                     or restored by its test guard",
    },
    ProcessCacheSurface {
        source_path: "src/internal/ai/observed_agents/transcript_source.rs",
        static_name: "PAUSE",
        discipline: "test-only secure transcript-open rendezvous; process-local and consumed \
                     or restored by its test guard",
    },
    ProcessCacheSurface {
        source_path: "src/internal/ai/capture/checkpoint.rs",
        static_name: "PRE_REGISTRATION_PAUSE",
        discipline: "test-only pre-registration checkpoint rendezvous; process-local and \
                     consumed by the scoped pause guard",
    },
    ProcessCacheSurface {
        source_path: "src/internal/ai/capture/checkpoint.rs",
        static_name: "REGISTRATION_PAUSE",
        discipline: "test-only checkpoint-registration rendezvous; process-local and \
                     consumed by the scoped pause guard",
    },
    ProcessCacheSurface {
        source_path: "src/internal/ai/subagent_content.rs",
        static_name: "TEST_SUBAGENT_FINAL_APPEND_DEADLINE",
        discipline: "test-only task-local observation mutex for the final subagent history append deadline; \
                     scoped to one regression test and holds no repository state",
    },
];

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        fs,
        path::{Path, PathBuf},
    };

    use super::*;

    /// Filenames matched by the scan that are NOT Code/Agent configuration.
    /// Adding a name here is a reviewed decision, same as adding a registry
    /// row. Test fixtures never reach this list: the scan reads only the
    /// production half of each file (everything before its `#[cfg(test)]`
    /// module), and path-shaped literals (containing `/`) are skipped —
    /// workdir config surfaces are single names directly under `.libra/`.
    const NON_CONFIG_ALLOWLIST: &[&str] = &[
        "state.json",               // runtime session state, not configuration
        "metadata.json",            // capture metadata written by the runtime
        "package.json",             // embedded web frontend asset
        "manifest.json",            // web asset manifest
        "Cargo.toml",               // project manifest probing in task workdirs
        ".snapshot.json",           // runtime session snapshot state
        "capability_packages.json", // agent capability data artifact, not configuration
        "pending_revision.json",    // pending plan-revision state written by the headless
        // runtime, not configuration
        "pending-start.json", // crash-recovery seed for a Phase 1 attempt, not configuration
        "settings.json",      // EXTERNAL provider settings (e.g. Claude Code's
        // .claude/settings.json) written by `agent enable` — not a .libra surface
        "file_history.json", // legacy AI file-undo manifest, persisted state rather than configuration
        "redaction_report.json", // E4 checkpoint sidecar (rule-hit stats only), not configuration
    ];

    /// Extract config-file name literals from the PRODUCTION half of one
    /// source file: both direct `join("<name>.toml|json")` calls and quoted
    /// `"<name>.toml|json"` literals on non-comment lines (the latter catches
    /// `const SANDBOX_CONFIG_FILE: &str = "sandbox.toml"`-style indirection).
    /// Rust convention in this tree keeps the test module at the bottom
    /// behind `#[cfg(test)]`; fixture names invented by tests must not force
    /// allowlist entries.
    ///
    /// KNOWN LIMITATION (accepted, documented): a read assembled at runtime
    /// (`format!`, path push of a variable) or a multi-segment literal other
    /// than the registered ones is invisible to this scan — the guard is a
    /// tripwire for the overwhelmingly common literal form, not a parser.
    fn scan_one(source: &str, found: &mut BTreeSet<String>) {
        let production = source.split("#[cfg(test)]").next().unwrap_or_default();
        for line in production.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue; // comments name files without reading them
            }
            for chunk in trimmed.split('"').skip(1).step_by(2) {
                let file_shaped = !chunk.contains('/')
                    && !chunk.contains(' ') // prose mentioning a file, not a filename
                    && !chunk.contains('{') // format!-template state filenames
                    && chunk != ".toml"
                    && chunk != ".json" // bare extension-suffix literals
                    && (chunk.ends_with(".toml") || chunk.ends_with(".json"));
                if file_shaped {
                    found.insert(chunk.to_string());
                }
            }
        }
    }

    fn scan_sources(dir: &Path, found: &mut BTreeSet<String>) {
        for entry in fs::read_dir(dir).expect("source dir must be readable") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                scan_sources(&path, found);
                continue;
            }
            if path.extension().is_none_or(|ext| ext != "rs") {
                continue;
            }
            let source = fs::read_to_string(&path).expect("source file must be readable");
            scan_one(&source, found);
        }
    }

    /// §C.4.1.1 register guard: a `.libra` config-file read appearing in the
    /// Code/Agent namespaces without a registry row (or a reviewed allowlist
    /// entry) fails this test. This is the fail-closed half of "新增 mutable
    /// state/cache 未登记则测试失败".
    /// Namespaces and entrypoint files hosting Code/Agent configuration reads
    /// or process-local cells (the retired TUI module was removed in W5-03;
    /// its config reads died with it). `src/main.rs` owns private helper test
    /// controls and must remain in the cache inventory scan.
    const SCANNED_NAMESPACES: &[&str] = &[
        "src/internal/ai",
        "src/command/agent",
        "src/command/automation.rs",
        "src/main.rs",
    ];

    /// A scanned cache identity. Static identifiers are scoped to their
    /// module, so a source path is required to distinguish two independent
    /// `CONTROLS` or `PAUSE` cells.
    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
    struct ProcessCacheKey {
        source_path: PathBuf,
        static_name: String,
    }

    impl ProcessCacheKey {
        fn new(source_path: impl Into<PathBuf>, static_name: impl Into<String>) -> Self {
            Self {
                source_path: source_path.into(),
                static_name: static_name.into(),
            }
        }
    }

    fn inventory_process_cache_keys() -> BTreeSet<ProcessCacheKey> {
        CODE_AGENT_PROCESS_CACHES
            .iter()
            .map(|surface| ProcessCacheKey::new(surface.source_path, surface.static_name))
            .collect()
    }

    fn extract_process_cache_statics(
        source_path: &Path,
        source: &str,
        found: &mut BTreeSet<ProcessCacheKey>,
    ) {
        for line in source.lines() {
            let trimmed = line.trim_start();
            // Every visibility spelling: `static`, `pub static`,
            // `pub(crate) static`, `pub(super) static`, ...
            let after_vis = trimmed
                .strip_prefix("pub")
                .map(|rest| {
                    rest.strip_prefix('(')
                        .and_then(|inner| inner.split_once(')'))
                        .map(|(_, tail)| tail)
                        .unwrap_or(rest)
                        .trim_start()
                })
                .unwrap_or(trimmed);
            let Some(rest) = after_vis.strip_prefix("static ") else {
                continue;
            };
            let Some((name, ty)) = rest.split_once(':') else {
                continue;
            };
            if ["OnceLock", "LazyLock", "RwLock", "Mutex", "Once", "Lazy"]
                .iter()
                .any(|marker| ty.contains(marker))
            {
                found.insert(ProcessCacheKey::new(source_path, name.trim().to_string()));
            }
        }
    }

    fn scan_process_cache_statics(
        manifest_dir: &Path,
        path: &Path,
        found: &mut BTreeSet<ProcessCacheKey>,
    ) {
        if path.is_dir() {
            for entry in fs::read_dir(path).expect("source dir must be readable") {
                scan_process_cache_statics(manifest_dir, &entry.expect("dir entry").path(), found);
            }
            return;
        }
        if path.extension().is_none_or(|ext| ext != "rs") {
            return;
        }
        let relative_path = path
            .strip_prefix(manifest_dir)
            .expect("scanned source must remain inside the repository root");
        extract_process_cache_statics(
            relative_path,
            &fs::read_to_string(path).expect("readable source"),
            found,
        );
    }

    fn scanned_process_cache_statics(manifest_dir: &Path) -> BTreeSet<ProcessCacheKey> {
        let mut statics = BTreeSet::new();
        for namespace in SCANNED_NAMESPACES {
            scan_process_cache_statics(manifest_dir, &manifest_dir.join(namespace), &mut statics);
        }
        statics
    }

    #[test]
    fn every_code_agent_config_file_read_is_registered() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut found = BTreeSet::new();
        for namespace in SCANNED_NAMESPACES {
            let path = manifest_dir.join(namespace);
            if path.is_dir() {
                scan_sources(&path, &mut found);
            } else {
                let source = fs::read_to_string(&path).expect("source file must be readable");
                scan_one(&source, &mut found);
            }
        }
        assert!(
            found.contains("config.toml"),
            "scan self-check: the known config.toml read must be visible; \
             an empty scan means the guard is broken, not that the tree is clean"
        );

        let registered: BTreeSet<&str> = CODE_AGENT_CONFIG_OWNERSHIP
            .iter()
            .filter(|surface| surface.kind == SurfaceKind::File)
            .map(|surface| surface.location)
            .collect();
        for name in &found {
            let known =
                registered.contains(name.as_str()) || NON_CONFIG_ALLOWLIST.contains(&name.as_str());
            assert!(
                known,
                "unregistered Code/Agent config surface '{name}': add a row to \
                 CODE_AGENT_CONFIG_OWNERSHIP (plan-20260714 §C.4.1.1) or, if it \
                 is not configuration, to NON_CONFIG_ALLOWLIST with a comment"
            );
        }
    }

    /// Reverse guard: registered rows must not be fiction — every registered
    /// file surface must appear as a NON-COMMENT string literal (including
    /// path-shaped and const-assigned forms) in the scanned namespaces plus
    /// publish.rs. Raw whole-file `contains` would be satisfied by doc
    /// comments naming a file no code reads any more.
    #[test]
    fn registered_workdir_surfaces_exist_in_source() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));

        fn literals_of(source: &str, found: &mut BTreeSet<String>) {
            let production = source.split("#[cfg(test)]").next().unwrap_or_default();
            for line in production.lines() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") {
                    continue;
                }
                for chunk in trimmed.split('"').skip(1).step_by(2) {
                    if chunk.ends_with(".toml") || chunk.ends_with(".json") {
                        found.insert(chunk.to_string());
                    }
                }
            }
        }
        fn collect(dir: &Path, found: &mut BTreeSet<String>) {
            for entry in fs::read_dir(dir).expect("source dir must be readable") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    collect(&path, found);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    literals_of(&fs::read_to_string(&path).expect("readable source"), found);
                }
            }
        }

        let mut literals = BTreeSet::new();
        for namespace in SCANNED_NAMESPACES {
            let path = manifest_dir.join(namespace);
            if path.is_dir() {
                collect(&path, &mut literals);
            } else {
                literals_of(
                    &fs::read_to_string(&path).expect("readable source"),
                    &mut literals,
                );
            }
        }

        for surface in CODE_AGENT_CONFIG_OWNERSHIP {
            if surface.kind != SurfaceKind::File {
                continue;
            }
            let name = surface
                .location
                .rsplit('/')
                .next()
                .expect("file locations are non-empty");
            assert!(
                literals.iter().any(|lit| lit.ends_with(name)),
                "registry row '{}' names '{name}' but no NON-COMMENT string \
                 literal in the scanned sources mentions it — stale registry \
                 rows hide real drift",
                surface.surface
            );
        }
    }

    /// Structural reverse coverage for the NON-file surface kinds — every
    /// declared kind is checked against the artifact that would prove it
    /// real:
    /// - `Directory` rows must be read via a `join("<name>")` in the
    ///   scanned Code/Agent namespaces;
    /// - `Store` rows naming database tables must appear in the SQL corpus
    ///   (bootstrap + migrations), and in-memory stores must name a type
    ///   that exists in the AI sources.
    ///
    /// Forward enforcement for new directories/tables remains a review
    /// obligation (documented in the module header); this guard ensures the
    /// REGISTERED rows can never silently rot.
    #[test]
    fn registered_directory_and_store_surfaces_exist_structurally() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));

        let mut ai_sources = String::new();
        fn collect_sources(dir: &Path, into: &mut String) {
            for entry in fs::read_dir(dir).expect("source dir must be readable") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    collect_sources(&path, into);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    into.push_str(&fs::read_to_string(&path).expect("readable source"));
                }
            }
        }
        for namespace in SCANNED_NAMESPACES {
            let path = manifest_dir.join(namespace);
            if path.is_dir() {
                collect_sources(&path, &mut ai_sources);
            } else {
                ai_sources.push_str(&fs::read_to_string(&path).expect("readable source"));
            }
        }

        let mut sql_corpus = String::new();
        fn collect_sql(dir: &Path, into: &mut String) {
            for entry in fs::read_dir(dir).expect("sql dir must be readable") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    collect_sql(&path, into);
                } else if path.extension().is_some_and(|ext| ext == "sql") {
                    into.push_str(&fs::read_to_string(&path).expect("readable sql"));
                }
            }
        }
        collect_sql(&manifest_dir.join("sql"), &mut sql_corpus);

        for surface in CODE_AGENT_CONFIG_OWNERSHIP {
            match surface.kind {
                SurfaceKind::File => {}
                SurfaceKind::Directory => {
                    let needle = format!("join(\"{}\")", surface.location);
                    assert!(
                        ai_sources.contains(&needle),
                        "directory registry row '{}' ({}) has no {needle} read \
                         in the scanned namespaces — stale rows hide real drift",
                        surface.surface,
                        surface.location
                    );
                }
                SurfaceKind::Store => {
                    for token in surface.location.split('/') {
                        let in_sql = sql_corpus.contains(token);
                        let in_sources = ai_sources.contains(token);
                        assert!(
                            in_sql || in_sources,
                            "store registry row '{}' names '{token}', which \
                             appears in neither the SQL corpus nor the AI \
                             sources — stale rows hide real drift",
                            surface.surface
                        );
                    }
                }
            }
        }
    }

    /// FORWARD enforcement for tables and process caches (§C.4.1.1 "新增
    /// mutable state/cache 未登记则测试失败"): every `CREATE TABLE` in the
    /// SQL corpus with a Code/Agent prefix must be classified in
    /// `CODE_AGENT_TABLE_OWNERSHIP`, and every `static` sync/cache cell in
    /// the Code/Agent namespaces must appear in
    /// `CODE_AGENT_PROCESS_CACHES` — a new table or cache cannot ship
    /// unregistered. Both directions: classified rows must also still
    /// exist (no rot).
    #[test]
    fn every_code_agent_table_and_cache_is_classified() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));

        // ---- Tables ----
        let mut sql_corpus = String::new();
        fn collect_sql(dir: &Path, into: &mut String) {
            for entry in fs::read_dir(dir).expect("sql dir must be readable") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    collect_sql(&path, into);
                } else if path.extension().is_some_and(|ext| ext == "sql") {
                    into.push_str(&fs::read_to_string(&path).expect("readable sql"));
                }
            }
        }
        collect_sql(&manifest_dir.join("sql"), &mut sql_corpus);
        let mut created: BTreeSet<String> = BTreeSet::new();
        let lowered = sql_corpus.to_lowercase();
        for chunk in lowered.split("create table").skip(1) {
            let name = chunk
                .trim_start()
                .strip_prefix("if not exists")
                .unwrap_or(chunk)
                .trim_start()
                .trim_start_matches(['`', '"'])
                .chars()
                .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')
                .collect::<String>();
            if !name.is_empty() {
                created.insert(name);
            }
        }
        let classified: BTreeSet<&str> = CODE_AGENT_TABLE_OWNERSHIP
            .iter()
            .map(|(name, _)| *name)
            .collect();
        for table in &created {
            let code_agent = ["agent_", "automation_", "approved_", "ai_", "source_"]
                .iter()
                .any(|prefix| table.starts_with(prefix));
            if code_agent {
                assert!(
                    classified.contains(table.as_str()),
                    "new Code/Agent table '{table}' is not classified in \
                     CODE_AGENT_TABLE_OWNERSHIP (plan-20260714 §C.4.1.1 / line 2272)"
                );
            }
        }
        for (table, _) in CODE_AGENT_TABLE_OWNERSHIP {
            assert!(
                created.contains(*table),
                "classified table '{table}' no longer exists in the SQL corpus — \
                 stale classification rows hide real drift"
            );
        }

        // ---- Process caches ----
        // Scanned over WHOLE files (no `#[cfg(test)]` split): several AI
        // sources carry cfg-gated items mid-file, and missing a production
        // static is the real risk; a test-only static costing an inventory
        // row is noise worth paying.
        let statics = scanned_process_cache_statics(manifest_dir);
        // Scanner SELF-TEST: a known live static must be visible — an empty
        // or broken scan must fail here, not silently pass the inventory.
        assert!(
            statics.contains(&ProcessCacheKey::new(
                "src/internal/ai/history.rs",
                "CLEANUP_HELPER_REAPER",
            )),
            "static scanner self-check failed: the known CLEANUP_HELPER_REAPER \
             declaration was not found"
        );
        let cache_inventory = inventory_process_cache_keys();
        assert_eq!(
            cache_inventory.len(),
            CODE_AGENT_PROCESS_CACHES.len(),
            "duplicate repository-relative process-cache inventory key hides a stale row"
        );
        for cache in &statics {
            assert!(
                cache_inventory.contains(cache),
                "new Code/Agent process static '{}::{}' is not in \
                 CODE_AGENT_PROCESS_CACHES — classify its key discipline \
                 (plan-20260714 §C.4.1.1 process-cache rules)",
                cache.source_path.display(),
                cache.static_name,
            );
        }
        for cache in &cache_inventory {
            assert!(
                statics.contains(cache),
                "inventoried process static '{}::{}' no longer exists — \
                 stale inventory rows hide real drift",
                cache.source_path.display(),
                cache.static_name,
            );
        }
    }

    /// Regression guard for duplicate Rust static identifiers: `CONTROLS`
    /// and `PAUSE` occur in independent modules, and each must retain its own
    /// repository-relative inventory key. A name-only `BTreeSet` would let
    /// one entry make all of these cells appear classified.
    #[test]
    fn process_cache_inventory_distinguishes_duplicate_static_names_by_source_path() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let statics = scanned_process_cache_statics(manifest_dir);
        let inventory = inventory_process_cache_keys();

        let expected = [
            ("src/main.rs", "CONTROLS"),
            ("src/command/agent/import.rs", "CONTROLS"),
            ("src/command/agent/doctor.rs", "PAUSE"),
            (
                "src/internal/ai/observed_agents/builtin/stable_promoted.rs",
                "PAUSE",
            ),
            (
                "src/internal/ai/observed_agents/transcript_source.rs",
                "PAUSE",
            ),
        ];
        for (source_path, static_name) in expected {
            let key = ProcessCacheKey::new(source_path, static_name);
            assert!(
                statics.contains(&key),
                "duplicate static scanner missed {source_path}::{static_name}"
            );
            assert!(
                inventory.contains(&key),
                "duplicate static inventory missed {source_path}::{static_name}"
            );
        }
        assert_eq!(
            statics
                .iter()
                .filter(|cache| cache.static_name == "CONTROLS")
                .count(),
            2,
            "both independent CONTROLS cells must retain distinct inventory keys"
        );
        assert_eq!(
            statics
                .iter()
                .filter(|cache| cache.static_name == "PAUSE")
                .count(),
            3,
            "all independent PAUSE cells must retain distinct inventory keys"
        );
    }

    /// The registry itself stays well-formed: unique locations, and the two
    /// security-critical rows (sandbox, hooks) keep their documented
    /// resolutions — a silent flip of either is exactly the brain-split this
    /// inventory exists to surface.
    #[test]
    fn registry_is_well_formed_and_pins_security_rows() {
        let mut seen = BTreeSet::new();
        for surface in CODE_AGENT_CONFIG_OWNERSHIP {
            assert!(
                seen.insert(surface.location),
                "duplicate registry location {}",
                surface.location
            );
        }
        let by_location = |loc: &str| {
            CODE_AGENT_CONFIG_OWNERSHIP
                .iter()
                .find(|surface| surface.location == loc)
                .unwrap_or_else(|| panic!("registry must keep the {loc} row"))
        };
        assert_eq!(
            by_location("sandbox.toml").resolution,
            ReadResolution::UnifiedResolver,
            "W4-11 sandbox reads go through the unified resolver — flipping \
             this row reopens the linked-worktree brain-split"
        );
        assert_eq!(
            by_location("hooks.json").resolution,
            ReadResolution::UnifiedResolver,
            "W4-11 hooks read through the unified resolver so repository \
             PreToolUse Block stays visible in every worktree"
        );
        assert_eq!(
            by_location("config.toml").resolution,
            ReadResolution::UnifiedResolver,
            "W4-11 [approval] in config.toml uses the unified resolver"
        );
        let location = "automations.toml";
        assert_eq!(
            by_location(location).resolution,
            ReadResolution::UnifiedResolver,
            "W4-12 extension surface '{location}' uses the unified resolver"
        );
        assert_eq!(
            by_location(location).consumer,
            ConfigConsumerKind::Extension,
            "W4-12 must not reclassify '{location}' away from Extension"
        );
        assert_eq!(
            by_location("sandbox.toml").consumer,
            ConfigConsumerKind::Security,
            "W4-12 must not reclassify sandbox.toml away from Security"
        );
    }
}
