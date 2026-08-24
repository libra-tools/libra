//! Architecture guard for the external-agent capture subsystem (AG-16/AG-24).
//!
//! Pins the boundary rules from `docs/development/tracing/agent.md`:
//! observed_agents (capture) stays decoupled from the internal AgentRuntime
//! and checkpoint layers, every known `AgentKind` resolves to a live
//! adapter, external agents cannot enter the static roster, and the SQL
//! CHECK constraint / doc roster / Rust enum stay in sync.

use std::{collections::BTreeSet, fs, path::Path};

use libra::internal::ai::observed_agents::{
    AgentKind, SlugLookup, agent_for, lookup_cli_slug, registration_for, registry,
};

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Capture modules must not import the internal AgentRuntime or the
/// checkpoint-writer layers. Allowed seams: `hooks::{lifecycle,provider}`
/// (hook contracts), `completion` (shared usage model), `session` (session
/// context types) and — documented exception — `orchestrator::types` for
/// the derived `ToolCallRecord` projection (`derived.rs`). Anything else
/// from the runtime side is a boundary violation.
///
/// The check is AST-based (`syn`): use-trees are flattened (so grouped and
/// nested-grouped imports cannot slip through), inline fully-qualified
/// paths are visited, and items annotated `#[cfg(test)]` are pruned —
/// schema-lockstep tests may deliberately drive runtime writers (e.g.
/// derived.rs's normalized-event integration test).
#[test]
fn observed_agent_modules_do_not_import_runtime_or_checkpoint_layers() {
    use syn::visit::Visit;

    /// Why a resolved path is out of bounds, or `None` when it is fine.
    ///
    /// `original` is a `::`-joined path as written. Leading `crate::` /
    /// `super::` chains are normalized away; the remainder is judged
    /// ai-relative when it came through `internal::ai::` explicitly or
    /// through enough `super::` hops to escape the capture module
    /// (`ai_root_supers` = 2 for files directly under `observed_agents/`,
    /// 3 for `builtin/`, …). Bare paths (`runtime::Handle` from a `use
    /// tokio::runtime` import) are not judged — their `use` item is.
    /// Module-root imports/renames of `crate::internal` / the ai root and
    /// root-level globs are forbidden outright: they would let an alias
    /// (`use crate::internal::ai as x; x::runtime::…`) evade the check.
    fn forbidden_reason(original: &str, ai_root_supers: usize) -> Option<String> {
        let had_crate = original.starts_with("crate::");
        let mut path = original.strip_prefix("crate::").unwrap_or(original);
        let mut supers = 0usize;
        while let Some(rest) = path.strip_prefix("super::") {
            path = rest;
            supers += 1;
        }
        if had_crate && (path == "internal" || path == "internal::ai") {
            return Some(
                "module-root import/rename of crate::internal(::ai) — alias bypass".to_string(),
            );
        }
        let candidate = if let Some(rest) = path
            .strip_prefix("internal::ai::")
            .or_else(|| path.strip_prefix("ai::"))
        {
            rest
        } else if !had_crate && supers >= ai_root_supers {
            path
        } else {
            return None;
        };
        if candidate.is_empty() {
            return Some("aliasing the internal::ai root — alias bypass".to_string());
        }
        if candidate == "*" {
            return Some("glob import from the internal::ai root".to_string());
        }
        if candidate == "hooks" || candidate == "hooks::*" {
            return Some(
                "module-root/glob import of internal::ai::hooks (surfaces hooks::runtime)"
                    .to_string(),
            );
        }
        for module in ["agent", "runtime", "agent_run", "history"] {
            if candidate == module || candidate.starts_with(&format!("{module}::")) {
                return Some(format!("internal::ai::{module}"));
            }
        }
        if candidate == "hooks::runtime" || candidate.starts_with("hooks::runtime::") {
            return Some("internal::ai::hooks::runtime".to_string());
        }
        if (candidate == "orchestrator" || candidate.starts_with("orchestrator::"))
            && !candidate.starts_with("orchestrator::types")
        {
            return Some("internal::ai::orchestrator (outside the ::types seam)".to_string());
        }
        None
    }

    /// Flatten a use-tree into fully-qualified `::`-joined paths.
    fn flatten_use(tree: &syn::UseTree, prefix: &str, out: &mut Vec<String>) {
        let join = |prefix: &str, ident: &dyn std::fmt::Display| {
            if prefix.is_empty() {
                ident.to_string()
            } else {
                format!("{prefix}::{ident}")
            }
        };
        match tree {
            syn::UseTree::Path(path) => {
                flatten_use(&path.tree, &join(prefix, &path.ident), out);
            }
            // `{self}` / `{self as x}` denote the prefix module itself —
            // normalize so root-alias checks fire on the real path.
            syn::UseTree::Name(name) if name.ident == "self" => out.push(prefix.to_string()),
            syn::UseTree::Rename(rename) if rename.ident == "self" => out.push(prefix.to_string()),
            syn::UseTree::Name(name) => out.push(join(prefix, &name.ident)),
            syn::UseTree::Rename(rename) => out.push(join(prefix, &rename.ident)),
            syn::UseTree::Glob(_) => out.push(join(prefix, &"*")),
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    flatten_use(item, prefix, out);
                }
            }
        }
    }

    /// Only the exact `#[cfg(test)]` predicate prunes — `cfg(not(test))`
    /// (and any compound predicate) is production code and stays guarded.
    fn has_cfg_test(attrs: &[syn::Attribute]) -> bool {
        attrs.iter().any(|attr| {
            attr.path().is_ident("cfg")
                && matches!(&attr.meta, syn::Meta::List(list) if list.tokens.to_string().trim() == "test")
        })
    }

    fn item_attrs(item: &syn::Item) -> &[syn::Attribute] {
        match item {
            syn::Item::Const(i) => &i.attrs,
            syn::Item::Enum(i) => &i.attrs,
            syn::Item::ExternCrate(i) => &i.attrs,
            syn::Item::Fn(i) => &i.attrs,
            syn::Item::ForeignMod(i) => &i.attrs,
            syn::Item::Impl(i) => &i.attrs,
            syn::Item::Macro(i) => &i.attrs,
            syn::Item::Mod(i) => &i.attrs,
            syn::Item::Static(i) => &i.attrs,
            syn::Item::Struct(i) => &i.attrs,
            syn::Item::Trait(i) => &i.attrs,
            syn::Item::TraitAlias(i) => &i.attrs,
            syn::Item::Type(i) => &i.attrs,
            syn::Item::Union(i) => &i.attrs,
            syn::Item::Use(i) => &i.attrs,
            _ => &[],
        }
    }

    struct BoundaryGuard {
        violations: Vec<String>,
        /// `super::` hops from this file's module to the `internal::ai`
        /// root: 2 for files directly under `observed_agents/`, 3 for
        /// `builtin/`, … Used to judge super-relative paths correctly.
        ai_root_supers: usize,
    }

    impl<'ast> Visit<'ast> for BoundaryGuard {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            // Prune #[cfg(test)] subtrees — test-only seams are allowed.
            if has_cfg_test(item_attrs(item)) {
                return;
            }
            syn::visit::visit_item(self, item);
        }

        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            let mut paths = Vec::new();
            flatten_use(&item.tree, "", &mut paths);
            for path in paths {
                if let Some(reason) = forbidden_reason(&path, self.ai_root_supers) {
                    self.violations.push(format!("use {path} → {reason}"));
                }
            }
        }

        fn visit_path(&mut self, path: &'ast syn::Path) {
            let joined = path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>()
                .join("::");
            if let Some(reason) = forbidden_reason(&joined, self.ai_root_supers) {
                self.violations.push(format!("path {joined} → {reason}"));
            }
            syn::visit::visit_path(self, path);
        }
    }

    let dir = repo_root().join("src/internal/ai/observed_agents");
    let mut checked = 0usize;
    let mut stack = vec![dir];
    let mut violations = Vec::new();
    while let Some(current) = stack.pop() {
        for entry in fs::read_dir(&current).expect("read observed_agents dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|ext| ext != "rs") {
                continue;
            }
            let source = fs::read_to_string(&path).expect("read source file");
            let file = syn::parse_file(&source)
                .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
            checked += 1;
            let relative = path
                .strip_prefix(repo_root().join("src/internal/ai/observed_agents"))
                .expect("scanned file lives under observed_agents");
            let depth = relative.components().count().saturating_sub(1);
            // `mod.rs` IS its directory's module — one super fewer than a
            // leaf file at the same directory level.
            let is_mod_rs = relative.file_name().is_some_and(|name| name == "mod.rs");
            let mut guard = BoundaryGuard {
                violations: Vec::new(),
                ai_root_supers: 2 + depth - usize::from(is_mod_rs),
            };
            guard.visit_file(&file);
            for violation in guard.violations {
                violations.push(format!("{}: {violation}", path.display()));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "observed_agents must stay decoupled from the internal AgentRuntime/checkpoint \
         layers:\n{}",
        violations.join("\n")
    );
    assert!(
        checked >= 8,
        "expected to scan the observed_agents sources, got {checked}"
    );
}

/// `agent_for` is total over `AgentKind` and each adapter reports the kind
/// it was registered under; the registry row exists for every kind.
#[test]
fn all_known_agent_kinds_resolve_non_null_adapter() {
    for kind in AgentKind::all() {
        let agent = agent_for(*kind);
        assert_eq!(agent.provider_kind(), *kind);
        let row = registration_for(*kind);
        assert_eq!(row.db_value, kind.as_db_str());
        // The capability introspection default must not panic for any kind.
        let _ = agent.declared_capabilities();
    }
}

/// External `libra-agent-*` binaries never appear in the static roster —
/// registration requires the AG-18 `info`/trust flow, so the static matrix
/// only carries built-in rows and unknown slugs stay quarantined.
#[test]
fn external_agent_info_is_required_for_registration() {
    for row in registry() {
        assert!(
            !row.external_binary,
            "{}: static registry rows must be built-in adapters; external agents \
             register through the AG-18 info/trust flow only",
            row.slug
        );
    }
    assert_eq!(
        lookup_cli_slug("libra-agent-anything"),
        SlugLookup::UnknownQuarantined
    );
}

/// The `agent_session.agent_kind` SQL CHECK constraint, the Rust enum and
/// the tracing/agent.md roster stay in sync.
#[test]
fn agent_kind_enum_sql_check_and_doc_roster_stay_in_sync() {
    // Rust enum → db values.
    let enum_values: BTreeSet<String> = AgentKind::all()
        .iter()
        .map(|kind| kind.as_db_str().to_string())
        .collect();

    // SQL CHECK constraint values from the capture migration.
    let migration =
        fs::read_to_string(repo_root().join("sql/migrations/2026050303_agent_capture.sql"))
            .expect("read agent capture migration");
    let check_block = migration
        .split("`agent_kind`           TEXT NOT NULL CHECK(`agent_kind` IN (")
        .nth(1)
        .and_then(|rest| rest.split("))").next())
        .expect("agent_kind CHECK block present in migration");
    let sql_values: BTreeSet<String> = check_block
        .split('\'')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect();
    assert_eq!(
        sql_values, enum_values,
        "agent_session.agent_kind CHECK constraint drifted from AgentKind::as_db_str"
    );

    // Doc roster (docs/development/tracing/agent.md 第一批支持项目) matches
    // the registry's supported set.
    let agent_doc = fs::read_to_string(repo_root().join("docs/development/tracing/agent.md"))
        .expect("read tracing/agent.md");
    let supported: Vec<&str> = registry()
        .iter()
        .filter(|row| row.supported)
        .map(|row| row.slug)
        .collect();
    assert_eq!(supported, ["claude-code", "codex", "opencode"]);
    for slug in &supported {
        assert!(
            agent_doc.contains(&format!("| `{slug}` |")),
            "tracing/agent.md first-batch roster table must list {slug}"
        );
    }
    // The doc must keep declaring the frozen first-batch roster line.
    assert!(
        agent_doc.contains("`claude-code` / `codex` / `opencode`"),
        "tracing/agent.md must keep the frozen first-batch roster statement"
    );
}

/// W0-03: TUI removal is forbidden until the Web-only completion checklist is
/// both complete and still tied to the runtime source seam. While TUI remains
/// compiled, this test freezes the complete checklist (parity dimensions,
/// A0 inputs, non-Code TUI consumers, and fixed product decisions) so later
/// phases cannot silently omit a required closeout item.
#[test]
fn code_web_only_completion_gate() {
    let code_doc = fs::read_to_string(repo_root().join("docs/development/tracing/code.md"))
        .expect("read tracing/code.md");
    let mut missing = Vec::new();

    if !code_doc.contains("## Web-only completion gate（W0-03）") {
        missing.push("heading:## Web-only completion gate（W0-03）".to_string());
    }
    // AC2: current Web-only direct-turn is explicitly not a completion state.
    if !(code_doc.contains("当前 Web-only direct-turn 不是完成态")
        || code_doc.contains("这不是 Web-only completion"))
    {
        missing.push("AC2:direct-turn-not-complete".to_string());
    }

    let gates = [
        ("GATE-WEB-PLAN", "plan workflow parity"),
        ("GATE-WEB-GOAL", "goal/task parity"),
        ("GATE-WEB-RESUME", "resume parity"),
        ("GATE-WEB-APPROVAL", "approval/cancel parity"),
        ("GATE-WEB-SSE", "SSE gap/backpressure"),
        ("GATE-WEB-CODEX", "Codex normalization"),
        ("GATE-WEB-MCP", "MCP / `code --control stdio` boundary"),
        ("GATE-WEB-DOCS", "docs/compat closeout"),
    ];
    for (gate, phrase) in gates {
        let listed = code_doc.contains(&format!("| [ ] {gate}"))
            || code_doc.contains(&format!("| [x] {gate}"));
        if !listed {
            missing.push(format!("gate:{gate}"));
        }
        if !code_doc.contains(phrase) {
            missing.push(format!("gate-phrase:{phrase}"));
        }
    }
    for decision in [
        "GATE-WEB-DECISION-WEB-ONLY",
        "GATE-WEB-DECISION-BAKE",
        "GATE-WEB-DECISION-STDIO",
        "GATE-WEB-DECISION-SSH",
        "GATE-WEB-DECISION-GRAPH",
    ] {
        if !code_doc.contains(decision) {
            missing.push(format!("decision:{decision}"));
        }
    }
    // AC4: A0-02..A0-11 are completed inputs; the gate must not recreate them.
    if !(code_doc.contains("A0-02..A0-11") && code_doc.contains("不因为本清单而被复制")) {
        missing.push("AC4:A0-inputs-not-copied".to_string());
    }
    // AC5: non-Code TUI consumers stay visible so W5 cannot orphan-delete tui.
    for consumer in [
        "src/command/graph.rs",
        "src/command/agent/graph.rs",
        "TuiControlError",
        "src/internal/ai/agent/format.rs",
    ] {
        if !code_doc.contains(consumer) {
            missing.push(format!("consumer:{consumer}"));
        }
    }
    // Product-decision fixed phrases (compat window / bake / stdio / SSH / graph).
    for phrase in [
        "--web-only",
        "3 patch",
        "MCP transport",
        "SSH",
        "libra graph",
    ] {
        if !code_doc.contains(phrase) {
            missing.push(format!("decision-phrase:{phrase}"));
        }
    }

    assert!(
        missing.is_empty(),
        "Web-only completion gate missing required items: {}",
        missing.join(", ")
    );

    let internal_mod =
        fs::read_to_string(repo_root().join("src/internal/mod.rs")).expect("read internal/mod.rs");
    let tui_still_compiled = internal_mod.contains("pub mod tui;");
    if !tui_still_compiled {
        let incomplete = gates
            .iter()
            .map(|(gate, _)| *gate)
            .filter(|gate| code_doc.contains(&format!("| [ ] {gate}")))
            .collect::<Vec<_>>();
        assert!(
            incomplete.is_empty(),
            "TUI was removed before Web-only completion gates passed: {}",
            incomplete.join(", ")
        );
        let runtime_mod = fs::read_to_string(repo_root().join("src/internal/ai/runtime/mod.rs"))
            .expect("read runtime/mod.rs");
        assert!(
            runtime_mod.contains("pub mod worker;"),
            "TUI removal requires the UI-neutral AgentRuntime worker seam"
        );
    }
}

/// W5-10: once the internal terminal UI module is retired, neither its direct
/// dependencies nor its production symbol names may return unnoticed.
#[test]
fn terminal_ui_dependencies_and_production_symbols_remain_retired() {
    let manifest = fs::read_to_string(repo_root().join("Cargo.toml")).expect("read Cargo.toml");
    let manifest: toml::Value = manifest.parse().expect("parse Cargo.toml");

    fn manifest_mentions_dependency(value: &toml::Value, dependency: &str) -> bool {
        let Some(table) = value.as_table() else {
            return false;
        };
        table.iter().any(|(key, value)| {
            key == dependency
                || value
                    .as_table()
                    .and_then(|value| value.get("package"))
                    .and_then(toml::Value::as_str)
                    .is_some_and(|package| package == dependency)
                || manifest_mentions_dependency(value, dependency)
        })
    }

    for dependency in ["ratatui", "crossterm"] {
        let present = manifest_mentions_dependency(&manifest, dependency);
        assert!(
            !present,
            "Cargo.toml must not restore the retired direct {dependency} dependency"
        );
    }

    fn visit_source_tree(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
        for entry in fs::read_dir(dir).expect("read production source directory") {
            let entry = entry.expect("read production source entry");
            let path = entry.path();
            if path.is_dir() {
                visit_source_tree(&path, files);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }

    let mut sources = Vec::new();
    visit_source_tree(&repo_root().join("src"), &mut sources);
    let library_root = fs::read_to_string(repo_root().join("src/lib.rs")).expect("read lib.rs");
    assert!(
        library_root.contains("supported, patch-compatible embedding API")
            && library_root.contains("`internal` is an implementation detail"),
        "the public internal module must remain documented as an unstable implementation detail",
    );
    for source in sources {
        let contents = fs::read_to_string(&source).expect("read production Rust source");
        for forbidden in ["ratatui", "crossterm", "internal::tui", "Tui"] {
            assert!(
                !contents.contains(forbidden),
                "{} reintroduced retired terminal-UI token {forbidden:?}",
                source.display()
            );
        }
    }
}

/// W5-05: Code runtime behavior must not return to TUI or a Web-private plan
/// workflow state machine. Docs must keep Web as the default surface.
#[test]
fn code_runtime_stays_web_owned_without_tui_or_private_plan_state() {
    let code = fs::read_to_string(repo_root().join("src/command/code.rs")).expect("read code.rs");
    for forbidden in ["TuiCodeUiAdapter", "execute_tui", "Tui::new", "Tui::"] {
        assert!(
            !code.contains(forbidden),
            "src/command/code.rs must not restore TUI startup/adapter token {forbidden:?}"
        );
    }

    let mut web_sources = Vec::new();
    fn visit(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
        for entry in fs::read_dir(dir).expect("read web source directory") {
            let entry = entry.expect("read web source entry");
            let path = entry.path();
            if path.is_dir() {
                visit(&path, files);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }
    visit(&repo_root().join("src/internal/ai/web"), &mut web_sources);
    let mut saw_runtime_plan_handoff = false;
    for source in &web_sources {
        let contents = fs::read_to_string(source).expect("read web rust source");
        assert!(
            !contents.contains("TuiCodeUiAdapter"),
            "{} must not restore TuiCodeUiAdapter",
            source.display()
        );
        assert!(
            !contents.contains("pending_post_plan") && !contents.contains("struct PendingPlan"),
            "{} must not keep a private Plan workflow state machine",
            source.display()
        );
        if contents.contains("submit_confirmed_plan_execution")
            || contents.contains("StartPlanExecution")
        {
            saw_runtime_plan_handoff = true;
        }
    }
    assert!(
        saw_runtime_plan_handoff,
        "Web adapter must hand confirmed plan execution to AgentRuntime"
    );

    let code_doc = fs::read_to_string(repo_root().join("docs/commands/code.md"))
        .expect("read docs/commands/code.md");
    let zh_doc = fs::read_to_string(repo_root().join("docs/commands/zh-CN/code.md"))
        .expect("read docs/commands/zh-CN/code.md");
    for (path, body) in [
        ("docs/commands/code.md", &code_doc),
        ("docs/commands/zh-CN/code.md", &zh_doc),
    ] {
        let lowered = body.to_ascii_lowercase();
        assert!(
            !lowered.contains("default mode launches the tui")
                && !lowered.contains("defaults to the tui")
                && !lowered.contains("默认启动 tui"),
            "{path} must not advertise TUI as the current default"
        );
        assert!(
            body.contains("Web Code UI") || body.contains("Web Code UI"),
            "{path} must keep Web Code UI as the default surface"
        );
    }
}

/// W0-01: retain the source-grounded conflict and A0-consumption records
/// needed to prevent a future Code migration from silently recreating an
/// Agent-side queue, trust policy, or artifact store.
#[test]
fn code_runtime_anchor_audit_is_documented() {
    let code_doc = fs::read_to_string(repo_root().join("docs/development/tracing/code.md"))
        .expect("read tracing/code.md");
    for heading in [
        "### C1–C10 契约冲突表（W0-01）",
        "### A0 接口漂移登记表（W0-01）",
    ] {
        assert!(
            code_doc.contains(heading),
            "tracing/code.md must retain {heading}"
        );
    }
    for contract in 1..=10 {
        assert!(
            code_doc.contains(&format!("| C{contract} |")),
            "runtime contract audit is missing C{contract}"
        );
    }
    for artifact in [
        "A0-02 subagent checkpoint",
        "A0-03 stable error emit",
        "A0-04 run admission",
        "A0-05 fix bridge",
        "A0-06 findings artifacts",
        "A0-07 skill projection",
        "A0-08 trust",
        "A0-09 retention",
        "A0-10 cloud tombstone",
        "A0-11 deferred parity",
        "src/command/agent/graph.rs",
    ] {
        assert!(
            code_doc.contains(artifact),
            "runtime anchor audit is missing {artifact}"
        );
    }
}

/// W1-03: Code workflow JSONL stays additive to the existing session stream
/// and must not silently absorb agent-owned checkpoint/finding/capture data.
#[test]
fn code_session_event_boundary_is_documented() {
    let code_doc = fs::read_to_string(repo_root().join("docs/development/tracing/code.md"))
        .expect("read tracing/code.md");
    for required in [
        "### W1-03 Code 会话事件边界",
        ".libra/sessions/{session_id}/events.jsonl",
        "code_workflow",
        "event_id",
        "sequence: u64",
        "IndeterminateSideEffect",
        "A0-02 subagent checkpoint",
        "A0-06 review/investigate findings",
        "external-agent capture retention",
        "A0-10\ncloud tombstone",
        "W1-05",
        "W1-06",
        "### W1-05 Runtime command durability boundary",
        "RuntimeCommandDurability",
        "command_indeterminate_side_effect",
        "sync_data",
    ] {
        assert!(
            code_doc.contains(required),
            "W1-03 session boundary documentation is missing {required:?}"
        );
    }

    let jsonl = fs::read_to_string(repo_root().join("src/internal/ai/session/jsonl.rs"))
        .expect("read session JSONL implementation");
    for required in [
        "CodeWorkflowEventKind",
        "CommandAccepted",
        "TerminalSuccess",
        "TerminalFailure",
        "IndeterminateSideEffect",
        "append_code_workflow",
        "load_code_workflow_replay",
        "admit_code_command",
        "recover_code_command",
    ] {
        assert!(
            jsonl.contains(required),
            "W1-03 JSONL schema is missing {required}"
        );
    }

    let durability = fs::read_to_string(repo_root().join("src/internal/ai/runtime/durability.rs"))
        .expect("read runtime command durability implementation");
    for required in [
        "BeforeIntentFsync",
        "AfterIntentFsyncBeforeDispatch",
        "AfterDispatchBeforeTerminalFsync",
        "AfterTerminalFsync",
        "retry_recovered_read_only",
    ] {
        assert!(
            durability.contains(required),
            "W1-05 runtime durability boundary is missing {required}"
        );
    }
}
