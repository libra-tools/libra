//! Guard: every surviving non-`none` `#[serial]` annotation must justify itself.
//!
//! Serialization is not free — `serial_test`'s unkeyed `#[serial]` locks the
//! empty-string key, so every unkeyed test is serialized against every other
//! unkeyed test (but NOT against named lanes; named `#[serial(key)]` locks are
//! per-key). This guard keeps that cost deliberate: each surviving
//! annotation the classifier judges `global`/`lane:*` has a row in
//! `tests/SERIAL_REGISTRY.tsv` naming the lane and the reason (`none`-judged
//! annotations are deletion candidates and intentionally carry no row until the
//! conversion lands, see plan-20260729 DEFER-09), and the registry must agree
//! with what the classifier derives from the source. Re-running the classifier
//! is what stops someone writing themselves a row by hand.

use std::{collections::BTreeMap, path::PathBuf, process::Command};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn registry() -> BTreeMap<String, (String, String)> {
    let text = std::fs::read_to_string(repo_root().join("tests/SERIAL_REGISTRY.tsv"))
        .expect("read tests/SERIAL_REGISTRY.tsv");
    let mut out = BTreeMap::new();
    for (n, line) in text.lines().enumerate() {
        if n == 0 {
            assert_eq!(
                line, "test_fn\tlane\treason",
                "SERIAL_REGISTRY.tsv: unexpected header"
            );
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        assert_eq!(
            cols.len(),
            3,
            "SERIAL_REGISTRY.tsv line {}: expected 3 tab-separated columns",
            n + 1
        );
        assert!(
            !cols[2].trim().is_empty(),
            "SERIAL_REGISTRY.tsv: {} has an empty reason",
            cols[0]
        );
        let prior = out.insert(
            cols[0].to_string(),
            (cols[1].to_string(), cols[2].to_string()),
        );
        assert!(
            prior.is_none(),
            "SERIAL_REGISTRY.tsv: duplicate row {}",
            cols[0]
        );
    }
    out
}

fn classify_raw() -> String {
    let out = Command::new("sh")
        .arg(repo_root().join("tests/SERIAL_CLASSIFY.sh"))
        .current_dir(repo_root())
        .output()
        .expect("run tests/SERIAL_CLASSIFY.sh");
    assert!(
        out.status.success(),
        "SERIAL_CLASSIFY.sh failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("classifier output is UTF-8")
}

/// Parse `<fn>\t<verdict>` rows, refusing to merge duplicates: a bare fn name
/// is the only key available, so a repeated name must stop the guard instead
/// of silently keeping whichever verdict came last.
fn parse_classify(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let (fnname, verdict) = line
            .split_once('\t')
            .expect("classifier emits <fn>\\t<verdict>");
        let prior = map.insert(fnname.to_string(), verdict.to_string());
        assert!(
            prior.is_none(),
            "classifier emitted a duplicate row for {fnname}; two files share the \
             same test fn name, so bare-name keys are ambiguous"
        );
    }
    assert!(!map.is_empty(), "the classifier produced no rows");
    map
}

fn classify() -> BTreeMap<String, String> {
    parse_classify(&classify_raw())
}

/// The registry and the classifier must agree, in both directions.
#[test]
fn serial_registry_matches_the_classifier() {
    let reg = registry();
    let derived = classify();
    let expected: BTreeMap<String, String> = derived
        .iter()
        .filter(|(_, v)| v.as_str() != "none")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let missing: Vec<&str> = expected
        .keys()
        .filter(|k| !reg.contains_key(k.as_str()))
        .map(|k| k.as_str())
        .collect();
    assert!(
        missing.is_empty(),
        "these tests are still serialized but have no registry row: {missing:?}"
    );

    let dangling: Vec<&str> = reg
        .keys()
        .filter(|k| !expected.contains_key(k.as_str()))
        .map(|k| k.as_str())
        .collect();
    assert!(
        dangling.is_empty(),
        "these registry rows name nothing that is serialized any more: {dangling:?}"
    );

    let drifted: Vec<String> = expected
        .iter()
        .filter_map(|(k, v)| {
            let (lane, _) = reg.get(k)?;
            (lane != v).then(|| format!("{k}: registry says {lane}, classifier says {v}"))
        })
        .collect();
    assert!(
        drifted.is_empty(),
        "registry/classifier lane drift: {drifted:?}"
    );
}

/// A registry lane is `global` or `lane:<key>(+<key>)*` — one lane per matched
/// process-wide resource (`serial_test` supports multiple keys), each key a
/// non-empty snake_case identifier.
fn lane_is_valid(lane: &str) -> bool {
    if lane == "global" {
        return true;
    }
    let Some(keys) = lane.strip_prefix("lane:") else {
        return false;
    };
    !keys.is_empty() && keys.split('+').all(key_is_valid)
}

fn key_is_valid(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Every registry lane is either `global` or a named `lane:<key>` with a
/// non-empty key — never `none`, so every surviving serial annotation carries
/// a deliberate reason. (An unkeyed `#[serial]` is NOT restricted to `global`:
/// the classifier may still justify it as a named lane such as `lane:cwd`.)
#[test]
fn registry_lanes_are_global_or_named() {
    for (fnname, (lane, _)) in registry() {
        assert!(lane_is_valid(&lane), "{fnname}: invalid lane {lane}");
    }
}

/// Counterexample: the lane closure rejects an empty or malformed key.
#[test]
fn lane_validation_rejects_empty_and_malformed_keys() {
    assert!(lane_is_valid("global"));
    assert!(lane_is_valid("lane:cwd"));
    assert!(lane_is_valid("lane:rpc_env_probe"));
    assert!(lane_is_valid("lane:hash_kind+cwd"));
    assert!(!lane_is_valid("lane:"));
    assert!(!lane_is_valid("lane:+cwd"));
    assert!(!lane_is_valid("lane:hash_kind+"));
    assert!(!lane_is_valid("lane:hash_kind+Camel"));
    assert!(!lane_is_valid("lane:Camel"));
    assert!(!lane_is_valid("none"));
}

/// `<site:path:line>` rows (attributes inside `macro_rules!` bodies) must point
/// at a real attribute site: the file exists, the line is in range, and that
/// line still carries a serial attribute as its first token.
#[test]
fn site_rows_point_at_real_attribute_sites() {
    let mut sites = 0;
    for (key, _) in registry() {
        let Some(inner) = key.strip_prefix("<site:").and_then(|k| k.strip_suffix('>')) else {
            continue;
        };
        sites += 1;
        let (path, line) = inner
            .rsplit_once(':')
            .unwrap_or_else(|| panic!("site key {key} is not <site:path:line>"));
        let line: usize = line
            .parse()
            .unwrap_or_else(|_| panic!("site key {key} has a non-numeric line"));
        let text = std::fs::read_to_string(repo_root().join(path))
            .unwrap_or_else(|e| panic!("site key {key}: cannot read {path}: {e}"));
        let row = text
            .lines()
            .nth(line - 1)
            .unwrap_or_else(|| panic!("site key {key}: {path} has no line {line}"));
        let attr = row.trim_start();
        assert!(
            attr.starts_with("#[serial") || attr.starts_with("#[serial_test::serial"),
            "site key {key}: line does not carry a serial attribute: {row}"
        );
    }
    assert!(sites > 0, "expected at least one macro-body site row");
}

/// The classifier is a pure function of the tree: two runs agree byte for byte
/// on raw stdout, so order or duplication drift cannot hide behind a map.
#[test]
fn classifier_is_deterministic() {
    assert_eq!(
        classify_raw(),
        classify_raw(),
        "SERIAL_CLASSIFY.sh is not deterministic"
    );
}

/// Counterexample: the parser must refuse duplicate fn rows instead of letting
/// the later verdict silently overwrite the earlier one.
#[test]
#[should_panic(expected = "duplicate row")]
fn parse_classify_rejects_duplicate_fn_names() {
    parse_classify("some_test\tglobal\nsome_test\tlane:cwd\n");
}

/// Counterexample: a named key whose set does not cover the body's own
/// process-wide pollution is an insufficient lock and must stop the classifier
/// instead of being blessed as a composite lane.
#[test]
fn classifier_rejects_named_key_with_uncovered_pollution() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("COMPATIBILITY.md"), b"").expect("write marker");
    std::fs::create_dir(tmp.path().join(".git")).expect("create .git");
    std::fs::create_dir_all(tmp.path().join("tests")).expect("create tests/");
    std::fs::write(
        tmp.path().join("tests/u_insufficient_key.rs"),
        "#[test]\n#[serial(private)]\nfn insufficient() {\n    std::env::set_var(\"X\", \"1\");\n}\n",
    )
    .expect("write insufficient-key fixture");
    let out = Command::new("sh")
        .arg(repo_root().join("tests/SERIAL_CLASSIFY.sh"))
        .env("SERIAL_CLASSIFY_ROOT", tmp.path())
        .output()
        .expect("run classifier against insufficient-key fixture");
    assert!(
        !out.status.success(),
        "classifier must reject a named key that does not cover env pollution"
    );
    let stderr = String::from_utf8(out.stderr).expect("stderr is UTF-8");
    assert!(
        stderr.contains("do not cover process-wide pollution lane(s) env"),
        "rejection must name the uncovered lane, got: {stderr}"
    );
}

/// Counterexample: the scanner reads `#[test] #[serial]` written on one line;
/// ignores `#[serial]` inside string literals (normal/raw/byte/C), char and
/// byte-char literals, and line/single/nested block comments; and keeps every
/// matched resource lane for mixed hash+cwd pollution (`lane:hash_kind+cwd`).
#[test]
fn classifier_ignores_string_literals_and_reads_same_line_attributes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("COMPATIBILITY.md"), b"").expect("write marker");
    std::fs::create_dir(tmp.path().join(".git")).expect("create .git");
    std::fs::create_dir_all(tmp.path().join("tests")).expect("create tests/");
    std::fs::write(
        tmp.path().join("tests/fixture.rs"),
        concat!(
            "#[test] #[serial]\nfn same_line_multi_attr() {}\n\n",
            "#[test]\n#[serial]\nfn real_serial() {}\n\n",
            "#[test]\n#[serial(inner_attrs_key)]\nfn prefixed_key() {}\n\n",
            "#[test]\n#[serial(k, crate = wrapper::fs)]\nfn keyed_with_crate_config() {}\n\n",
            "#[test]\n#[serial(crate = wrapper::fs)]\nfn crate_config_only() {}\n\n",
            "#[test]\n#[serial(\n)]\nfn newline_bare_attr() {}\n\n",
            "#[test]\n#[serial(a)   \n]\nfn trailing_space_before_bracket() {}\n\n",
            "#[test]\n#[serial(a)\n   \n]\nfn blank_line_before_bracket() {}\n\n",
            "#[test]\n#[serial]\nfn mixed_resources() {\n",
            "    let _g = ChangeDirGuard;\n",
            "    set_hash_kind();\n",
            "}\n\n",
            "#[test]\n#[serial(hash_kind, cwd)]\nfn already_named_composite() {\n",
            "    let _g = ChangeDirGuard;\n",
            "    set_hash_kind();\n",
            "}\n\n",
            "#[test]\n#[serial(a, inner_attrs = [ntest::timeout(100)])]\nfn with_inner_attrs() {}\n\n",
            "#[test]\n#[serial(\n",
            "    hash_kind,\n",
            "    cwd,\n",
            ")]\nfn multi_line_composite() {\n",
            "    let _g = ChangeDirGuard;\n",
            "    set_hash_kind();\n",
            "}\n\n",
            "#[test]\nfn string_fixture() {\n",
            "    let s = \"#[serial]\\n#[serial]\\n\";\n",
            "    let raw = r#\"#[serial]\n#[serial]\n\"#;\n",
            "    let b = b\"#[serial]\";\n",
            "    let br = br#\"#[serial]\n\"#;\n",
            "    let c = c\"#[serial]\";\n",
            "    let cr = cr#\"#[serial]\n\"#;\n",
            "    let ch = '#';\n",
            "    let cb = b'#';\n",
            "    let q = '\\'';\n",
            "    // #[serial]\n",
            "    /* #[serial] */\n",
            "    /* outer /* #[serial] */ still comment */\n",
            "    /* multi-line\n",
            "       #[serial]\n",
            "       end */\n",
            "    let _ = (s, raw, b, br, c, cr, ch, cb, q);\n",
            "}\n",
        ),
    )
    .expect("write fixture");
    std::fs::write(tmp.path().join("tests/z_unclosed.rs"), "#[serial(a,\n")
        .expect("write unbalanced fixture");
    std::fs::write(
        tmp.path().join("tests/y_unclosed_bare.rs"),
        "#[serial\nfn unclosed_bare() {}\n",
    )
    .expect("write bare-unclosed fixture");
    std::fs::write(
        tmp.path().join("tests/x_missing_bracket.rs"),
        "#[serial(a)\nfn missing_bracket() {}\n",
    )
    .expect("write missing-bracket fixture");
    std::fs::write(
        tmp.path().join("tests/w_mismatched.rs"),
        "#[serial(a])\nfn mismatched() {}\n",
    )
    .expect("write mismatched-delimiter fixture");
    std::fs::write(
        tmp.path().join("tests/v_unclosed_body.rs"),
        "#[test]\n#[serial]\nfn unclosed_body() {\n    let x = 1;\n",
    )
    .expect("write unclosed-body fixture");
    let out = Command::new("sh")
        .arg(repo_root().join("tests/SERIAL_CLASSIFY.sh"))
        .env("SERIAL_CLASSIFY_ROOT", tmp.path())
        .output()
        .expect("run classifier against fixture tree");
    assert!(
        out.status.success(),
        "classifier failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("classifier output is UTF-8");
    assert_eq!(
        stdout,
        concat!(
            "<site:tests/w_mismatched.rs:1>\tglobal\n",
            "<site:tests/x_missing_bracket.rs:1>\tglobal\n",
            "<site:tests/y_unclosed_bare.rs:1>\tglobal\n",
            "<site:tests/z_unclosed.rs:1>\tglobal\n",
            "already_named_composite\tlane:hash_kind+cwd\n",
            "blank_line_before_bracket\tlane:a\n",
            "crate_config_only\tnone\n",
            "keyed_with_crate_config\tlane:k\n",
            "mixed_resources\tlane:hash_kind+cwd\n",
            "multi_line_composite\tlane:hash_kind+cwd\n",
            "newline_bare_attr\tnone\n",
            "prefixed_key\tlane:inner_attrs_key\n",
            "real_serial\tnone\n",
            "same_line_multi_attr\tnone\n",
            "trailing_space_before_bracket\tlane:a\n",
            "unclosed_body\tglobal\n",
            "with_inner_attrs\tlane:a\n",
        ),
        "string/char/comment literals must not produce rows, same-line attributes must be read, mixed pollution keeps both lanes, inner_attrs is not a lock key, prefixed keys are kept, multi-line attributes parse across lines, and malformed attributes (missing/mismatched delimiters) fail closed as global sites"
    );
}
