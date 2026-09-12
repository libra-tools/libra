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
    /// Agents, automations, prompt rules/contexts, skills, commands, MCP config.
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

/// The §C.4.1.1 registry. Rows mirror plan-20260714 lines 2268/2272/2274.
pub const CODE_AGENT_CONFIG_OWNERSHIP: &[ConfigSurface] = &[
    ConfigSurface {
        surface: "code/provider/MCP configuration ([mcp] sources included; \
                  [approval] is security-sensitive — W4-06 treats the whole \
                  file as Security so overlays cannot wholesale weaken approval; \
                  W4-11/W4-12 section-merge MCP vs approval)",
        location: "config.toml",
        kind: SurfaceKind::File,
        owner: ConfigOwner::RepositoryWithOptionalOverlay,
        resolution: ReadResolution::UnifiedResolver,
        consumer: ConfigConsumerKind::Security,
    },
    ConfigSurface {
        surface: "agent definitions",
        location: "agents.toml",
        kind: SurfaceKind::File,
        owner: ConfigOwner::RepositoryWithOptionalOverlay,
        resolution: ReadResolution::UnifiedResolver,
        consumer: ConfigConsumerKind::Extension,
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
        surface: "prompt rules (security-sensitive prompt policy; repository \
                  layer must remain visible in linked worktrees)",
        location: "rules",
        kind: SurfaceKind::Directory,
        owner: ConfigOwner::RepositoryWithOptionalOverlay,
        resolution: ReadResolution::UnifiedResolver,
        consumer: ConfigConsumerKind::Security,
    },
    ConfigSurface {
        surface: "prompt contexts (security-sensitive prompt policy; repository \
                  layer must remain visible in linked worktrees)",
        location: "contexts",
        kind: SurfaceKind::Directory,
        owner: ConfigOwner::RepositoryWithOptionalOverlay,
        resolution: ReadResolution::UnifiedResolver,
        consumer: ConfigConsumerKind::Security,
    },
    ConfigSurface {
        surface: "agent definitions directory",
        location: "agents",
        kind: SurfaceKind::Directory,
        owner: ConfigOwner::RepositoryWithOptionalOverlay,
        resolution: ReadResolution::UnifiedResolver,
        consumer: ConfigConsumerKind::Extension,
    },
    ConfigSurface {
        surface: "custom commands directory",
        location: "commands",
        kind: SurfaceKind::Directory,
        owner: ConfigOwner::RepositoryWithOptionalOverlay,
        resolution: ReadResolution::UnifiedResolver,
        consumer: ConfigConsumerKind::Extension,
    },
    ConfigSurface {
        surface: "skills directory",
        location: "skills",
        kind: SurfaceKind::Directory,
        owner: ConfigOwner::RepositoryWithOptionalOverlay,
        resolution: ReadResolution::UnifiedResolver,
        consumer: ConfigConsumerKind::Extension,
    },
    ConfigSurface {
        surface: "executable VCS hooks directory (plan line 2272: shared \
                  repository semantics like Git hooks; does not migrate \
                  with this Part)",
        location: "hooks",
        kind: SurfaceKind::Directory,
        owner: ConfigOwner::Repository,
        resolution: ReadResolution::CommonStorage,
        consumer: ConfigConsumerKind::Security,
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
    ConfigSurface {
        surface: "publish worker-template manifest (init/status/deploy drift \
                  gate reads ONE repository manifest)",
        location: "publish/worker-template-manifest.json",
        kind: SurfaceKind::File,
        owner: ConfigOwner::Repository,
        resolution: ReadResolution::CommonStorage,
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
/// cell in the Code/Agent namespaces, with its key discipline. The forward
/// guard scans for `static NAME: …OnceLock|LazyLock|Mutex|RwLock` in those
/// namespaces and fails when a new one is missing here.
pub const CODE_AGENT_PROCESS_CACHES: &[(&str, &str)] = &[
    (
        "WORKSPACE_CONTEXT_CACHE",
        "keyed by canonical workspace PathBuf — per-workdir, no cross-scope reuse",
    ),
    (
        "HISTORY_APPEND_LOCK",
        "a lock, not a cache: serializes history appends within one process",
    ),
    (
        "CLEANUP_HELPER_REAPER",
        "process-lifetime reaper thread handle; holds no repository state",
    ),
    (
        "MACOS_FUSE_MOUNT_HANDSHAKE_LOCK",
        "a lock, not a cache: serializes FUSE mount handshakes",
    ),
    (
        "BODY",
        "compiled regex/template constant; input-independent",
    ),
    (
        "DEFAULT_RULES",
        "compiled built-in redaction rule set; input-independent constant",
    ),
    (
        "OPTS",
        "compiled parser options constant; input-independent",
    ),
    ("REG", "compiled regex constant; input-independent"),
    (
        "CURRENT_PROCESS_OWNER_IDENTITY",
        "process-lifetime own pid/starttime/boot_id identity for the session \
         writer-lease liveness probe; holds no repository state",
    ),
    (
        "BRIDGE_LIVE_REVIEW_RUNS",
        "not a cache: the live-run registry `libra agent bridge --stdio` uses to \
         cancel and drain the review runs IT started when the stdio loop ends \
         (GC-LB-10). Keyed by run id, holds only cancel/join handles for this \
         process's own runs, and is emptied by the shutdown drain",
    ),
];

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, fs, path::Path};

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
        // runtime (web/headless.rs), not configuration
        "pending-start.json", // crash-recovery seed for a Phase 1 attempt, not configuration
        "settings.json",      // EXTERNAL provider settings (e.g. Claude Code's
                              // .claude/settings.json) written by `agent enable` — not a .libra surface
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
    /// Namespaces hosting Code/Agent configuration reads (the retired TUI
    /// module was removed in W5-03; its config reads died with it);
    /// `agent`/`code_control` are the remaining runtime command surfaces.
    const SCANNED_NAMESPACES: &[&str] = &[
        "src/internal/ai",
        "src/command/agent",
        "src/command/code.rs",
        "src/command/code_control.rs",
        "src/command/automation.rs",
    ];

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
        literals_of(
            &fs::read_to_string(manifest_dir.join("src/command/publish.rs")).unwrap(),
            &mut literals,
        );

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
        fn extract_statics(source: &str, found: &mut BTreeSet<String>) {
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
                    found.insert(name.trim().to_string());
                }
            }
        }
        let mut statics: BTreeSet<String> = BTreeSet::new();
        fn scan_statics(dir: &Path, found: &mut BTreeSet<String>) {
            for entry in fs::read_dir(dir).expect("source dir must be readable") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    scan_statics(&path, found);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    extract_statics(&fs::read_to_string(&path).expect("readable source"), found);
                }
            }
        }
        for namespace in SCANNED_NAMESPACES {
            let path = manifest_dir.join(namespace);
            if path.is_dir() {
                scan_statics(&path, &mut statics);
            } else {
                extract_statics(
                    &fs::read_to_string(&path).expect("readable source"),
                    &mut statics,
                );
            }
        }
        // Scanner SELF-TEST: a known live static must be visible — an empty
        // or broken scan must fail here, not silently pass the inventory.
        assert!(
            statics.contains("WORKSPACE_CONTEXT_CACHE"),
            "static scanner self-check failed: the known WORKSPACE_CONTEXT_CACHE \
             declaration was not found"
        );
        let cache_inventory: BTreeSet<&str> = CODE_AGENT_PROCESS_CACHES
            .iter()
            .map(|(name, _)| *name)
            .collect();
        for name in &statics {
            assert!(
                cache_inventory.contains(name.as_str()),
                "new Code/Agent process static '{name}' is not in \
                 CODE_AGENT_PROCESS_CACHES — classify its key discipline \
                 (plan-20260714 §C.4.1.1 process-cache rules)"
            );
        }
        for (name, _) in CODE_AGENT_PROCESS_CACHES {
            assert!(
                statics.contains(*name),
                "inventoried process static '{name}' no longer exists — \
                 stale inventory rows hide real drift"
            );
        }
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
            "W4-11 [approval]/[mcp] in config.toml use the unified resolver"
        );
        assert_eq!(
            by_location("rules").resolution,
            ReadResolution::UnifiedResolver,
            "W4-11 rules directory uses the unified resolver"
        );
        assert_eq!(
            by_location("contexts").resolution,
            ReadResolution::UnifiedResolver,
            "W4-11 contexts directory uses the unified resolver"
        );
        for location in [
            "agents.toml",
            "automations.toml",
            "agents",
            "commands",
            "skills",
        ] {
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
        }
        assert_eq!(
            by_location("sandbox.toml").consumer,
            ConfigConsumerKind::Security,
            "W4-12 must not reclassify sandbox.toml away from Security"
        );
        assert_eq!(
            by_location("publish/worker-template-manifest.json").resolution,
            ReadResolution::CommonStorage,
            "the publish manifest was routed through the common-storage \
             resolver in W0 — regressing to a workdir join reopens the \
             per-worktree manifest fork"
        );
    }
}
