//! W4-06/W4-11/W4-12: Code/Agent config resolver + security/extension loaders.
//!
//! L1 — deterministic. Linked-worktree launch enablement lives in
//! `code_agent_linked_guard_test.rs` (W4-08).

use std::{fs, path::Path};

use libra::{
    internal::{
        ai::{
            agent::profile::{AgentsConfig, load_profiles},
            automation::AutomationConfig,
            commands::load_commands,
            hooks::{HookEvent, load_hook_config},
            prompt::{ContextMode, RuleCategory, load_rule},
            sandbox::{
                load_approval_project_config, load_sandbox_config_network_access,
                load_sandbox_deny_read_paths,
            },
            skills::load_skills,
            sources::{
                BUILTIN_MCP_SOURCE_SLUG, SourceEnablement,
                resolver::{
                    ConfigLayer, ConfigResolveError, resolve_config_dir, resolve_config_file,
                    surface_by_location,
                },
                source_config_view_from_project_config,
            },
        },
        config_ownership::{
            CODE_AGENT_CONFIG_OWNERSHIP, ConfigConsumerKind, ConfigOwner, ReadResolution,
            SurfaceKind,
        },
        worktree_scope::{RequestScope, WorktreeScope},
    },
    utils::test::ChangeDirGuard,
};

use super::{assert_cli_success, run_libra_command};

fn request_for(workdir: &Path) -> RequestScope {
    RequestScope::resolve(workdir.to_path_buf()).expect("RequestScope for workdir")
}

fn repo_with_linked_worktree() -> (tempfile::TempDir, tempfile::TempDir) {
    let repo = tempfile::tempdir().expect("repo");
    let p = repo.path();
    assert_cli_success(&run_libra_command(&["init", "--vault=false"], p), "init");
    assert_cli_success(&run_libra_command(&["config", "user.name", "t"], p), "name");
    assert_cli_success(
        &run_libra_command(&["config", "user.email", "t@t"], p),
        "email",
    );
    fs::write(p.join("a.txt"), "a\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit",
    );
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], p),
        "worktree add",
    );
    (repo, parent)
}

#[test]
fn code_agent_config_resolver_scope_precedence() {
    let (repo, parent) = repo_with_linked_worktree();
    // Provenance reports paths as the worktree registry pinned them, i.e.
    // canonical. `tempfile` hands back the caller's spelling, and on stock
    // macOS that still contains `/var -> private/var`, so anchor every
    // expectation below on the canonical form (same convention as
    // `worktree_test` / `rev_parse_test`).
    let main = repo
        .path()
        .canonicalize()
        .expect("canonical repository path");
    let main = main.as_path();
    let wt = parent
        .path()
        .join("wt")
        .canonicalize()
        .expect("canonical linked worktree path");
    let main_request = request_for(main);
    let linked_request = request_for(&wt);

    let repo_sandbox = main.join(".libra").join("sandbox.toml");
    fs::write(&repo_sandbox, "[sandbox.network]\nmode = \"deny\"\n")
        .expect("write repository sandbox.toml");

    assert_eq!(main_request.scope, WorktreeScope::Main);
    let main_resolved = resolve_config_file(&main_request, "sandbox.toml").expect("main sandbox");
    assert_eq!(
        main_resolved.provenance.winning_layer,
        ConfigLayer::Repository
    );
    assert_eq!(
        main_resolved.provenance.consumer,
        ConfigConsumerKind::Security
    );
    assert!(main_resolved.overlay_bytes.is_none());
    assert!(
        String::from_utf8_lossy(&main_resolved.bytes).contains("deny"),
        "main resolves repository bytes"
    );

    assert!(linked_request.scope.is_linked());
    let linked_gitdir = wt.join(".libra");
    let overlay_sandbox = linked_gitdir.join("sandbox.toml");
    fs::write(&overlay_sandbox, "[sandbox.network]\nmode = \"full\"\n")
        .expect("write overlay sandbox that would loosen policy");

    let linked_resolved =
        resolve_config_file(&linked_request, "sandbox.toml").expect("linked sandbox");
    assert_eq!(
        linked_resolved.provenance.winning_layer,
        ConfigLayer::RepositoryWithTighteningOverlay
    );
    assert_eq!(
        linked_resolved.provenance.overlay_path.as_deref(),
        Some(overlay_sandbox.as_path())
    );
    assert!(
        String::from_utf8_lossy(&linked_resolved.bytes).contains("deny"),
        "effective bytes stay repository (never overlay-only replace)"
    );
    assert!(
        linked_resolved
            .overlay_bytes
            .as_ref()
            .is_some_and(|b| String::from_utf8_lossy(b).contains("full")),
        "security overlay bytes must be exposed for W4-11 tighten-only merge"
    );

    // Absent security repository layer is an empty default base (sandbox loader
    // maps NotFound → SandboxConfigFile::default); overlay still exposed for
    // tighten-only composition in W4-11.
    fs::remove_file(&repo_sandbox).expect("remove repository sandbox");
    let absent = resolve_config_file(&linked_request, "sandbox.toml").expect("absent ok");
    assert!(
        absent.bytes.is_empty() && absent.repository_bytes.is_empty(),
        "absence yields empty default base"
    );
    assert_eq!(
        absent.provenance.winning_layer,
        ConfigLayer::RepositoryWithTighteningOverlay
    );
    assert!(
        absent
            .overlay_bytes
            .as_ref()
            .is_some_and(|b| String::from_utf8_lossy(b).contains("full")),
        "overlay bytes remain visible when repository file is absent"
    );
    fs::write(&repo_sandbox, "[sandbox.network]\nmode = \"deny\"\n")
        .expect("restore repository sandbox after absence check");

    #[cfg(unix)]
    {
        use std::os::unix::fs::{PermissionsExt, symlink};
        fs::set_permissions(&repo_sandbox, fs::Permissions::from_mode(0o000))
            .expect("chmod 000 sandbox.toml");
        let unreadable = resolve_config_file(&linked_request, "sandbox.toml");
        match unreadable {
            Err(ConfigResolveError::SecurityRepositoryUnreadable {
                location,
                repository_path,
                message,
            }) => {
                assert_eq!(location, "sandbox.toml");
                assert_eq!(repository_path, repo_sandbox);
                assert!(!message.is_empty());
                let display = ConfigResolveError::SecurityRepositoryUnreadable {
                    location,
                    repository_path: repository_path.clone(),
                    message: message.clone(),
                }
                .to_string();
                assert!(
                    display.contains("unreadable") && display.contains("sandbox.toml"),
                    "got {display}"
                );
            }
            other => panic!("expected SecurityRepositoryUnreadable, got {other:?}"),
        }
        fs::set_permissions(&repo_sandbox, fs::Permissions::from_mode(0o644))
            .expect("restore sandbox perms");

        // Dangling symlink must not look like an absent empty baseline.
        let dangling = parent.path().join("missing-sandbox-target.toml");
        let _ = fs::remove_file(&repo_sandbox);
        symlink(&dangling, &repo_sandbox).expect("dangling sandbox symlink");
        match resolve_config_file(&linked_request, "sandbox.toml") {
            Err(err) => {
                assert!(err.is_fail_closed_security(), "got {err}");
                assert!(
                    err.to_string().contains("unreadable")
                        && err.to_string().contains("sandbox.toml"),
                    "got {err}"
                );
            }
            Ok(ok) => panic!(
                "expected dangling symlink fail-closed, got {:?}",
                ok.provenance
            ),
        }
        fs::remove_file(&repo_sandbox).expect("remove dangling sandbox");
        fs::write(&repo_sandbox, "[sandbox.network]\nmode = \"deny\"\n")
            .expect("restore repository sandbox");
    }

    // Extension surfaces: overlay wins when present.
    let repo_agents = main.join(".libra").join("agents.toml");
    fs::write(&repo_agents, "name = \"repo\"\n").expect("repo agents");
    let overlay_agents = linked_gitdir.join("agents.toml");
    fs::write(&overlay_agents, "name = \"overlay\"\n").expect("overlay agents");
    let agents = resolve_config_file(&linked_request, "agents.toml").expect("agents");
    assert_eq!(agents.provenance.winning_layer, ConfigLayer::Overlay);
    assert_eq!(agents.provenance.consumer, ConfigConsumerKind::Extension);
    assert!(String::from_utf8_lossy(&agents.bytes).contains("overlay"));

    // Absent extension file yields empty repository baseline (load_or_default).
    fs::remove_file(&repo_agents).expect("remove repo agents");
    fs::remove_file(&overlay_agents).expect("remove overlay agents");
    let absent_agents =
        resolve_config_file(&linked_request, "agents.toml").expect("absent agents ok");
    assert!(absent_agents.bytes.is_empty());
    assert_eq!(
        absent_agents.provenance.winning_layer,
        ConfigLayer::Repository
    );
    fs::write(&repo_agents, "name = \"repo\"\n").expect("restore repo agents");
    fs::write(&overlay_agents, "name = \"overlay\"\n").expect("restore overlay agents");

    // Repository-only owner never consults overlay (publish manifest).
    let repo_manifest = main
        .join(".libra")
        .join("publish")
        .join("worker-template-manifest.json");
    fs::create_dir_all(repo_manifest.parent().unwrap()).expect("publish dir");
    fs::write(&repo_manifest, "{\"v\":1}\n").expect("repo manifest");
    let linked_manifest = linked_gitdir
        .join("publish")
        .join("worker-template-manifest.json");
    fs::create_dir_all(linked_manifest.parent().unwrap()).expect("linked publish dir");
    fs::write(&linked_manifest, "{\"v\":99}\n").expect("linked manifest");
    let manifest = resolve_config_file(&linked_request, "publish/worker-template-manifest.json")
        .expect("manifest");
    assert_eq!(manifest.provenance.owner, ConfigOwner::Repository);
    assert!(manifest.provenance.overlay_path.is_none());
    assert_eq!(manifest.provenance.winning_layer, ConfigLayer::Repository);
    assert!(String::from_utf8_lossy(&manifest.bytes).contains("\"v\":1"));

    // Directory surfaces resolve paths for W4-11/W4-12.
    let rules_dir = main.join(".libra").join("rules");
    fs::create_dir_all(&rules_dir).expect("rules dir");
    let overlay_rules = linked_gitdir.join("rules");
    fs::create_dir_all(&overlay_rules).expect("overlay rules");
    let dir = resolve_config_dir(&linked_request, "rules").expect("rules dir");
    assert_eq!(dir.repository_path, rules_dir);
    assert_eq!(dir.provenance.consumer, ConfigConsumerKind::Security);
    assert_eq!(
        dir.provenance.winning_layer,
        ConfigLayer::RepositoryWithTighteningOverlay
    );

    // Absent security directory repository layer is allowed (empty default).
    fs::remove_dir_all(&rules_dir).expect("remove repository rules");
    let absent_rules = resolve_config_dir(&linked_request, "rules").expect("absent rules ok");
    assert_eq!(
        absent_rules.provenance.winning_layer,
        ConfigLayer::RepositoryWithTighteningOverlay
    );
    fs::create_dir_all(&rules_dir).expect("restore rules dir");

    // Security directory that exists but cannot be enumerated must fail-closed
    // (metadata alone is not enough — mode 000 still stats for the owner).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&rules_dir, fs::Permissions::from_mode(0o000))
            .expect("chmod 000 rules");
        match resolve_config_dir(&linked_request, "rules") {
            Err(err) => {
                assert!(err.is_fail_closed_security(), "got {err}");
                assert!(
                    err.to_string().contains("unreadable") && err.to_string().contains("rules"),
                    "got {err}"
                );
            }
            Ok(ok) => panic!(
                "expected unreadable rules fail-closed, got {:?}",
                ok.provenance
            ),
        }
        fs::set_permissions(&rules_dir, fs::Permissions::from_mode(0o755))
            .expect("restore rules perms");
    }

    // Extension overlay unreadable / inaccessible must not silently fall back.
    #[cfg(unix)]
    {
        use std::os::unix::fs::{PermissionsExt, symlink};

        fn assert_overlay_access_error(err: ConfigResolveError, location: &str) {
            match err {
                ConfigResolveError::Paths { message, .. } => {
                    assert!(
                        (message.contains("unreadable") || message.contains("inaccessible"))
                            && message.contains(location),
                        "got {message}"
                    );
                }
                other => panic!("expected Paths for overlay access error, got {other:?}"),
            }
        }

        fs::set_permissions(&overlay_agents, fs::Permissions::from_mode(0o000))
            .expect("chmod 000 overlay agents");
        assert_overlay_access_error(
            resolve_config_file(&linked_request, "agents.toml")
                .expect_err("extension overlay chmod 000"),
            "agents.toml",
        );
        fs::set_permissions(&overlay_agents, fs::Permissions::from_mode(0o644))
            .expect("restore overlay agents");

        // Symlink into a chmod-000 directory: Path::is_file would look like "missing".
        let trap = parent.path().join("trap-ext");
        fs::create_dir(&trap).expect("trap-ext");
        let trap_agents = trap.join("agents.toml");
        fs::write(&trap_agents, "name = \"trap\"\n").expect("trap agents");
        fs::set_permissions(&trap, fs::Permissions::from_mode(0o000)).expect("chmod trap-ext");
        fs::remove_file(&overlay_agents).expect("remove overlay agents");
        symlink(&trap_agents, &overlay_agents).expect("symlink overlay agents");
        assert_overlay_access_error(
            resolve_config_file(&linked_request, "agents.toml")
                .expect_err("extension overlay via inaccessible parent"),
            "agents.toml",
        );
        fs::set_permissions(&trap, fs::Permissions::from_mode(0o755)).expect("unlock trap-ext");
        fs::remove_file(&overlay_agents).expect("remove symlink agents");
        fs::write(&overlay_agents, "name = \"overlay\"\n").expect("restore overlay agents");

        // Security overlay through inaccessible parent must not drop to repository-only.
        fs::write(&repo_sandbox, "[sandbox.network]\nmode = \"deny\"\n")
            .expect("ensure repository sandbox");
        let trap_sec = parent.path().join("trap-sec");
        fs::create_dir(&trap_sec).expect("trap-sec");
        let trap_sandbox = trap_sec.join("sandbox.toml");
        fs::write(&trap_sandbox, "[sandbox.network]\nmode = \"full\"\n").expect("trap sandbox");
        fs::set_permissions(&trap_sec, fs::Permissions::from_mode(0o000)).expect("chmod trap-sec");
        let _ = fs::remove_file(&overlay_sandbox);
        symlink(&trap_sandbox, &overlay_sandbox).expect("symlink overlay sandbox");
        assert_overlay_access_error(
            resolve_config_file(&linked_request, "sandbox.toml")
                .expect_err("security overlay via inaccessible parent"),
            "sandbox.toml",
        );
        fs::set_permissions(&trap_sec, fs::Permissions::from_mode(0o755)).expect("unlock trap-sec");
        fs::remove_file(&overlay_sandbox).expect("remove sandbox symlink");

        // Security directory overlay through inaccessible parent must not look absent.
        // Symlink *to* a mode-000 dir is insufficient (owner can still stat it); symlink
        // *through* a mode-000 parent forces PermissionDenied on metadata follow.
        let trap_rules_parent = parent.path().join("trap-rules-parent");
        let trap_rules_inner = trap_rules_parent.join("rules");
        fs::create_dir_all(&trap_rules_inner).expect("trap-rules-inner");
        fs::set_permissions(&trap_rules_parent, fs::Permissions::from_mode(0o000))
            .expect("chmod trap-rules-parent");
        let _ = fs::remove_dir_all(&overlay_rules);
        let _ = fs::remove_file(&overlay_rules);
        symlink(&trap_rules_inner, &overlay_rules).expect("symlink overlay rules");
        assert_overlay_access_error(
            resolve_config_dir(&linked_request, "rules")
                .expect_err("security rules overlay via inaccessible parent"),
            "rules",
        );
        fs::set_permissions(&trap_rules_parent, fs::Permissions::from_mode(0o755))
            .expect("unlock trap-rules-parent");
        fs::remove_file(&overlay_rules).expect("remove rules symlink");
        fs::create_dir_all(&overlay_rules).expect("restore overlay rules");

        // Wrong-type overlay (directory where a file is required) must not fall back.
        fs::remove_file(&overlay_agents).expect("remove agents file");
        fs::create_dir(&overlay_agents).expect("agents.toml as directory");
        match resolve_config_file(&linked_request, "agents.toml") {
            Err(ConfigResolveError::Paths { message, .. }) => {
                assert!(
                    message.contains("wrong type") && message.contains("agents.toml"),
                    "got {message}"
                );
            }
            other => panic!("expected wrong-type overlay error, got {other:?}"),
        }
        fs::remove_dir(&overlay_agents).expect("remove wrong-type agents dir");
        fs::write(&overlay_agents, "name = \"overlay\"\n").expect("restore overlay agents");
    }

    // Forged RequestScope (scope key disagrees with pinned gitdir) is rejected.
    let mut forged = linked_request.clone();
    forged.scope = WorktreeScope::Main;
    let mismatch = resolve_config_file(&forged, "agents.toml");
    assert!(matches!(
        mismatch,
        Err(ConfigResolveError::ScopeMismatch { .. })
    ));

    // Forged storage (gitdir A + storage B) must not mix repository layers.
    let other_for_forge = tempfile::tempdir().expect("forge other");
    assert_cli_success(
        &run_libra_command(&["init", "--vault=false"], other_for_forge.path()),
        "init forge other",
    );
    let other_request = request_for(other_for_forge.path());
    let mut mixed = main_request.clone();
    mixed.storage = other_request.storage.clone();
    match resolve_config_file(&mixed, "sandbox.toml") {
        Err(ConfigResolveError::Paths { message, .. }) => {
            assert!(
                message.contains("does not match common storage"),
                "got {message}"
            );
        }
        other => panic!("expected mixed-storage rejection, got {other:?}"),
    }

    // Pinned RequestScope must not follow a later cwd move into another Main repo.
    let other = tempfile::tempdir().expect("other repo");
    assert_cli_success(
        &run_libra_command(&["init", "--vault=false"], other.path()),
        "init other",
    );
    fs::write(
        other.path().join(".libra").join("sandbox.toml"),
        "[sandbox.network]\nmode = \"full\"\n",
    )
    .expect("other sandbox");
    let _cwd = ChangeDirGuard::new(other.path());
    let still_main = resolve_config_file(&main_request, "sandbox.toml").expect("pinned main");
    assert_eq!(
        still_main.provenance.repository_path,
        main_request.storage.join("sandbox.toml")
    );
    assert!(
        String::from_utf8_lossy(&still_main.bytes).contains("deny"),
        "must keep repository A policy after cwd moved to B"
    );

    assert_eq!(
        surface_by_location("config.toml").expect("config").consumer,
        ConfigConsumerKind::Security
    );
    assert_eq!(
        surface_by_location("agents.toml").expect("agents").consumer,
        ConfigConsumerKind::Extension
    );
}

#[test]
fn sandbox_and_approval_config_not_split_brained_across_scopes() {
    let (repo, parent) = repo_with_linked_worktree();
    let main = repo.path();
    let wt = parent.path().join("wt");

    fs::write(
        main.join(".libra").join("sandbox.toml"),
        r#"deny_read = ["repo-secret"]
[sandbox.network]
mode = "denied"
"#,
    )
    .expect("repo sandbox");
    fs::write(
        wt.join(".libra").join("sandbox.toml"),
        r#"deny_read = []
[sandbox.network]
mode = "full"
"#,
    )
    .expect("overlay sandbox loosen");

    fs::write(
        main.join(".libra").join("config.toml"),
        r#"[approval]
ttl_seconds = 60
protected_branches = ["main", "release"]
allowed_network_domains = ["github.com"]
no_cache_unknown_network = true

[mcp]
enabled = true
"#,
    )
    .expect("repo approval+mcp");
    fs::write(
        wt.join(".libra").join("config.toml"),
        r#"[approval]
ttl_seconds = 3600
protected_branches = ["develop"]
allowed_network_domains = ["example.com"]
no_cache_unknown_network = false

[mcp]
enabled = false
"#,
    )
    .expect("overlay approval+mcp loosen");

    let main_network = load_sandbox_config_network_access(main)
        .expect("main sandbox")
        .expect("network section");
    let linked_network = load_sandbox_config_network_access(&wt)
        .expect("linked sandbox")
        .expect("network section");
    assert!(
        main_network.is_denied() && linked_network.is_denied(),
        "overlay mode=full must not loosen repository denied network"
    );

    let main_deny = load_sandbox_deny_read_paths(main).expect("main deny");
    let linked_deny = load_sandbox_deny_read_paths(&wt).expect("linked deny");
    assert!(
        main_deny.iter().any(|p| p.ends_with("repo-secret")),
        "repository deny_read must load on main"
    );
    assert!(
        linked_deny.iter().any(|p| p.ends_with("repo-secret")),
        "overlay cannot drop repository deny_read"
    );

    let main_approval = load_approval_project_config(main).expect("main approval");
    let linked_approval = load_approval_project_config(&wt).expect("linked approval");
    assert_eq!(main_approval.ttl, Some(std::time::Duration::from_secs(60)));
    assert_eq!(
        linked_approval.ttl,
        Some(std::time::Duration::from_secs(60)),
        "overlay cannot lengthen approval TTL"
    );
    assert!(
        linked_approval
            .cache_policy
            .protected_branches
            .iter()
            .any(|b| b == "main")
            && linked_approval
                .cache_policy
                .protected_branches
                .iter()
                .any(|b| b == "release"),
        "overlay cannot drop repository protected branches"
    );
    assert!(
        linked_approval.cache_policy.no_cache_unknown_network,
        "overlay cannot clear no_cache_unknown_network"
    );
    assert_eq!(
        linked_approval.cache_policy.allowed_network_domains,
        Vec::<String>::new(),
        "disjoint overlay allowlist intersects to empty (tighter)"
    );

    let mcp = source_config_view_from_project_config(&wt).expect("mcp view");
    let entry = mcp
        .source(BUILTIN_MCP_SOURCE_SLUG)
        .expect("builtin mcp present");
    assert!(
        entry.enablement.is_enabled() && entry.enablement == SourceEnablement::ProjectConfig,
        "overlay cannot disable repository-enabled builtin MCP"
    );
}

#[test]
fn approval_overlay_empty_allowlist_tightens_to_empty() {
    let (repo, parent) = repo_with_linked_worktree();
    let main = repo.path();
    let wt = parent.path().join("wt");

    fs::write(
        main.join(".libra").join("config.toml"),
        r#"[approval]
allowed_network_domains = ["github.com"]
no_cache_unknown_network = true
"#,
    )
    .expect("repo approval allowlist");
    fs::write(
        wt.join(".libra").join("config.toml"),
        r#"[approval]
allowed_network_domains = []
"#,
    )
    .expect("overlay explicit empty allowlist");

    let main_approval = load_approval_project_config(main).expect("main approval");
    let linked_approval = load_approval_project_config(&wt).expect("linked approval");
    assert_eq!(
        main_approval.cache_policy.allowed_network_domains,
        vec!["github.com".to_string()]
    );
    assert!(
        linked_approval
            .cache_policy
            .allowed_network_domains
            .is_empty(),
        "explicit empty overlay allowlist must revoke repository cached-network domains"
    );
    assert!(
        linked_approval.cache_policy.no_cache_unknown_network,
        "overlay omission must not clear repository no_cache_unknown_network"
    );
}

#[test]
fn approval_overlay_cannot_loosen_default_ttl_or_protected_branches() {
    let (repo, parent) = repo_with_linked_worktree();
    let main = repo.path();
    let wt = parent.path().join("wt");

    fs::write(
        main.join(".libra").join("config.toml"),
        "[mcp]\nenabled = true\n",
    )
    .expect("repo omits approval section");
    fs::write(
        wt.join(".libra").join("config.toml"),
        r#"[approval]
ttl_seconds = 3600
protected_branches = ["feature"]
"#,
    )
    .expect("overlay longer ttl + replace branches");

    let linked = load_approval_project_config(&wt).expect("linked approval");
    assert_eq!(
        linked.ttl,
        Some(std::time::Duration::from_secs(300)),
        "overlay cannot lengthen the 300s default TTL when repository omits ttl_seconds"
    );
    assert!(
        linked
            .cache_policy
            .protected_branches
            .iter()
            .any(|b| b == "main"),
        "overlay cannot drop default protected branch `main`"
    );
    assert!(
        linked
            .cache_policy
            .protected_branches
            .iter()
            .any(|b| b == "feature"),
        "overlay may add protected branches"
    );
}

#[test]
fn repository_layer_hooks_visible_in_linked_worktree() {
    let (repo, parent) = repo_with_linked_worktree();
    let main = repo.path();
    let wt = parent.path().join("wt");
    fs::write(
        main.join(".libra").join("hooks.json"),
        r#"{"hooks":[{"event":"pre_tool_use","matcher":"shell","command":"echo repo-block"}]}"#,
    )
    .expect("repo hooks");

    let main_hooks = load_hook_config(main).expect("main hooks");
    let linked_hooks = load_hook_config(&wt).expect("linked hooks");
    assert!(
        main_hooks
            .hooks
            .iter()
            .any(|h| h.event == HookEvent::PreToolUse && h.command.contains("repo-block"))
    );
    assert!(
        linked_hooks
            .hooks
            .iter()
            .any(|h| h.event == HookEvent::PreToolUse && h.command.contains("repo-block")),
        "repository PreToolUse must remain visible in the linked worktree"
    );
}

#[test]
fn hook_overlay_cannot_remove_repository_security_hooks() {
    let (repo, parent) = repo_with_linked_worktree();
    let main = repo.path();
    let wt = parent.path().join("wt");
    fs::write(
        main.join(".libra").join("hooks.json"),
        r#"{"hooks":[{"event":"pre_tool_use","matcher":"shell","command":"echo repo-block","enabled":true}]}"#,
    )
    .expect("repo hooks");
    fs::write(
        wt.join(".libra").join("hooks.json"),
        r#"{"hooks":[{"event":"pre_tool_use","matcher":"shell","command":"echo repo-block","enabled":false},{"event":"post_tool_use","matcher":"*","command":"echo overlay-post"}]}"#,
    )
    .expect("overlay disable + extra");

    let linked = load_hook_config(&wt).expect("linked hooks");
    let pre = linked
        .hooks
        .iter()
        .find(|h| h.event == HookEvent::PreToolUse && h.command.contains("repo-block"))
        .expect("repository PreToolUse must survive overlay disable");
    assert!(
        pre.enabled,
        "overlay enabled=false must not disable repository PreToolUse"
    );
    assert!(
        linked
            .hooks
            .iter()
            .any(|h| h.event == HookEvent::PostToolUse && h.command.contains("overlay-post")),
        "overlay may add non-PreToolUse hooks"
    );
}

#[test]
fn sub_agent_and_task_workdir_hooks_not_split_brained() {
    let (repo, parent) = repo_with_linked_worktree();
    let main = repo.path();
    let wt = parent.path().join("wt");
    fs::write(
        main.join(".libra").join("hooks.json"),
        r#"{"hooks":[{"event":"pre_tool_use","matcher":"*","command":"echo repo-block"}]}"#,
    )
    .expect("repo hooks");

    let subdir = main.join("crates").join("task-src");
    fs::create_dir_all(&subdir).expect("task subdir");
    let fake_task = parent.path().join("isolated-task");
    fs::create_dir_all(fake_task.join(".libra")).expect("fake task libra");
    fs::write(
        fake_task.join(".libra").join("hooks.json"),
        r#"{"hooks":[]}"#,
    )
    .expect("empty fake hooks");

    let from_subdir = load_hook_config(&subdir).expect("subdir hooks");
    let from_linked = load_hook_config(&wt).expect("linked hooks");
    assert!(
        from_subdir
            .hooks
            .iter()
            .any(|h| h.event == HookEvent::PreToolUse && h.command.contains("repo-block")),
        "task cwd inside the repository must still see repository PreToolUse"
    );
    assert!(
        from_linked
            .hooks
            .iter()
            .any(|h| h.event == HookEvent::PreToolUse && h.command.contains("repo-block")),
        "linked worktree task cwd must still see repository PreToolUse"
    );

    let isolated = load_hook_config(&fake_task).expect("isolated fake dir");
    assert!(
        isolated.hooks.is_empty(),
        "a non-repo task dir is not a Libra worktree; executor must reuse the session runner instead of reloading here"
    );
}

#[test]
fn repository_layer_rules_and_contexts_visible_in_linked() {
    let (repo, parent) = repo_with_linked_worktree();
    let main = repo.path();
    let wt = parent.path().join("wt");

    let rules_dir = main.join(".libra").join("rules");
    fs::create_dir_all(&rules_dir).expect("rules dir");
    fs::write(rules_dir.join("base.md"), "repository-base-rule\n").expect("repo base rule");
    let overlay_rules = wt.join(".libra").join("rules");
    fs::create_dir_all(&overlay_rules).expect("overlay rules");
    fs::write(overlay_rules.join("base.md"), "overlay-must-not-win\n").expect("overlay base");
    fs::write(overlay_rules.join("extra.md"), "overlay-extra-ok\n").expect("overlay extra");

    let contexts_dir = main.join(".libra").join("contexts");
    fs::create_dir_all(&contexts_dir).expect("contexts dir");
    fs::write(contexts_dir.join("dev.md"), "repository-dev-context\n").expect("repo context");
    let overlay_contexts = wt.join(".libra").join("contexts");
    fs::create_dir_all(&overlay_contexts).expect("overlay contexts");
    fs::write(
        overlay_contexts.join("dev.md"),
        "overlay-dev-must-not-win\n",
    )
    .expect("overlay context");

    let main_rule = load_rule(RuleCategory::Base, main).expect("main rule");
    let linked_rule = load_rule(RuleCategory::Base, &wt).expect("linked rule");
    assert_eq!(main_rule.content.trim(), "repository-base-rule");
    assert_eq!(
        linked_rule.content.trim(),
        "repository-base-rule",
        "overlay must not replace repository rules"
    );

    let main_ctx = ContextMode::Dev.load_content(main).expect("main context");
    let linked_ctx = ContextMode::Dev.load_content(&wt).expect("linked context");
    assert_eq!(main_ctx.trim(), "repository-dev-context");
    assert_eq!(
        linked_ctx.trim(),
        "repository-dev-context",
        "overlay must not replace repository contexts"
    );
}

#[test]
fn blank_repository_rule_blocks_overlay_same_name() {
    let (repo, parent) = repo_with_linked_worktree();
    let main = repo.path();
    let wt = parent.path().join("wt");
    let rules_dir = main.join(".libra").join("rules");
    fs::create_dir_all(&rules_dir).expect("rules dir");
    fs::write(rules_dir.join("base.md"), "   \n").expect("blank repo rule");
    let overlay_rules = wt.join(".libra").join("rules");
    fs::create_dir_all(&overlay_rules).expect("overlay rules");
    fs::write(
        overlay_rules.join("base.md"),
        "overlay-must-not-replace-blank\n",
    )
    .expect("overlay same-name rule");

    let linked = load_rule(RuleCategory::Base, &wt).expect("linked rule");
    assert!(
        !linked.content.contains("overlay-must-not-replace-blank"),
        "blank repository file must block overlay replacement"
    );
    assert!(
        linked.content.contains("{working_dir}"),
        "blank repository rule should fall back to embedded, not overlay"
    );
}

#[test]
fn repository_disabled_pretool_stays_disabled() {
    let (repo, parent) = repo_with_linked_worktree();
    let main = repo.path();
    let wt = parent.path().join("wt");
    fs::write(
        main.join(".libra").join("hooks.json"),
        r#"{"hooks":[{"event":"pre_tool_use","matcher":"shell","command":"echo intentionally-off","enabled":false}]}"#,
    )
    .expect("repo disabled hook");
    fs::write(
        wt.join(".libra").join("hooks.json"),
        r#"{"hooks":[{"event":"pre_tool_use","matcher":"shell","command":"echo intentionally-off","enabled":true}]}"#,
    )
    .expect("overlay re-enable attempt");

    let main_hooks = load_hook_config(main).expect("main hooks");
    let linked = load_hook_config(&wt).expect("linked hooks");
    let main_pre = main_hooks
        .hooks
        .iter()
        .find(|h| h.event == HookEvent::PreToolUse && h.command.contains("intentionally-off"))
        .expect("repository disabled PreToolUse");
    assert!(
        !main_pre.enabled,
        "repository enabled=false must be preserved"
    );
    let linked_pre = linked
        .hooks
        .iter()
        .find(|h| h.event == HookEvent::PreToolUse && h.command.contains("intentionally-off"))
        .expect("disabled PreToolUse survives overlay");
    assert!(
        !linked_pre.enabled,
        "overlay must not re-enable a repository PreToolUse that is disabled"
    );
}

#[test]
fn code_agent_config_resolves_repository_layer_in_linked() {
    use std::collections::HashSet;

    let mut seen = HashSet::new();
    for surface in CODE_AGENT_CONFIG_OWNERSHIP {
        assert!(
            seen.insert(surface.location),
            "duplicate inventory location {}",
            surface.location
        );
    }

    let unified_file_or_dir = [
        ("config.toml", ConfigConsumerKind::Security),
        ("sandbox.toml", ConfigConsumerKind::Security),
        ("hooks.json", ConfigConsumerKind::Security),
        ("rules", ConfigConsumerKind::Security),
        ("contexts", ConfigConsumerKind::Security),
        ("agents.toml", ConfigConsumerKind::Extension),
        ("automations.toml", ConfigConsumerKind::Extension),
        ("agents", ConfigConsumerKind::Extension),
        ("commands", ConfigConsumerKind::Extension),
        ("skills", ConfigConsumerKind::Extension),
    ];
    for (location, consumer) in unified_file_or_dir {
        let surface = surface_by_location(location).expect(location);
        assert_eq!(surface.consumer, consumer, "{location} consumer");
        assert_eq!(
            surface.resolution,
            ReadResolution::UnifiedResolver,
            "{location} must use UnifiedResolver"
        );
        assert_eq!(
            surface.owner,
            ConfigOwner::RepositoryWithOptionalOverlay,
            "{location} owner"
        );
        match surface.kind {
            SurfaceKind::File => {
                // File surfaces are resolvable as files.
            }
            SurfaceKind::Directory => {}
            SurfaceKind::Store => panic!("{location} should not be a store"),
        }
    }

    let (repo, parent) = repo_with_linked_worktree();
    let main = repo.path();
    let wt = parent.path().join("wt");

    fs::write(
        main.join(".libra").join("agents.toml"),
        "[code.multi_agent]\nenabled = true\nmax_concurrent_subagents = 2\nmax_subagent_depth = 1\n",
    )
    .expect("repo agents.toml");
    fs::write(
        main.join(".libra").join("automations.toml"),
        "[[rules]]\nid = \"repo-rule\"\n[rules.trigger]\nkind = \"vcs\"\nevent = \"post_commit\"\n[rules.action]\nkind = \"prompt\"\nprompt = \"repo automation\"\n",
    )
    .expect("repo automations.toml");
    let agents_dir = main.join(".libra").join("agents");
    fs::create_dir_all(&agents_dir).expect("agents dir");
    fs::write(
        agents_dir.join("repo_agent.md"),
        "---\nname: repo_agent\ndescription: Repository agent\ntools: []\nmodel: default\n---\nbody\n",
    )
    .expect("repo agent profile");
    fs::write(
        agents_dir.join("shared.md"),
        "---\nname: shared_agent\ndescription: Repository shared\ntools: []\nmodel: default\n---\nrepo body\n",
    )
    .expect("repo shared agent");
    let commands_dir = main.join(".libra").join("commands");
    fs::create_dir_all(&commands_dir).expect("commands dir");
    fs::write(
        commands_dir.join("repo_cmd.md"),
        "---\nname: repo_cmd\ndescription: Repository command\n---\nRepo $ARGUMENTS\n",
    )
    .expect("repo command");
    fs::write(
        commands_dir.join("shared.md"),
        "---\nname: shared_cmd\ndescription: Repository shared command\n---\nRepo shared $ARGUMENTS\n",
    )
    .expect("repo shared command");
    let skills_dir = main.join(".libra").join("skills");
    fs::create_dir_all(&skills_dir).expect("skills dir");
    fs::write(
        skills_dir.join("repo_skill.md"),
        "---\nname = \"repo_skill\"\n---\nRepository skill body\n",
    )
    .expect("repo skill");
    fs::write(
        skills_dir.join("shared.md"),
        "---\nname = \"shared_skill\"\ndescription = \"Repository shared skill\"\n---\nbody\n",
    )
    .expect("repo shared skill");

    let linked_agents = AgentsConfig::load_from_working_dir(&wt).expect("linked agents.toml");
    assert!(
        linked_agents.multi_agent.enabled,
        "repository agents.toml must be visible in linked worktree"
    );

    let linked_automation =
        AutomationConfig::load_from_working_dir(&wt).expect("linked automations.toml");
    assert!(
        linked_automation
            .rules
            .iter()
            .any(|rule| rule.id == "repo-rule"),
        "repository automations.toml must be visible in linked worktree"
    );

    let profiles = load_profiles(&wt);
    assert!(
        profiles.iter().any(|profile| profile.name == "repo_agent"),
        "repository agent profile must be visible in linked worktree"
    );

    let commands = load_commands(&wt);
    assert!(
        commands.iter().any(|command| command.name == "repo_cmd"),
        "repository command must be visible in linked worktree"
    );

    let skills = load_skills(&wt);
    assert!(
        skills.iter().any(|skill| skill.name == "repo_skill"),
        "repository skill must be visible in linked worktree"
    );

    let overlay_gitdir = wt.join(".libra");
    fs::write(
        overlay_gitdir.join("agents.toml"),
        "[code.multi_agent]\nenabled = false\n",
    )
    .expect("overlay agents.toml");
    fs::write(
        overlay_gitdir.join("automations.toml"),
        "[[rules]]\nid = \"overlay-rule\"\n[rules.trigger]\nkind = \"vcs\"\nevent = \"post_commit\"\n[rules.action]\nkind = \"prompt\"\nprompt = \"overlay automation\"\n",
    )
    .expect("overlay automations.toml");
    let overlay_agents = overlay_gitdir.join("agents");
    fs::create_dir_all(&overlay_agents).expect("overlay agents dir");
    fs::write(
        overlay_agents.join("shared.md"),
        "---\nname: shared_agent\ndescription: Overlay shared\ntools: []\nmodel: default\n---\noverlay body\n",
    )
    .expect("overlay shared agent");
    let overlay_commands = overlay_gitdir.join("commands");
    fs::create_dir_all(&overlay_commands).expect("overlay commands dir");
    fs::write(
        overlay_commands.join("shared.md"),
        "---\nname: shared_cmd\ndescription: Overlay shared command\n---\nOverlay shared $ARGUMENTS\n",
    )
    .expect("overlay shared command");
    let overlay_skills = overlay_gitdir.join("skills");
    fs::create_dir_all(&overlay_skills).expect("overlay skills dir");
    fs::write(
        overlay_skills.join("shared.md"),
        "---\nname = \"shared_skill\"\ndescription = \"Overlay shared skill\"\n---\nbody\n",
    )
    .expect("overlay shared skill");

    let overlay_agents_cfg = AgentsConfig::load_from_working_dir(&wt).expect("overlay agents.toml");
    assert!(
        !overlay_agents_cfg.multi_agent.enabled,
        "extension overlay must win on agents.toml"
    );

    let overlay_automation =
        AutomationConfig::load_from_working_dir(&wt).expect("overlay automations.toml");
    assert!(
        overlay_automation
            .rules
            .iter()
            .any(|rule| rule.id == "overlay-rule"),
        "extension overlay must win on automations.toml"
    );
    assert!(
        !overlay_automation
            .rules
            .iter()
            .any(|rule| rule.id == "repo-rule"),
        "file overlay replace must not keep repository-only automation rules"
    );

    let overlay_profiles = load_profiles(&wt);
    let shared_agent = overlay_profiles
        .iter()
        .find(|profile| profile.name == "shared_agent")
        .expect("shared agent");
    assert_eq!(
        shared_agent.description, "Overlay shared",
        "same-name agent overlay must win"
    );
    assert!(
        overlay_profiles
            .iter()
            .any(|profile| profile.name == "repo_agent"),
        "repository-only agent must remain visible beside overlay"
    );

    let overlay_cmds = load_commands(&wt);
    let shared_cmd = overlay_cmds
        .iter()
        .find(|command| command.name == "shared_cmd")
        .expect("shared command");
    assert_eq!(
        shared_cmd.description, "Overlay shared command",
        "same-name command overlay must win"
    );
    assert!(
        overlay_cmds
            .iter()
            .any(|command| command.name == "repo_cmd"),
        "repository-only command must remain visible beside overlay"
    );

    let overlay_skills_loaded = load_skills(&wt);
    let shared_skill = overlay_skills_loaded
        .iter()
        .find(|skill| skill.name == "shared_skill")
        .expect("shared skill");
    assert_eq!(
        shared_skill.description, "Overlay shared skill",
        "same-name skill overlay must win"
    );
    assert!(
        overlay_skills_loaded
            .iter()
            .any(|skill| skill.name == "repo_skill"),
        "repository-only skill must remain visible beside overlay"
    );
}
