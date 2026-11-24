# `libra var` — development notes

- Source: `src/command/var.rs`; CLI wiring in `src/cli.rs` (`Commands::Var`,
  `VAR_EXAMPLES`, read-only `command_scope`, "Maintenance And Plumbing" group).
- Identity: `resolve_user_identity_sources(LocalIdentityTarget::CurrentRepo)` and
  only its `config_name`/`config_email` fields (GC-02 single source of truth).
- Signature date/timezone: `git_internal::...::signature::Signature::new`
  (current local time + `TZ` offset), then the one-line ident form is formatted
  from the fields because `Signature`'s `Display` is the multi-line object header.
- Errors: `LBR-CLI-002` (unknown/missing name), `LBR-AUTH-001` (missing identity),
  `LBR-REPO-001` (outside a repository).
- 2026-11-23 (PM-10 / #995): initial implementation; identity/date environment
  variables are deliberately ignored (PM-10a/PM-37 rewire them).

## Tests

- `command::var_test::var_ident_ignores_identity_and_date_env` (AC1/AC2)
- `command::var_test::var_list_reports_both_idents_in_order` (AC3)
- `command::var_test::var_rejects_unknown_and_missing_repo` (AC4/AC5)
- `command::var_test::var_json_and_missing_identity` (AC6/AC7)
