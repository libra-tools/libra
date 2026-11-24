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
- 2026-11-23 (PM-10a / #995): `GIT_AUTHOR_IDENT` now goes through the new
  `pub(crate) commit::create_author_signature` (same `resolve_author_identity` +
  `GIT_AUTHOR_DATE` + `user.useConfigOnly` path `libra commit` uses); the
  committer side still uses the PM-10 config-only path until PM-37.
- 2026-11-23 (PM-37 / #995): `GIT_COMMITTER_IDENT` now goes through
  `commit::create_committer_signature` (committer identity + `GIT_COMMITTER_DATE`),
  so both variables report `libra commit`'s signatures. Tests:
  `var_committer_ident_uses_committer_env` (AC1–AC8).

## Tests

- `command::var_test::var_ident_ignores_identity_and_date_env` (AC1/AC2)
- `command::var_test::var_list_reports_both_idents_in_order` (AC3)
- `command::var_test::var_rejects_unknown_and_missing_repo` (AC4/AC5)
- `command::var_test::var_json_and_missing_identity` (AC6/AC7)
- 2026-11-23 (PM-22 / #995): `GIT_EDITOR` reuses `editor::resolve_editor`
  (`GIT_EDITOR` > `core.editor` > `VISUAL` > `EDITOR`) plus Git's terminal
  fallback (`vi` for a usable `TERM`, otherwise exit 1/empty). Tests:
  `var_editor_variable` (AC1–AC6).
- 2026-11-23 (PM-36 / #995): `GIT_SEQUENCE_EDITOR` reuses
  `editor::resolve_sequence_editor` and the same fallback. Tests:
  `var_sequence_editor_variable` (AC1–AC5). `var -l` now lists four rows; the
  full Git table is still PM-25.
