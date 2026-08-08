//! `compat_status_args_parser_compile` — R0-4 (plan-20260714 §B.4.3): the
//! public `StatusArgs` stays source-compatible for EXTERNAL embedders — both
//! `try_parse_from` and an exhaustive-free struct-literal construction must
//! keep compiling, and the parser must accept `-z` AND its Git-parity
//! `--null` long alias.

use clap::Parser;
use libra::command::status::StatusArgs;

#[test]
fn external_try_parse_from_compiles_and_accepts_z_and_null() {
    let z = StatusArgs::try_parse_from(["status", "-z", "--porcelain"]).expect("-z parses");
    assert!(z.null_terminated);
    let null =
        StatusArgs::try_parse_from(["status", "--null", "--porcelain"]).expect("--null parses");
    assert!(null.null_terminated);
    assert!(
        StatusArgs::try_parse_from(["status", "--find-renames=75"]).is_ok(),
        "optional-value find-renames stays parseable"
    );
}

#[test]
fn external_struct_literal_compiles() {
    // `..Default::default()` keeps this robust to additive fields while still
    // proving the named public fields exist with their documented types.
    let args = StatusArgs {
        short: true,
        null_terminated: true,
        find_renames: Some(75),
        ..Default::default()
    };
    assert!(args.short && args.null_terminated);
    assert_eq!(args.find_renames, Some(75));
}
