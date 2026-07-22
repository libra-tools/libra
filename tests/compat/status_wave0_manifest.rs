//! Canonical manifest of `tests/command/status_wave0_test.rs` test names
//! (plan-20260714 §B.9).
//!
//! This constant is the single source of truth for the wave-0 status test
//! set: `compat_status_wave0_register` asserts bidirectional set equality
//! against `cargo test --test command_test -- --list` and strict
//! alphabetical ordering. Add or remove a test in the module and this list
//! together — never edit only one side.

pub const STATUS_WAVE0_TESTS: &[&str] = &[
    "chain_rename_default_untracked_d_and_question",
    "pathspec_old_only_new_only_matrix",
    "porcelain_v1_rename_output_stays_add_delete",
    "porcelain_v1_uses_rename_arrow_when_detected",
    "porcelain_v2_unmerged_u_line",
    "rename_config_cli_find_renames_overrides_false",
    "rename_config_status_renames_false_disables",
    "rename_exact_staged_detected_by_default",
    "rename_from_subdirectory_detected",
    "rename_inexact_content_change_detected",
    "rename_json_includes_score_and_side",
    "rename_no_renames_flag_splits_add_delete",
    "rename_porcelain_v2_emits_rename_record",
    "rename_short_format_uses_arrow",
    "rename_untracked_config_cascade",
    "resolved_conflict_with_stage0_emits_no_u_line",
    "staged_rename_then_delete_emits_rd",
    "staged_rename_then_modify_emits_rm",
    "tracked_unreadable_path_fails_closed_not_deleted",
    "unmerged_stage_presence_to_xy_mapping",
];
