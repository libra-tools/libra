//! Canonical manifest of `tests/command/status_wave0_test.rs` test names
//! (plan-20260714 §B.9).
//!
//! This constant is the single source of truth for the wave-0 status test
//! set: `compat_status_wave0_register` asserts bidirectional set equality
//! against `cargo test --test command_test -- --list` and strict
//! alphabetical ordering. Add or remove a test in the module and this list
//! together — never edit only one side.

pub const STATUS_WAVE0_TESTS: &[&str] = &[
    "porcelain_v1_rename_output_stays_add_delete",
    "porcelain_v2_unmerged_u_line",
    "resolved_conflict_with_stage0_emits_no_u_line",
    "unmerged_stage_presence_to_xy_mapping",
];
