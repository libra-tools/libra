use super::*;

/// Pin the `Display` format for every variant of [`DescribeError`].
/// These strings are used directly as the CliError message via
/// `describe_cli_error` and surface in human and `--json` envelopes.
#[test]
fn describe_error_display_pins_each_variant() {
    assert_eq!(
        DescribeError::HeadUnborn.to_string(),
        "HEAD does not point to a commit",
    );
    assert_eq!(
        DescribeError::InvalidReference("bad-ref".to_string()).to_string(),
        "bad-ref",
    );
    assert_eq!(
        DescribeError::ReadFailure("db locked".to_string()).to_string(),
        "db locked",
    );
    assert_eq!(
        DescribeError::CorruptReference("bad commit hash".to_string()).to_string(),
        "bad commit hash",
    );
    assert_eq!(
        DescribeError::LoadCommit {
            commit_id: "deadbeef".to_string(),
            detail: "object not found".to_string(),
        }
        .to_string(),
        "failed to load commit 'deadbeef': object not found",
    );
    assert_eq!(
        DescribeError::NoNamesFound.to_string(),
        "no names found, cannot describe anything",
    );
    assert_eq!(
        DescribeError::NoContainingTag {
            commit_id: "deadbeef".to_string(),
        }
        .to_string(),
        "cannot describe 'deadbeef': no tag contains it",
    );
    assert_eq!(
        DescribeError::NoExactMatch {
            commit_id: "deadbeef".to_string(),
        }
        .to_string(),
        "no tag exactly matches 'deadbeef'",
    );
    assert_eq!(
        DescribeError::LongWithAbbrevZero.to_string(),
        "options '--long' and '--abbrev=0' cannot be used together",
    );
    assert_eq!(
        DescribeError::InvalidArgument("glob too long".to_string()).to_string(),
        "glob too long",
    );
}

/// `--match`/`--exclude` filter semantics: exclude wins over match, and an empty
/// match set lets every non-excluded name through.
#[test]
fn tag_passes_filters_match_exclude_semantics() {
    let no_globs: [wax::Glob<'_>; 0] = [];
    // No filters: every name passes.
    assert!(tag_passes_filters("v1.0", &no_globs, &no_globs));
    // --match only: name must match at least one glob.
    let match_pats = ["v1.*".to_string()];
    let m = compile_globs(&match_pats).expect("valid glob");
    assert!(tag_passes_filters("v1.2", &m, &no_globs));
    assert!(!tag_passes_filters("v2.0", &m, &no_globs));
    // --exclude wins over --match.
    let exclude_pats = ["*rc*".to_string()];
    let e = compile_globs(&exclude_pats).expect("valid glob");
    assert!(!tag_passes_filters("v1.0rc1", &m, &e));
    assert!(tag_passes_filters("v1.0", &m, &e));
}

/// Overlong and malformed globs are rejected as usage errors rather than panicking.
#[test]
fn compile_globs_rejects_overlong_and_invalid() {
    let long = "a".repeat(MAX_GLOB_LEN + 1);
    assert!(matches!(
        compile_globs(&[long]),
        Err(DescribeError::InvalidArgument(_))
    ));
    // An unterminated alternation `{` is not a valid glob.
    assert!(matches!(
        compile_globs(&["v{1".to_string()]),
        Err(DescribeError::InvalidArgument(_))
    ));
}

/// DESC-04 AC8: `--long` / `--no-long` are last-wins at the parser layer.
#[test]
fn no_long_parse_order() {
    use clap::Parser;

    let args = super::DescribeArgs::try_parse_from(["describe", "--long", "--no-long"])
        .expect("both flags parse");
    assert!(!args.long, "AC8: --no-long wins when it comes last");
    assert!(args.no_long);

    let args = super::DescribeArgs::try_parse_from(["describe", "--no-long", "--long"])
        .expect("both flags parse");
    assert!(args.long, "AC4: --long wins when it comes last");
    assert!(!args.no_long);
}

/// DESC-06 AC5: `--abbrev=N --no-abbrev` leaves `--no-abbrev` in effect.
#[test]
fn no_abbrev_parse_order() {
    use clap::Parser;

    let args = super::DescribeArgs::try_parse_from(["describe", "--abbrev=5", "--no-abbrev"])
        .expect("both flags parse");
    assert_eq!(args.abbrev, None, "AC5 abbrev cleared");
    assert!(args.no_abbrev, "AC5 no_abbrev active");
}
