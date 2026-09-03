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

/// Both-direction registry/classifier comparison, shared with the TA-02
/// injection counterexamples: returns every violation instead of panicking.
fn registry_diff(
    expected: &BTreeMap<String, String>,
    reg: &BTreeMap<String, (String, String)>,
) -> Vec<String> {
    let mut out = Vec::new();
    for k in expected.keys() {
        if !reg.contains_key(k.as_str()) {
            out.push(format!("missing registry row: {k}"));
        }
    }
    for k in reg.keys() {
        if !expected.contains_key(k.as_str()) {
            out.push(format!("dangling registry row: {k}"));
        }
    }
    for (k, v) in expected {
        if let Some((lane, _)) = reg.get(k)
            && lane != v
        {
            out.push(format!("{k}: registry says {lane}, classifier says {v}"));
        }
    }
    out
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
    let violations = registry_diff(&expected, &reg);
    assert!(
        violations.is_empty(),
        "registry/classifier disagreement: {violations:?}"
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

/// Count the serial attributes inside one brace-delimited region starting at
/// `open` (the byte offset of the region's opening `{`).
fn serial_attrs_in_braces(text: &str, open: usize) -> usize {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    let mut end = text.len();
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = i;
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    text[open..end]
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            t.starts_with("#[serial") || t.starts_with("#[serial_test::serial")
        })
        .count()
}

/// TA-02: site keys are CONTENT anchors —
/// `<site:path:macro:<name>#<ordinal>>` for attributes inside `macro_rules!`
/// bodies (the guard re-locates the macro by NAME and requires the body to
/// still hold at least `ordinal` serial attributes), and
/// `<site:path:orphan#<ordinal>>` for unattributable attributes outside any
/// macro. Line numbers are banned from keys outright, so editing lines above
/// an anchored attribute can never invalidate it (the failure mode that hit
/// plan-20260824 DF-05).
#[test]
fn site_rows_point_at_real_attribute_sites() {
    let mut sites = 0;
    for (key, _) in registry() {
        let Some(inner) = key.strip_prefix("<site:").and_then(|k| k.strip_suffix('>')) else {
            continue;
        };
        sites += 1;
        assert!(
            !inner
                .rsplit_once(':')
                .is_some_and(|(_, tail)| tail.parse::<usize>().is_ok()),
            "site key {key} is line-anchored; TA-02 bans line numbers in keys"
        );
        if let Some((path, rest)) = inner.split_once(":macro:") {
            let (name, ordinal) = rest
                .rsplit_once('#')
                .unwrap_or_else(|| panic!("site key {key}: missing #ordinal"));
            let ordinal: usize = ordinal
                .parse()
                .unwrap_or_else(|_| panic!("site key {key}: non-numeric ordinal"));
            assert!(ordinal >= 1, "site key {key}: ordinal must be 1-based");
            let text = std::fs::read_to_string(repo_root().join(path))
                .unwrap_or_else(|e| panic!("site key {key}: cannot read {path}: {e}"));
            let needle = format!("macro_rules! {name}");
            let mut resolved = false;
            let mut from = 0;
            while let Some(pos) = text[from..].find(&needle) {
                let at = from + pos;
                if let Some(open) = text[at..].find('{')
                    && serial_attrs_in_braces(&text, at + open) >= ordinal
                {
                    resolved = true;
                    break;
                }
                from = at + needle.len();
            }
            assert!(
                resolved,
                "site key {key}: no macro_rules! {name} body in {path} holds \
                 {ordinal} serial attribute(s)"
            );
        } else if let Some((path, rest)) = inner.split_once(":orphan#") {
            let ordinal: usize = rest
                .parse()
                .unwrap_or_else(|_| panic!("site key {key}: non-numeric ordinal"));
            let text = std::fs::read_to_string(repo_root().join(path))
                .unwrap_or_else(|e| panic!("site key {key}: cannot read {path}: {e}"));
            let n = text
                .lines()
                .filter(|l| {
                    let t = l.trim_start();
                    t.starts_with("#[serial") || t.starts_with("#[serial_test::serial")
                })
                .count();
            assert!(
                n >= ordinal,
                "site key {key}: {path} holds only {n} serial attribute(s)"
            );
        } else {
            panic!("site key {key}: neither :macro: nor :orphan# form");
        }
    }
    assert!(sites > 0, "expected at least one macro-body site row");
}

/// TA-03 standing invariant (ADR-TA-02): after the mechanical conversion,
/// `tests/**` holds ZERO unkeyed `#[serial]` — the classifier must emit no
/// `none` verdict. An unkeyed attribute locks only serial_test's
/// empty-string key, so it recreates the accidental global convoy the
/// conversion removed.
#[test]
fn no_unkeyed_serial_attributes_remain() {
    let offenders: Vec<String> = classify()
        .into_iter()
        .filter(|(_, v)| v == "none")
        .map(|(k, _)| k)
        .collect();
    assert!(
        offenders.is_empty(),
        "unkeyed #[serial] found on: {offenders:?}\n\
         Two legal fixes:\n\
         1. the test does not touch process-global state -> drop the \
         #[serial] attribute entirely; or\n\
         2. it does -> name the lane(s), e.g. #[serial(cwd)], and add the \
         matching row (lane + reason) to tests/SERIAL_REGISTRY.tsv."
    );
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

/// TA-01 counterexample: a call to a helper that resolves nowhere must fail
/// the caller closed to `global`, never `none`; and a resolvable helper whose
/// body holds pollution propagates its lane to the caller.
#[test]
fn classifier_unknown_helper_is_global() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("COMPATIBILITY.md"), b"").expect("write marker");
    std::fs::create_dir(tmp.path().join(".git")).expect("create .git");
    std::fs::create_dir_all(tmp.path().join("tests")).expect("create tests/");
    std::fs::write(
        tmp.path().join("tests/u_unknown_helper.rs"),
        concat!(
            "#[test]\n#[serial]\nfn calls_mystery() {\n    mystery_helper();\n}\n\n",
            "fn guard_helper() {\n    let _g = ChangeDirGuard;\n}\n\n",
            "#[test]\n#[serial]\nfn calls_guarded_helper() {\n    guard_helper();\n}\n",
        ),
    )
    .expect("write unknown-helper fixture");
    let out = Command::new("sh")
        .arg(repo_root().join("tests/SERIAL_CLASSIFY.sh"))
        .env("SERIAL_CLASSIFY_ROOT", tmp.path())
        .output()
        .expect("run classifier against unknown-helper fixture");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("stdout is UTF-8");
    assert_eq!(
        stdout, "calls_guarded_helper\tlane:cwd\ncalls_mystery\tglobal\n",
        "an unresolvable helper must fail closed to global, and an expanded \
         helper's pollution must propagate its lane to the caller"
    );
}

/// TA-01 counterexample: a helper that calls itself (any cycle) must fail the
/// caller closed to `global` — bounded expansion refuses cycles.
#[test]
fn classifier_recursive_helper_is_global() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("COMPATIBILITY.md"), b"").expect("write marker");
    std::fs::create_dir(tmp.path().join(".git")).expect("create .git");
    std::fs::create_dir_all(tmp.path().join("tests")).expect("create tests/");
    std::fs::write(
        tmp.path().join("tests/u_recursive_helper.rs"),
        concat!(
            "fn rec_helper(depth: u32) {\n    if depth > 0 {\n        rec_helper(depth - 1);\n    }\n}\n\n",
            "#[test]\n#[serial]\nfn calls_recursive() {\n    rec_helper(3);\n}\n",
        ),
    )
    .expect("write recursive-helper fixture");
    let out = Command::new("sh")
        .arg(repo_root().join("tests/SERIAL_CLASSIFY.sh"))
        .env("SERIAL_CLASSIFY_ROOT", tmp.path())
        .output()
        .expect("run classifier against recursive-helper fixture");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("stdout is UTF-8");
    assert_eq!(
        stdout, "calls_recursive\tglobal\n",
        "a recursive helper must fail closed to global"
    );
}

/// TA-01 counterexample: any call outside the explicit allowlist — here an
/// unknown method name — must fail the caller closed to `global`.
#[test]
fn classifier_call_outside_allowlist_is_global() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("COMPATIBILITY.md"), b"").expect("write marker");
    std::fs::create_dir(tmp.path().join(".git")).expect("create .git");
    std::fs::create_dir_all(tmp.path().join("tests")).expect("create tests/");
    std::fs::write(
        tmp.path().join("tests/u_outside_allowlist.rs"),
        concat!(
            "#[test]\n#[serial]\nfn calls_unlisted_method() {\n",
            "    let v = vec![1];\n    v.launder();\n}\n",
        ),
    )
    .expect("write outside-allowlist fixture");
    let out = Command::new("sh")
        .arg(repo_root().join("tests/SERIAL_CLASSIFY.sh"))
        .env("SERIAL_CLASSIFY_ROOT", tmp.path())
        .output()
        .expect("run classifier against outside-allowlist fixture");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("stdout is UTF-8");
    assert_eq!(
        stdout, "calls_unlisted_method\tglobal\n",
        "a call outside the allowlist must fail closed to global"
    );
}

/// TA-01 Codex-R23 counterexamples: dynamic (non-literal) set_var keys must
/// be consumed by the benign env-read gate. Three trees: (1) a test mutating
/// a benign key through a local binding — untraceable, so the benign list is
/// disabled and the reader lanes env; (2) the EnvVarGuard pattern — an assoc
/// fn forwarding its literal-called param plus a Drop restore of `self.key`
/// — fully traced, so a benign reader elsewhere KEEPS `none`; (3) a unicode
/// alias of set_var — invisible to the tokenizer, benign list disabled.
#[test]
fn classifier_dynamic_mutation_tracing() {
    let run = |files: &[(&str, &str)]| -> std::collections::HashMap<String, String> {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("COMPATIBILITY.md"), b"").expect("write marker");
        std::fs::create_dir(tmp.path().join(".git")).expect("create .git");
        std::fs::create_dir_all(tmp.path().join("tests")).expect("create tests/");
        for (name, body) in files {
            std::fs::write(tmp.path().join("tests").join(name), body).expect("write fixture");
        }
        let out = Command::new("sh")
            .arg(repo_root().join("tests/SERIAL_CLASSIFY.sh"))
            .env("SERIAL_CLASSIFY_ROOT", tmp.path())
            .output()
            .expect("run classifier against dynamic-mutation fixture");
        assert!(
            out.status.success(),
            "classifier must succeed, stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout)
            .expect("stdout is UTF-8")
            .lines()
            .filter_map(|l| {
                let (f, v) = l.split_once('\t')?;
                Some((f.to_string(), v.to_string()))
            })
            .collect()
    };
    let reader = concat!(
        "#[test]\n#[serial]\nfn benign_reader() {\n",
        "    let _ = std::env::var(\"LLVM_PROFILE_FILE\");\n}\n",
    );

    // (1) split-file dynamic mutation: the key escapes the scan, so the
    // benign list is disabled for the whole run — the reader must lane.
    let v = run(&[
        (
            "u_dyn_split.rs",
            concat!(
                "#[test]\n#[serial]\nfn mutates_benign_key_dynamically() {\n",
                "    let key = \"LLVM_PROFILE_FILE\";\n",
                "    unsafe { std::env::set_var(key, \"polluted\"); }\n}\n",
            ),
        ),
        ("u_dyn_reader.rs", reader),
    ]);
    assert_eq!(v["mutates_benign_key_dynamically"], "lane:env");
    assert_eq!(
        v["benign_reader"], "lane:env",
        "an untraceable dynamic mutation must disable the benign read list"
    );

    // (2) the EnvVarGuard pattern: assoc-fn callers all pass literals and the
    // Drop restore replays `self.key` — traced, so the benign list SURVIVES.
    let v = run(&[
        (
            "u_dyn_guard.rs",
            concat!(
                "struct G {\n    key: &'static str,\n",
                "    original: Option<std::ffi::OsString>,\n}\n\n",
                "impl G {\n",
                "    fn set(key: &'static str, value: &str) -> Self {\n",
                "        let original = std::env::var_os(key);\n",
                "        unsafe { std::env::set_var(key, value) };\n",
                "        Self { key, original }\n    }\n}\n\n",
                "impl Drop for G {\n    fn drop(&mut self) {\n",
                "        match &self.original {\n",
                "            Some(v) => unsafe { std::env::set_var(self.key, v) },\n",
                "            None => unsafe { std::env::remove_var(self.key) },\n",
                "        }\n    }\n}\n\n",
                "#[test]\n#[serial]\nfn guard_mutates_nonbenign() {\n",
                "    let _g = G::set(\"TA01_POLLUTE\", \"1\");\n}\n",
            ),
        ),
        ("u_dyn_traced_reader.rs", reader),
    ]);
    // R27 refinement: the Drop merge plus the traced caller-key evidence
    // discharge every channel to the TRUE lane (set body + drop body -> env)
    // instead of the older fail-closed global.
    assert_eq!(v["guard_mutates_nonbenign"], "lane:env");
    assert_eq!(
        v["benign_reader"], "none",
        "a fully traced guard pattern must not disable the benign read list"
    );

    // (3) a unicode alias of set_var: the tokenizer cannot see the mutation,
    // so the benign list is disabled tree-wide.
    let v = run(&[
        (
            "u_dyn_unicode.rs",
            "use std::env::set_var as \u{5199};\n\n#[test]\n#[serial]\nfn unicode_alias_env_pollutes() {\n    unsafe { \u{5199}(\"TA01_POLLUTE\", \"1\"); }\n}\n",
        ),
        ("u_dyn_unicode_reader.rs", reader),
    ]);
    assert_eq!(v["unicode_alias_env_pollutes"], "global");
    assert_eq!(
        v["benign_reader"], "lane:env",
        "an unscannable set_var alias must disable the benign read list"
    );

    // (4) a NON-LITERAL include! splices unscannable source: the file fails
    // closed and the benign list is disabled tree-wide.
    let v = run(&[
        (
            "u_dyn_include.rs",
            concat!(
                "include!(concat!(env!(\"OUT_DIR\"), \"/gen.rs\"));\n\n",
                "#[test]\n#[serial]\nfn dynamic_include_fails_closed() {\n",
                "    let _ = 1 + 1;\n}\n",
            ),
        ),
        ("u_dyn_include_reader.rs", reader),
    ]);
    assert_eq!(v["dynamic_include_fails_closed"], "global");
    assert_eq!(
        v["benign_reader"], "lane:env",
        "an unresolved include! must disable the benign read list"
    );

    // (5) an item-position macro generating a Drop impl whose set_var KEY is
    // a metavariable: the type fails closed and the benign list is disabled.
    let v = run(&[
        (
            "u_dyn_macro_drop.rs",
            concat!(
                "struct T2;\n\n",
                "macro_rules! gen_drop_dyn {\n",
                "    ($T:ident, $k:expr) => {\n",
                "        impl Drop for $T {\n",
                "            fn drop(&mut self) {\n",
                "                unsafe { std::env::set_var($k, \"1\"); }\n",
                "            }\n        }\n    };\n}\n\n",
                "gen_drop_dyn!(T2, \"SOME_KEY\");\n\n",
                "#[test]\n#[serial]\nfn metavar_key_drop_fails_closed() {\n",
                "    let _p = T2;\n}\n",
            ),
        ),
        ("u_dyn_macro_drop_reader.rs", reader),
    ]);
    assert_eq!(v["metavar_key_drop_fails_closed"], "global");
    assert_eq!(
        v["benign_reader"], "lane:env",
        "a metavariable set_var key in a generated Drop must disable the benign read list"
    );

    // (6) R33 path-argument proof POSITIVES: tempdir-derived paths, absolute
    // literals, and helper-parameter paths whose callers all prove their
    // arguments must all KEEP `none`.
    let v = run(&[(
        "u_path_proof.rs",
        concat!(
            "use std::path::Path;\n\n",
            "fn write_into(dir: &Path, name: &str) {\n",
            "    std::fs::write(dir.join(name), \"x\").unwrap();\n}\n\n",
            "#[test]\n#[serial]\nfn tempdir_derived_stays_none() {\n",
            "    let d = tempfile::tempdir().unwrap();\n",
            "    std::fs::write(d.path().join(\"f.txt\"), \"x\").unwrap();\n",
            "    write_into(d.path(), \"g.txt\");\n}\n\n",
            "#[test]\n#[serial]\nfn absolute_literal_stays_none() {\n",
            "    let _ = std::fs::read_to_string(\"/etc/hosts\");\n}\n\n",
            "#[test]\n#[serial]\nfn binding_named_tempdir_stays_none() {\n",
            "    let tempdir = tempfile::tempdir().unwrap();\n",
            "    std::fs::write(tempdir.path().join(\"f.txt\"), \"x\").unwrap();\n}\n",
        ),
    )]);
    assert_eq!(
        v["tempdir_derived_stays_none"], "none",
        "tempdir-derived and caller-proven helper paths must stay none"
    );
    assert_eq!(v["absolute_literal_stays_none"], "none");
    assert_eq!(
        v["binding_named_tempdir_stays_none"], "none",
        "a binding NAMED tempdir proven by its rhs must stay none"
    );

    // (7) R36: an ALIASED fs API with a proven tempdir argument keeps none.
    let v = run(&[(
        "u_alias_fs_safe.rs",
        concat!(
            "use std::fs::write as persist;\n\n",
            "#[test]\n#[serial]\nfn aliased_fs_write_tempdir_stays_none() {\n",
            "    let d = tempfile::tempdir().unwrap();\n",
            "    persist(d.path().join(\"f.txt\"), \"x\").unwrap();\n}\n",
        ),
    )]);
    assert_eq!(
        v["aliased_fs_write_tempdir_stays_none"], "none",
        "an aliased fs call with a proven argument must stay none"
    );
}

/// TA-02: the content-anchored site key survives LINE DRIFT — inserting
/// lines above the anchored attribute leaves the key byte-identical and
/// still resolvable, exactly the failure mode that broke the old line
/// anchor (plan-20260824 DF-05).
#[test]
fn site_key_survives_line_drift() {
    let fixture = concat!(
        "macro_rules! drift_case {\n",
        "    ($name:ident) => {\n",
        "        #[test]\n",
        "        #[serial]\n",
        "        fn $name() {\n",
        "            unsafe { std::env::set_var(\"TA02_POLLUTE\", \"1\"); }\n",
        "        }\n",
        "    };\n",
        "}\n\n",
        "drift_case!(drift_a);\n",
    );
    let run = |body: &str| -> Vec<String> {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("COMPATIBILITY.md"), b"").expect("write marker");
        std::fs::create_dir(tmp.path().join(".git")).expect("create .git");
        std::fs::create_dir_all(tmp.path().join("tests")).expect("create tests/");
        std::fs::write(tmp.path().join("tests/u_drift.rs"), body).expect("write fixture");
        let out = Command::new("sh")
            .arg(repo_root().join("tests/SERIAL_CLASSIFY.sh"))
            .env("SERIAL_CLASSIFY_ROOT", tmp.path())
            .output()
            .expect("run classifier");
        assert!(out.status.success(), "classifier failed");
        String::from_utf8(out.stdout)
            .expect("stdout is UTF-8")
            .lines()
            .filter(|l| l.starts_with("<site:"))
            .map(str::to_string)
            .collect()
    };
    let before = run(fixture);
    assert!(
        before
            .iter()
            .any(|l| l.starts_with("<site:tests/u_drift.rs:macro:drift_case#1>")),
        "expected a content-anchored site key, got: {before:?}"
    );
    let drifted =
        format!("// drift line one\n// drift line two\n\nfn unrelated_helper() {{}}\n\n{fixture}");
    let after = run(&drifted);
    assert_eq!(
        before, after,
        "inserting lines above the attribute must not move the site key"
    );

    // Codex TA-02 R1 P1: an attribute on the SAME LINE as a one-line macro
    // body but AFTER its closing brace is OUTSIDE the macro — orphan form.
    let same_line = concat!(
        "macro_rules! done { () => {}; }#[serial]\n",
        "const BAD: () = ();\n",
    );
    let keys = run(same_line);
    assert!(
        keys.iter()
            .any(|l| l.starts_with("<site:tests/u_drift.rs:orphan#1>")),
        "post-brace same-line attribute must key as orphan, got: {keys:?}"
    );
    assert!(
        !keys.iter().any(|l| l.contains(":macro:done#")),
        "post-brace attribute must not key into the closed macro: {keys:?}"
    );
}

/// TA-02 injection counterexamples: a MISSING registry row, a DANGLING row,
/// and a LANE-DRIFTED row must each be reported by the comparison the main
/// guard runs — corruption cannot pass silently.
#[test]
fn registry_injection_counterexamples() {
    let mut expected: BTreeMap<String, String> = BTreeMap::new();
    expected.insert("alpha_test".into(), "global".into());
    expected.insert("beta_test".into(), "lane:env".into());
    let clean: BTreeMap<String, (String, String)> = [
        (
            "alpha_test".to_string(),
            ("global".to_string(), "r".to_string()),
        ),
        (
            "beta_test".to_string(),
            ("lane:env".to_string(), "r".to_string()),
        ),
    ]
    .into_iter()
    .collect();
    assert!(registry_diff(&expected, &clean).is_empty());

    let mut missing = clean.clone();
    missing.remove("beta_test");
    let v = registry_diff(&expected, &missing);
    assert!(
        v.iter()
            .any(|m| m.contains("missing registry row: beta_test")),
        "a missing row must be reported: {v:?}"
    );

    let mut dangling = clean.clone();
    dangling.insert("ghost_test".into(), ("global".into(), "r".into()));
    let v = registry_diff(&expected, &dangling);
    assert!(
        v.iter()
            .any(|m| m.contains("dangling registry row: ghost_test")),
        "a dangling row must be reported: {v:?}"
    );

    let mut drifted = clean;
    drifted.insert("beta_test".into(), ("lane:cwd".into(), "r".into()));
    let v = registry_diff(&expected, &drifted);
    assert!(
        v.iter()
            .any(|m| m.contains("registry says lane:cwd, classifier says lane:env")),
        "a lane drift must be reported: {v:?}"
    );
}

/// TA-01 Codex-R22 counterexample: mutating a BENIGN env-read key through an
/// alias (`use std::env::set_var as write`) must hard-fail the classifier's
/// benign-list startup self-check, exactly like the direct spelling.
#[test]
fn classifier_rejects_aliased_benign_key_mutation() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("COMPATIBILITY.md"), b"").expect("write marker");
    std::fs::create_dir(tmp.path().join(".git")).expect("create .git");
    std::fs::create_dir_all(tmp.path().join("tests")).expect("create tests/");
    std::fs::write(
        tmp.path().join("tests/u_aliased_benign_mut.rs"),
        concat!(
            "use std::env::set_var as write;\n\n",
            "#[test]\n#[serial]\nfn mutates_benign_key_via_alias() {\n",
            "    unsafe {\n        write(\"LLVM_PROFILE_FILE\", \"polluted\");\n    }\n}\n",
        ),
    )
    .expect("write aliased-benign-mutation fixture");
    let out = Command::new("sh")
        .arg(repo_root().join("tests/SERIAL_CLASSIFY.sh"))
        .env("SERIAL_CLASSIFY_ROOT", tmp.path())
        .output()
        .expect("run classifier against aliased-benign-mutation fixture");
    assert!(
        !out.status.success(),
        "classifier must reject an aliased mutation of a benign env-read key"
    );
    let stderr = String::from_utf8(out.stderr).expect("stderr is UTF-8");
    assert!(
        stderr.contains("benign env-read key"),
        "rejection must name the benign-key invariant, got: {stderr}"
    );
}

/// TA-01 Codex-R1..R21 counterexamples: seventy-four laundering shapes that
/// once blessed real pollution/dependency as `none` must land on their true
/// lane — aliased `Command` spawns (plain, whitespace, brace-import,
/// lowercase use/type aliases), a decoy local `fn env_clear`, renamed
/// `set_var`, an aliased `ChangeDirGuard`, an imported bare
/// `std::env::current_dir`, polluting local methods behind method-only
/// allowlist names, shadowing and `#[macro_use]`-imported `macro_rules!`
/// under allowlisted names, `Self::new` chains, and (generic) type-alias
/// constructors.
#[test]
fn classifier_alias_and_local_method_laundering_fail_closed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("COMPATIBILITY.md"), b"").expect("write marker");
    std::fs::create_dir(tmp.path().join(".git")).expect("create .git");
    std::fs::create_dir_all(tmp.path().join("tests")).expect("create tests/");
    std::fs::write(
        tmp.path().join("tests/u_alias_cmd.rs"),
        concat!(
            "use std::process::Command as Cmd;\n\n",
            "#[test]\n#[serial]\nfn aliased_command_new_inherits_env() {\n",
            "    let _ = Cmd::new(\"env\").output().unwrap();\n}\n",
        ),
    )
    .expect("write aliased-command fixture");
    std::fs::write(
        tmp.path().join("tests/u_local_method.rs"),
        concat!(
            "struct Evil;\n\nimpl Evil {\n    fn execute(&self) {\n",
            "        unsafe {\n            std::env::set_var(\"TA01_POLLUTE\", \"1\");\n        }\n",
            "    }\n}\n\n",
            "#[test]\n#[serial]\nfn local_method_execute_pollutes() {\n",
            "    let evil = Evil;\n    evil.execute();\n}\n",
        ),
    )
    .expect("write local-method fixture");
    std::fs::write(
        tmp.path().join("tests/u_decoy_env_clear.rs"),
        concat!(
            "use std::process::Command;\nfn env_clear() {}\n\n",
            "#[test]\n#[serial]\nfn command_new_with_decoy_env_clear_inherits_env() {\n",
            "    env_clear();\n    let _ = Command::new(\"env\").output().unwrap();\n}\n",
        ),
    )
    .expect("write decoy-env-clear fixture");
    std::fs::write(
        tmp.path().join("tests/u_macro_shadow.rs"),
        concat!(
            "macro_rules! json {\n",
            "    () => {{ unsafe {{ std::env::set_var(\"TA01_POLLUTE\", \"1\"); }} }};\n}\n\n",
            "#[test]\n#[serial]\nfn allowlisted_macro_shadow_pollutes() {\n    json!();\n}\n",
        ),
    )
    .expect("write macro-shadow fixture");
    std::fs::write(
        tmp.path().join("tests/u_self_ctor.rs"),
        concat!(
            "struct SelfPolluter;\n\nimpl SelfPolluter {\n",
            "    fn new() -> Self {\n",
            "        unsafe {\n            std::env::set_var(\"TA01_POLLUTE\", \"1\");\n        }\n",
            "        SelfPolluter\n    }\n",
            "    fn call_new() -> Self {\n        Self::new()\n    }\n}\n\n",
            "#[test]\n#[serial]\nfn self_constructor_pollutes() {\n",
            "    let _ = SelfPolluter::call_new();\n}\n",
        ),
    )
    .expect("write self-constructor fixture");
    std::fs::write(
        tmp.path().join("tests/u_generic_alias.rs"),
        concat!(
            "struct GenPolluter;\ntype GenAlias = GenPolluter;\n\n",
            "impl GenPolluter {\n    fn new() -> Self {\n",
            "        unsafe {\n            std::env::set_var(\"TA01_POLLUTE\", \"1\");\n        }\n",
            "        GenPolluter\n    }\n}\n\n",
            "#[test]\n#[serial]\nfn generic_alias_constructor_pollutes() {\n",
            "    let _ = GenAlias::<u8>::new();\n}\n",
        ),
    )
    .expect("write generic-alias fixture");
    std::fs::write(
        tmp.path().join("tests/u_brace_alias.rs"),
        concat!(
            "use std::process::{Command as BraceCmd};\n\n",
            "#[test]\n#[serial]\nfn brace_alias_command_inherits_env() {\n",
            "    let _ = BraceCmd::new(\"env\").output().unwrap();\n}\n",
        ),
    )
    .expect("write brace-alias fixture");
    std::fs::write(
        tmp.path().join("tests/u_lowercase_use.rs"),
        concat!(
            "use std::process::Command as cmd;\n\n",
            "#[test]\n#[serial]\nfn lowercase_use_alias_command_inherits_env() {\n",
            "    let _ = cmd::new(\"env\").output().unwrap();\n}\n",
        ),
    )
    .expect("write lowercase-use fixture");
    std::fs::write(
        tmp.path().join("tests/u_lowercase_type.rs"),
        concat!(
            "type lower_command = std::process::Command;\n\n",
            "#[test]\n#[serial]\nfn lowercase_type_alias_command_inherits_env() {\n",
            "    let _ = lower_command::new(\"env\").output().unwrap();\n}\n",
        ),
    )
    .expect("write lowercase-type fixture");
    std::fs::write(
        tmp.path().join("tests/u_import_current_dir.rs"),
        concat!(
            "use std::env::current_dir;\n\n",
            "#[test]\n#[serial]\nfn env_read_dependency_hidden_by_import() {\n",
            "    let _ = current_dir().unwrap();\n}\n",
        ),
    )
    .expect("write imported-current-dir fixture");
    std::fs::write(
        tmp.path().join("tests/u_renamed_set_var.rs"),
        concat!(
            "use std::env::set_var as write;\n\n",
            "#[test]\n#[serial]\nfn alias_env_pollutes() {\n",
            "    unsafe {\n        write(\"TA01_POLLUTE\", \"1\");\n    }\n}\n",
        ),
    )
    .expect("write renamed-set-var fixture");
    std::fs::write(
        tmp.path().join("tests/u_guard_alias.rs"),
        concat!(
            "use libra::utils::test::ChangeDirGuard as G;\n\n",
            "#[test]\n#[serial]\nfn alias_cwd_guard_pollutes() {\n",
            "    let _guard = G::new(\"/\");\n}\n",
        ),
    )
    .expect("write guard-alias fixture");
    std::fs::write(
        tmp.path().join("tests/u_macro_mod.rs"),
        concat!(
            "macro_rules! format {\n",
            "    () => {{ unsafe {{ std::env::set_var(\"TA01_POLLUTE\", \"1\"); }} }};\n}\n",
        ),
    )
    .expect("write macro-mod fixture");
    std::fs::write(
        tmp.path().join("tests/u_macro_import.rs"),
        concat!(
            "#[macro_use]\nmod u_macro_mod;\n\n",
            "#[test]\n#[serial]\nfn imported_allowlisted_macro_pollutes() {\n",
            "    format!();\n}\n",
        ),
    )
    .expect("write macro-import fixture");
    std::fs::write(
        tmp.path().join("tests/u_clear_after_output.rs"),
        concat!(
            "use std::process::Command;\n\n",
            "#[test]\n#[serial]\nfn command_output_before_env_clear_inherits_env() {\n",
            "    let mut cmd = Command::new(\"env\");\n",
            "    let _ = cmd.output().unwrap();\n",
            "    cmd.env_clear();\n}\n",
        ),
    )
    .expect("write clear-after-output fixture");
    std::fs::write(
        tmp.path().join("tests/u_conditional_clear.rs"),
        concat!(
            "use std::process::Command;\n\n",
            "#[test]\n#[serial]\nfn conditional_clear_inherits_env() {\n",
            "    let mut cmd = Command::new(\"env\");\n",
            "    if false {\n        cmd.env_clear();\n    }\n",
            "    let _ = cmd.output().unwrap();\n}\n",
        ),
    )
    .expect("write conditional-clear fixture");
    std::fs::write(
        tmp.path().join("tests/u_partial_clear.rs"),
        concat!(
            "use std::process::Command;\n\n",
            "#[test]\n#[serial]\nfn two_commands_partial_clear_inherits_env() {\n",
            "    let mut a = Command::new(\"env\");\n",
            "    let mut b = Command::new(\"env\");\n",
            "    b.env_clear();\n",
            "    let _ = a.output().unwrap();\n",
            "    let _ = b.output().unwrap();\n}\n",
        ),
    )
    .expect("write partial-clear fixture");
    std::fs::write(
        tmp.path().join("tests/u_const_set_var.rs"),
        concat!(
            "const write: unsafe fn(&'static str, &'static str) =\n",
            "    std::env::set_var::<&'static str, &'static str>;\n\n",
            "#[test]\n#[serial]\nfn local_const_allowlisted_write_pollutes_env() {\n",
            "    unsafe {\n        write(\"TA01_POLLUTE\", \"1\");\n    }\n}\n",
        ),
    )
    .expect("write const-set-var fixture");
    std::fs::write(
        tmp.path().join("tests/u_const_command.rs"),
        concat!(
            "const output: fn(&'static str) -> std::process::Command =\n",
            "    std::process::Command::new::<&'static str>;\n\n",
            "#[test]\n#[serial]\nfn local_const_allowlisted_output_inherits_env() {\n",
            "    let _ = output(\"env\").output().unwrap();\n}\n",
        ),
    )
    .expect("write const-command fixture");
    std::fs::write(
        tmp.path().join("tests/u_const_closure.rs"),
        concat!(
            "const F: fn() = || unsafe {{ std::env::set_var(\"X\", \"1\") }};\n\n",
            "#[test]\n#[serial]\nfn closure_const_pollutes() {\n    F();\n}\n",
        ),
    )
    .expect("write const-closure fixture");
    std::fs::create_dir_all(tmp.path().join("tests/x_shared_helpers"))
        .expect("create shared-helper module dir");
    std::fs::write(
        tmp.path().join("tests/x_shared_helpers/mod.rs"),
        concat!(
            "use std::env::set_var as do_set;\n",
            "pub fn write() {\n    unsafe {\n        do_set(\"TA01_POLLUTE\", \"1\");\n    }\n}\n\n",
            "pub fn output() -> std::process::Output {\n",
            "    std::process::Command::new(\"env\").output().unwrap()\n}\n",
        ),
    )
    .expect("write shared-helper fixture module");
    std::fs::write(
        tmp.path().join("tests/u_shared_allowlisted.rs"),
        concat!(
            "mod x_shared_helpers;\nuse x_shared_helpers::{output, write};\n\n",
            "#[test]\n#[serial]\nfn shared_allowlisted_helper_alias_pollutes() {\n",
            "    write();\n}\n\n",
            "#[test]\n#[serial]\nfn shared_output_helper_inherits_env() {\n",
            "    let _ = output();\n}\n",
        ),
    )
    .expect("write shared-allowlisted fixture");
    std::fs::write(
        tmp.path().join("tests/u_raw_ident.rs"),
        concat!(
            "use std::process::Command as r#cmd;\n\n",
            "#[test]\n#[serial]\nfn raw_identifier_command_inherits_env() {\n",
            "    let _ = r#cmd::new(\"env\").output().unwrap();\n}\n",
        ),
    )
    .expect("write raw-ident fixture");
    std::fs::write(
        tmp.path().join("tests/u_lifetime_alias.rs"),
        concat!(
            "struct LifePolluter<'a>(&'a str);\ntype LifeAlias<'a> = LifePolluter<'a>;\n\n",
            "impl<'a> LifePolluter<'a> {\n    fn new() -> Self {\n",
            "        unsafe {\n            std::env::set_var(\"TA01_POLLUTE\", \"1\");\n        }\n",
            "        LifePolluter(\"\")\n    }\n}\n\n",
            "#[test]\n#[serial]\nfn lifetime_alias_constructor_pollutes() {\n",
            "    let _ = LifeAlias::new();\n}\n",
        ),
    )
    .expect("write lifetime-alias fixture");
    std::fs::write(
        tmp.path().join("tests/y_path_hidden.rs"),
        concat!(
            "pub fn write() {\n",
            "    unsafe {\n        std::env::set_var(\"TA01_POLLUTE\", \"1\");\n    }\n}\n",
        ),
    )
    .expect("write path-hidden fixture");
    std::fs::write(
        tmp.path().join("tests/u_path_attr.rs"),
        concat!(
            "#[path = \"y_path_hidden.rs\"]\nmod hidden;\n\n",
            "#[test]\n#[serial]\nfn path_attr_qualified_allowlisted_fn_pollutes() {\n",
            "    hidden::write();\n}\n",
        ),
    )
    .expect("write path-attr fixture");
    std::fs::write(
        tmp.path().join("tests/u_mod_qual.rs"),
        concat!(
            "mod x_shared_helpers2;\n\n",
            "#[test]\n#[serial]\nfn default_mod_qual_pollutes() {\n",
            "    x_shared_helpers2::write();\n}\n",
        ),
    )
    .expect("write mod-qual fixture");
    std::fs::create_dir_all(tmp.path().join("tests/x_shared_helpers2"))
        .expect("create second helper module dir");
    std::fs::write(
        tmp.path().join("tests/x_shared_helpers2/mod.rs"),
        concat!(
            "pub fn write() {\n",
            "    unsafe {\n        std::env::set_var(\"P\", \"1\");\n    }\n}\n",
        ),
    )
    .expect("write second helper module");
    std::fs::write(
        tmp.path().join("tests/u_where_alias.rs"),
        concat!(
            "struct WherePolluter<T>(std::marker::PhantomData<T>);\n",
            "type WhereAlias<T> where T: Copy = WherePolluter<T>;\n\n",
            "impl<T> WherePolluter<T> {\n    fn new() -> Self {\n",
            "        unsafe {\n            std::env::set_var(\"TA01_POLLUTE\", \"1\");\n        }\n",
            "        WherePolluter(std::marker::PhantomData)\n    }\n}\n\n",
            "#[test]\n#[serial]\nfn alias_where_before_equals_pollutes() {\n",
            "    let _ = WhereAlias::<u8>::new();\n}\n",
        ),
    )
    .expect("write where-alias fixture");
    std::fs::write(
        tmp.path().join("tests/u_unparsed_alias.rs"),
        concat!(
            "struct FnAliasPolluter;\ntype FnAlias = fn() -> FnAliasPolluter;\n\n",
            "impl FnAliasPolluter {\n    fn new() -> Self {\n",
            "        unsafe {\n            std::env::set_var(\"X\", \"1\");\n        }\n",
            "        FnAliasPolluter\n    }\n}\n\n",
            "#[test]\n#[serial]\nfn unparsed_alias_fails_closed() {\n",
            "    let _ = FnAlias::default();\n}\n",
        ),
    )
    .expect("write unparsed-alias fixture");
    std::fs::write(
        tmp.path().join("tests/y_hidden_type.rs"),
        concat!(
            "pub struct ModPolluter;\n",
            "impl ModPolluter {\n    pub fn new() -> Self {\n",
            "        unsafe {\n            std::env::set_var(\"TA01_POLLUTE\", \"1\");\n        }\n",
            "        ModPolluter\n    }\n}\n",
        ),
    )
    .expect("write hidden-type fixture");
    std::fs::write(
        tmp.path().join("tests/u_mod_type.rs"),
        concat!(
            "mod y_hidden_type;\nuse y_hidden_type::ModPolluter as ModAlias;\n\n",
            "#[test]\n#[serial]\nfn qualified_mod_type_constructor_pollutes() {\n",
            "    let _ = y_hidden_type::ModPolluter::new();\n}\n\n",
            "#[test]\n#[serial]\nfn imported_mod_type_constructor_pollutes() {\n",
            "    let _ = ModAlias::new();\n}\n",
        ),
    )
    .expect("write mod-type fixture");
    std::fs::write(
        tmp.path().join("tests/y_dup_a.rs"),
        concat!(
            "pub struct DupType;\n",
            "impl DupType {\n    pub fn new() -> Self {\n",
            "        unsafe {\n            std::env::set_var(\"X\", \"1\");\n        }\n",
            "        DupType\n    }\n}\n",
        ),
    )
    .expect("write dup-a fixture");
    std::fs::write(
        tmp.path().join("tests/y_dup_b.rs"),
        concat!(
            "pub struct DupType;\n",
            "impl DupType {\n    pub fn new() -> Self {\n        DupType\n    }\n}\n",
        ),
    )
    .expect("write dup-b fixture");
    std::fs::write(
        tmp.path().join("tests/u_dup_type.rs"),
        concat!(
            "#[test]\n#[serial]\nfn ambiguous_type_fails_closed() {\n",
            "    let _ = DupType::new();\n}\n",
        ),
    )
    .expect("write dup-type fixture");
    std::fs::create_dir_all(tmp.path().join("tests/y_outer/inner"))
        .expect("create nested module dirs");
    std::fs::write(tmp.path().join("tests/y_outer/mod.rs"), "pub mod inner;\n")
        .expect("write outer mod fixture");
    std::fs::write(
        tmp.path().join("tests/y_outer/inner/mod.rs"),
        concat!(
            "pub fn write() {\n",
            "    unsafe {\n        std::env::set_var(\"TA01_POLLUTE\", \"1\");\n    }\n}\n",
        ),
    )
    .expect("write inner mod fixture");
    std::fs::write(
        tmp.path().join("tests/u_nested_mod.rs"),
        concat!(
            "mod y_outer;\nuse y_outer::inner::write as nested_write;\n\n",
            "#[test]\n#[serial]\nfn nested_mod_write_pollutes() {\n",
            "    y_outer::inner::write();\n}\n\n",
            "#[test]\n#[serial]\nfn nested_import_write_pollutes() {\n",
            "    nested_write();\n}\n",
        ),
    )
    .expect("write nested-mod fixture");
    std::fs::write(
        tmp.path().join("tests/y_gap_clean.rs"),
        "pub fn write() {}\n",
    )
    .expect("write gap-clean fixture");
    std::fs::write(
        tmp.path().join("tests/y_gap_polluting.rs"),
        concat!(
            "pub fn write() {\n",
            "    unsafe {\n        std::env::set_var(\"TA01_POLLUTE\", \"1\");\n    }\n}\n",
        ),
    )
    .expect("write gap-polluting fixture");
    std::fs::write(
        tmp.path().join("tests/u_path_gap.rs"),
        concat!(
            "#[path = \"y_gap_polluting.rs\"]\n\n\nmod y_gap_clean;\n\n",
            "#[test]\n#[serial]\nfn path_attr_with_gap_uses_polluting_module() {\n",
            "    y_gap_clean::write();\n}\n",
        ),
    )
    .expect("write path-gap fixture");
    std::fs::write(
        tmp.path().join("tests/u_env_read.rs"),
        concat!(
            "#[test]\n#[serial]\nfn reads_parent_env() {\n",
            "    let _ = std::env::var(\"HOME\");\n}\n\n",
            "#[test]\n#[serial]\nfn vars_iter_lanes() {\n",
            "    for (_k, _v) in std::env::vars() {}\n}\n\n",
            "#[test]\n#[serial]\nfn reads_parent_env_with_turbofish() {\n",
            "    let _ = std::env::var::<&str>(\"HOME\");\n}\n\n",
            "#[test]\n#[serial]\nfn reads_parent_env_os_with_turbofish() {\n",
            "    let _ = std::env::var_os::<&str>(\"HOME\");\n}\n",
        ),
    )
    .expect("write env-read fixture");
    std::fs::write(
        tmp.path().join("tests/u_env_benign.rs"),
        concat!(
            "#[test]\n#[serial]\nfn benign_read_ok() {\n",
            "    let _ = std::env::var_os(\"LLVM_PROFILE_FILE\");\n}\n",
        ),
    )
    .expect("write benign-read fixture");
    {
        let pad = "x".repeat(700);
        std::fs::write(
            tmp.path().join("tests/u_long_use.rs"),
            format!(
                concat!(
                    "use std::env::{{ /* {} */ set_var as write }};\n\n",
                    "#[test]\n#[serial]\nfn long_use_alias_write_pollutes_env() {{\n",
                    "    unsafe {{ write(\"TA01_POLLUTE\", \"1\"); }}\n}}\n",
                ),
                pad
            ),
        )
        .expect("write long-use fixture");
    }
    std::fs::write(
        tmp.path().join("tests/u_terminal_hide.rs"),
        concat!(
            "mod some_terms {\n    pub fn output() {}\n}\n",
            "const output: fn() = some_terms::output;\n\n",
            "#[test]\n#[serial]\nfn const_name_must_not_hide_method_terminal() {\n",
            "    let mut cmd = std::process::Command::new(\"env\");\n",
            "    let _ = cmd.output();\n",
            "    cmd.env_clear();\n}\n",
        ),
    )
    .expect("write terminal-hide fixture");
    std::fs::write(
        tmp.path().join("tests/u_macro_metavar.rs"),
        concat!(
            "macro_rules! run_cmd {\n",
            "    ($c:ident) => {{\n",
            "        let _ = $c::new(\"env\").output().unwrap();\n",
            "    }};\n}\n\n",
            "use std::process::Command as MetaCmd;\n\n",
            "#[test]\n#[serial]\nfn macro_metavar_command_spawn_inherits_env() {\n",
            "    run_cmd!(MetaCmd);\n}\n",
        ),
    )
    .expect("write macro-metavar fixture");
    std::fs::write(
        tmp.path().join("tests/u_fn_ref_spawn.rs"),
        concat!(
            "use std::env::set_var as write;\n\n",
            "fn pollute_env() {\n",
            "    unsafe { std::env::set_var(\"TA01_POLLUTE\", \"1\"); }\n}\n\n",
            "fn pollute_via_alias() {\n",
            "    unsafe { write(\"TA01_POLLUTE\", \"1\"); }\n}\n\n",
            "#[test]\n#[serial]\nfn thread_spawn_fn_pointer_pollutes() {\n",
            "    std::thread::spawn(pollute_env).join().unwrap();\n}\n\n",
            "#[test]\n#[serial]\nfn builder_spawn_fn_pointer_pollutes() {\n",
            "    std::thread::Builder::new().spawn(pollute_env).unwrap().join().unwrap();\n}\n\n",
            "#[test]\n#[serial]\nfn let_bound_fn_pointer_pollutes() {\n",
            "    let f = pollute_env;\n",
            "    std::thread::spawn(f).join().unwrap();\n}\n\n",
            "#[test]\n#[serial]\nfn thread_spawn_aliased_pollutes() {\n",
            "    std::thread::spawn(pollute_via_alias).join().unwrap();\n}\n",
        ),
    )
    .expect("write fn-ref-spawn fixture");
    std::fs::write(
        tmp.path().join("tests/u_const_closure_value.rs"),
        concat!(
            "const F: fn() = || unsafe { std::env::set_var(\"TA01_POLLUTE\", \"1\") };\n\n",
            "#[test]\n#[serial]\nfn const_closure_spawn_value_pollutes() {\n",
            "    std::thread::spawn(F).join().unwrap();\n}\n\n",
            "#[test]\n#[serial]\nfn const_closure_let_bound_spawn_value_pollutes() {\n",
            "    let f = F;\n",
            "    std::thread::spawn(f).join().unwrap();\n}\n",
        ),
    )
    .expect("write const-closure-value fixture");
    std::fs::write(
        tmp.path().join("tests/qv_helper.rs"),
        concat!(
            "pub fn pollute_in_mod() {\n",
            "    unsafe { std::env::remove_var(\"TA01_POLLUTE\"); }\n}\n",
        ),
    )
    .expect("write qualified-value helper module");
    std::fs::write(
        tmp.path().join("tests/u_qual_value.rs"),
        concat!(
            "mod qv_helper;\n\n",
            "struct S;\n\n",
            "impl S {\n    fn pollute() {\n",
            "        unsafe { std::env::set_var(\"TA01_POLLUTE\", \"1\"); }\n    }\n}\n\n",
            "#[test]\n#[serial]\nfn assoc_fn_value_ref_pollutes() {\n",
            "    std::thread::spawn(S::pollute).join().unwrap();\n}\n\n",
            "#[test]\n#[serial]\nfn mod_fn_value_ref_pollutes() {\n",
            "    std::thread::spawn(qv_helper::pollute_in_mod).join().unwrap();\n}\n",
        ),
    )
    .expect("write qualified-value fixture");
    std::fs::create_dir_all(tmp.path().join("tests/fixtures")).expect("create tests/fixtures/");
    std::fs::write(
        tmp.path().join("tests/fixtures/polluter.rs"),
        concat!(
            "pub struct IncludedPolluter;\n",
            "impl IncludedPolluter {\n    pub fn new() -> Self {\n",
            "        unsafe { std::env::set_var(\"TA01_POLLUTE\", \"1\"); }\n",
            "        IncludedPolluter\n    }\n}\n",
        ),
    )
    .expect("write included polluter fixture");
    std::fs::write(
        tmp.path().join("tests/u_include_splice.rs"),
        concat!(
            "include!(\"fixtures/polluter.rs\");\n\n",
            "#[test]\n#[serial]\nfn include_fixture_type_pollutes() {\n",
            "    let _ = IncludedPolluter::new();\n}\n",
        ),
    )
    .expect("write include-splice fixture");
    std::fs::write(
        tmp.path().join("tests/u_drop_value.rs"),
        concat!(
            "struct DropPolluter;\n\n",
            "impl DropPolluter {\n    fn new() -> Self {\n        DropPolluter\n    }\n}\n\n",
            "impl Drop for DropPolluter {\n    fn drop(&mut self) {\n",
            "        unsafe { std::env::set_var(\"TA01_POLLUTE\", \"1\"); }\n    }\n}\n\n",
            "#[test]\n#[serial]\nfn clean_constructor_drop_pollutes() {\n",
            "    let _p = DropPolluter::new();\n}\n\n",
            "#[test]\n#[serial]\nfn drop_polluter_reaches_drop_without_call() {\n",
            "    let _p = DropPolluter;\n}\n",
        ),
    )
    .expect("write drop-value fixture");
    std::fs::write(
        tmp.path().join("tests/u_drop_macro.rs"),
        concat!(
            "struct MacroDrop;\n\n",
            "macro_rules! gen_drop {\n    () => {};\n}\n\n",
            "impl Drop for MacroDrop {\n    gen_drop!();\n}\n\n",
            "#[test]\n#[serial]\nfn macro_generated_drop_fails_closed() {\n",
            "    let _p = MacroDrop;\n}\n",
        ),
    )
    .expect("write macro-drop fixture");
    std::fs::write(
        tmp.path().join("tests/u_item_macro_drop.rs"),
        concat!(
            "struct ItemMacroDrop;\n\n",
            "macro_rules! gen_drop_impl {\n",
            "    ($T:ident) => {\n",
            "        impl Drop for $T {\n",
            "            fn drop(&mut self) {\n",
            "                unsafe { std::env::set_var(\"TA01_POLLUTE\", \"1\"); }\n",
            "            }\n        }\n    };\n}\n\n",
            "gen_drop_impl!(ItemMacroDrop);\n\n",
            "#[test]\n#[serial]\nfn item_macro_generated_metavar_drop_impl_pollutes() {\n",
            "    let _p = ItemMacroDrop;\n}\n",
        ),
    )
    .expect("write item-macro-drop fixture");
    std::fs::write(
        tmp.path().join("tests/u_alias_drop.rs"),
        concat!(
            "struct AliasDropTarget;\n",
            "type DropAlias = AliasDropTarget;\n\n",
            "impl Drop for DropAlias {\n    fn drop(&mut self) {\n",
            "        unsafe { std::env::set_var(\"TA01_POLLUTE\", \"1\"); }\n    }\n}\n\n",
            "#[test]\n#[serial]\nfn drop_impl_for_alias_pollutes_underlying_type() {\n",
            "    let _p = AliasDropTarget;\n}\n",
        ),
    )
    .expect("write alias-drop fixture");
    std::fs::write(
        tmp.path().join("tests/u_alias_macro_drop.rs"),
        concat!(
            "struct AliasMacroTarget;\n",
            "type MAlias = AliasMacroTarget;\n\n",
            "macro_rules! gen_alias_drop {\n",
            "    ($T:ident) => {\n",
            "        impl Drop for $T {\n",
            "            fn drop(&mut self) {\n",
            "                unsafe { std::env::set_var(\"TA01_POLLUTE\", \"1\"); }\n",
            "            }\n        }\n    };\n}\n\n",
            "gen_alias_drop!(MAlias);\n\n",
            "#[test]\n#[serial]\nfn macro_drop_via_alias_pollutes_underlying() {\n",
            "    let _p = AliasMacroTarget;\n}\n",
        ),
    )
    .expect("write alias-macro-drop fixture");
    std::fs::write(
        tmp.path().join("tests/mq_helper.rs"),
        concat!(
            "pub struct MqPolluter;\n",
            "pub type MqAlias = MqPolluter;\n",
        ),
    )
    .expect("write module-alias helper");
    std::fs::write(
        tmp.path().join("tests/u_mod_alias_drop.rs"),
        concat!(
            "mod mq_helper;\n\n",
            "impl Drop for mq_helper::MqAlias {\n    fn drop(&mut self) {\n",
            "        unsafe { std::env::set_var(\"TA01_POLLUTE\", \"1\"); }\n    }\n}\n\n",
            "#[test]\n#[serial]\nfn split_module_alias_drop_pollutes_underlying_type() {\n",
            "    let _p = mq_helper::MqPolluter;\n}\n",
        ),
    )
    .expect("write split-module alias-drop fixture");
    std::fs::write(
        tmp.path().join("tests/u_gen_callables.rs"),
        concat!(
            "struct GenAssocHost;\n\n",
            "macro_rules! gen_free {\n",
            "    ($name:ident) => {\n",
            "        fn $name() { unsafe { std::env::set_var(\"TA01_POLLUTE\", \"1\"); } }\n",
            "    };\n}\n\n",
            "macro_rules! gen_assoc {\n",
            "    ($name:ident) => {\n",
            "        impl GenAssocHost {\n",
            "            fn $name() -> Self {\n",
            "                unsafe { std::env::set_var(\"TA01_POLLUTE\", \"1\"); }\n",
            "                GenAssocHost\n            }\n        }\n    };\n}\n\n",
            "gen_free!(write);\n",
            "gen_assoc!(new);\n\n",
            "#[test]\n#[serial]\nfn macro_generated_allowlisted_free_fn_pollutes() {\n",
            "    write();\n}\n\n",
            "#[test]\n#[serial]\nfn macro_generated_allowlisted_assoc_fn_pollutes() {\n",
            "    let _ = GenAssocHost::new();\n}\n",
        ),
    )
    .expect("write generated-callables fixture");
    std::fs::write(
        tmp.path().join("tests/u_gen_method.rs"),
        concat!(
            "struct GenMethodHost;\n\n",
            "macro_rules! gen_method {\n",
            "    ($name:ident) => {\n",
            "        impl GenMethodHost {\n",
            "            fn $name(&self) {\n",
            "                unsafe { std::env::set_var(\"TA01_POLLUTE\", \"1\"); }\n",
            "            }\n        }\n    };\n}\n\n",
            "gen_method!(output);\n\n",
            "#[test]\n#[serial]\nfn macro_generated_allowlisted_method_pollutes() {\n",
            "    GenMethodHost.output();\n}\n",
        ),
    )
    .expect("write generated-method fixture");
    std::fs::write(
        tmp.path().join("tests/u_long_invocation.rs"),
        concat!(
            "macro_rules! gen_long {\n",
            "    ($a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $name:ident) => {\n",
            "        fn $name() { unsafe { std::env::set_var(\"TA01_POLLUTE\", \"1\"); } }\n",
            "    };\n}\n\n",
            "gen_long!(\n    la,\n    lb,\n    lc,\n    ld,\n    le,\n    lf,\n    lg,\n    write\n);\n\n",
            "#[test]\n#[serial]\nfn long_macro_invocation_generated_allowlisted_free_fn_pollutes() {\n",
            "    write();\n}\n",
        ),
    )
    .expect("write long-invocation fixture");
    std::fs::write(
        tmp.path().join("tests/u_relative_fs.rs"),
        concat!(
            "#[test]\n#[serial]\nfn relative_fs_write_depends_on_process_cwd() {\n",
            "    std::fs::write(\"ta01-relative-output\", \"x\").unwrap();\n}\n\n",
            "#[test]\n#[serial]\nfn relative_path_exists_depends_on_cwd() {\n",
            "    let _ = std::path::Path::new(\"ta01-relative-output\").exists();\n}\n\n",
            "#[test]\n#[serial]\nfn variable_name_contains_tempdir_but_relative_path_depends_on_cwd() {\n",
            "    let not_tempdir = \"ta01-relative-output\";\n",
            "    std::fs::write(not_tempdir, \"x\").unwrap();\n}\n",
        ),
    )
    .expect("write relative-fs fixture");
    std::fs::write(
        tmp.path().join("tests/u_fake_path.rs"),
        concat!(
            "struct FakePath;\n\n",
            "impl FakePath {\n    fn path(&self) -> &'static str {\n",
            "        \"ta01-relative-output\"\n    }\n}\n\n",
            "#[test]\n#[serial]\nfn fake_path_method_relative_depends_on_cwd() {\n",
            "    let fake = FakePath;\n",
            "    std::fs::write(fake.path(), \"x\").unwrap();\n}\n",
        ),
    )
    .expect("write fake-path fixture");
    std::fs::write(
        tmp.path().join("tests/u_alias_fs.rs"),
        concat!(
            "use std::fs::write as persist;\n\n",
            "#[test]\n#[serial]\nfn aliased_fs_write_relative_depends_on_cwd() {\n",
            "    persist(\"ta01-relative-output\", \"x\").unwrap();\n}\n",
        ),
    )
    .expect("write aliased-fs fixture");
    std::fs::write(
        tmp.path().join("tests/u_alias_fs_brace.rs"),
        concat!(
            "use std::fs::{write as persist};\n\n",
            "#[test]\n#[serial]\nfn brace_aliased_fs_write_relative_depends_on_cwd() {\n",
            "    persist(\"ta01-relative-output\", \"x\").unwrap();\n}\n",
        ),
    )
    .expect("write brace-aliased-fs fixture");
    std::fs::write(
        tmp.path().join("tests/u_nested_alias_fs.rs"),
        concat!(
            "use std::{fs::{write as persist2}};\n\n",
            "#[test]\n#[serial]\nfn nested_brace_alias_relative_depends_on_cwd() {\n",
            "    persist2(\"ta01-relative-output\", \"x\").unwrap();\n}\n",
        ),
    )
    .expect("write nested-brace-alias fixture");
    std::fs::write(
        tmp.path().join("tests/u_ufcs.rs"),
        concat!(
            "#[test]\n#[serial]\nfn ufcs_command_new_inherits_env() {\n",
            "    let _ = <std::process::Command>::new(\"env\").output().unwrap();\n}\n\n",
            "#[test]\n#[serial]\nfn ufcs_as_trait_command() {\n",
            "    let _ = <std::process::Command as Sized>::new(\"env\").output().unwrap();\n}\n",
        ),
    )
    .expect("write ufcs fixture");
    std::fs::write(
        tmp.path().join("tests/u_same_line_path.rs"),
        concat!(
            "#[path = \"y_gap_polluting.rs\"] mod same_line_hidden;\n\n",
            "#[test]\n#[serial]\nfn same_line_path_attr_mod_pollutes() {\n",
            "    same_line_hidden::write();\n}\n",
        ),
    )
    .expect("write same-line path-attr fixture");
    std::fs::write(
        tmp.path().join("tests/u_comment_decoy.rs"),
        concat!(
            "// var(\"LLVM_PROFILE_FILE\")\n",
            "#[test]\n#[serial]\nfn reads_parent_env_comment_gap() {\n",
            "    let _ = std::env::var/*gap*/(\"HOME\");\n}\n",
        ),
    )
    .expect("write comment-decoy fixture");
    std::fs::write(
        tmp.path().join("tests/u_multiline_read.rs"),
        concat!(
            "#[test]\n#[serial]\nfn multiline_arg_read() {\n",
            "    let _ = std::env::var(\n        \"HOME\",\n    );\n}\n",
        ),
    )
    .expect("write multiline-read fixture");
    std::fs::write(
        tmp.path().join("tests/u_type_alias.rs"),
        concat!(
            "struct Polluter;\ntype Alias = Polluter;\n\n",
            "impl Polluter {\n    fn new() -> Self {\n",
            "        unsafe {\n            std::env::set_var(\"TA01_POLLUTE\", \"1\");\n        }\n",
            "        Polluter\n    }\n}\n\n",
            "#[test]\n#[serial]\nfn alias_constructor_pollutes() {\n",
            "    let _ = Alias::new();\n}\n",
        ),
    )
    .expect("write type-alias fixture");
    let out = Command::new("sh")
        .arg(repo_root().join("tests/SERIAL_CLASSIFY.sh"))
        .env("SERIAL_CLASSIFY_ROOT", tmp.path())
        .output()
        .expect("run classifier against laundering fixtures");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("stdout is UTF-8");
    assert_eq!(
        stdout,
        concat!(
            "alias_constructor_pollutes\tlane:env\n",
            "alias_cwd_guard_pollutes\tlane:cwd\n",
            "alias_env_pollutes\tlane:env\n",
            "alias_where_before_equals_pollutes\tlane:env\n",
            "aliased_command_new_inherits_env\tlane:env\n",
            "aliased_fs_write_relative_depends_on_cwd\tlane:cwd\n",
            "allowlisted_macro_shadow_pollutes\tlane:env\n",
            "ambiguous_type_fails_closed\tglobal\n",
            "assoc_fn_value_ref_pollutes\tlane:env\n",
            "benign_read_ok\tnone\n",
            "brace_alias_command_inherits_env\tlane:env\n",
            "brace_aliased_fs_write_relative_depends_on_cwd\tlane:cwd\n",
            "builder_spawn_fn_pointer_pollutes\tlane:env\n",
            "clean_constructor_drop_pollutes\tlane:env\n",
            "closure_const_pollutes\tglobal\n",
            "command_new_with_decoy_env_clear_inherits_env\tlane:env\n",
            "command_output_before_env_clear_inherits_env\tlane:env\n",
            "conditional_clear_inherits_env\tlane:env\n",
            "const_closure_let_bound_spawn_value_pollutes\tglobal\n",
            "const_closure_spawn_value_pollutes\tglobal\n",
            "const_name_must_not_hide_method_terminal\tlane:env\n",
            "default_mod_qual_pollutes\tlane:env\n",
            "drop_impl_for_alias_pollutes_underlying_type\tlane:env\n",
            "drop_polluter_reaches_drop_without_call\tlane:env\n",
            "env_read_dependency_hidden_by_import\tlane:cwd\n",
            "fake_path_method_relative_depends_on_cwd\tlane:cwd\n",
            "generic_alias_constructor_pollutes\tlane:env\n",
            "imported_allowlisted_macro_pollutes\tlane:env\n",
            "imported_mod_type_constructor_pollutes\tlane:env\n",
            "include_fixture_type_pollutes\tlane:env\n",
            "item_macro_generated_metavar_drop_impl_pollutes\tlane:env\n",
            "let_bound_fn_pointer_pollutes\tlane:env\n",
            "lifetime_alias_constructor_pollutes\tlane:env\n",
            "local_const_allowlisted_output_inherits_env\tlane:env\n",
            "local_const_allowlisted_write_pollutes_env\tlane:env\n",
            "local_method_execute_pollutes\tlane:env\n",
            "long_macro_invocation_generated_allowlisted_free_fn_pollutes\tlane:env\n",
            "long_use_alias_write_pollutes_env\tlane:env\n",
            "lowercase_type_alias_command_inherits_env\tlane:env\n",
            "lowercase_use_alias_command_inherits_env\tlane:env\n",
            "macro_drop_via_alias_pollutes_underlying\tlane:env\n",
            "macro_generated_allowlisted_assoc_fn_pollutes\tlane:env\n",
            "macro_generated_allowlisted_free_fn_pollutes\tlane:env\n",
            "macro_generated_allowlisted_method_pollutes\tlane:env\n",
            "macro_generated_drop_fails_closed\tglobal\n",
            "macro_metavar_command_spawn_inherits_env\tglobal\n",
            "mod_fn_value_ref_pollutes\tlane:env\n",
            "multiline_arg_read\tlane:env\n",
            "nested_brace_alias_relative_depends_on_cwd\tlane:cwd\n",
            "nested_import_write_pollutes\tlane:env\n",
            "nested_mod_write_pollutes\tlane:env\n",
            "path_attr_qualified_allowlisted_fn_pollutes\tlane:env\n",
            "path_attr_with_gap_uses_polluting_module\tlane:env\n",
            "qualified_mod_type_constructor_pollutes\tlane:env\n",
            "raw_identifier_command_inherits_env\tlane:env\n",
            "reads_parent_env\tlane:env\n",
            "reads_parent_env_comment_gap\tlane:env\n",
            "reads_parent_env_os_with_turbofish\tlane:env\n",
            "reads_parent_env_with_turbofish\tlane:env\n",
            "relative_fs_write_depends_on_process_cwd\tlane:cwd\n",
            "relative_path_exists_depends_on_cwd\tlane:cwd\n",
            "same_line_path_attr_mod_pollutes\tlane:env\n",
            "self_constructor_pollutes\tlane:env\n",
            "shared_allowlisted_helper_alias_pollutes\tlane:env\n",
            "shared_output_helper_inherits_env\tlane:env\n",
            "split_module_alias_drop_pollutes_underlying_type\tlane:env\n",
            "thread_spawn_aliased_pollutes\tlane:env\n",
            "thread_spawn_fn_pointer_pollutes\tlane:env\n",
            "two_commands_partial_clear_inherits_env\tlane:env\n",
            "ufcs_as_trait_command\tlane:env\n",
            "ufcs_command_new_inherits_env\tlane:env\n",
            "unparsed_alias_fails_closed\tglobal\n",
            "variable_name_contains_tempdir_but_relative_path_depends_on_cwd\tlane:cwd\n",
            "vars_iter_lanes\tlane:env\n",
        ),
        "every rename/import/macro/constructor laundering shape must land on \
         its true lane (env or cwd), never none"
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
            "<site:tests/w_mismatched.rs:orphan#1>\tglobal\n",
            "<site:tests/x_missing_bracket.rs:orphan#1>\tglobal\n",
            "<site:tests/y_unclosed_bare.rs:orphan#1>\tglobal\n",
            "<site:tests/z_unclosed.rs:orphan#1>\tglobal\n",
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

/// plan-20260827 NP-01 (ADR-NP-01): `.config/nextest.toml` is a generated
/// artifact — regenerating it from `tests/SERIAL_REGISTRY.tsv` must reproduce
/// the committed file byte for byte, and the `external` union group must hold
/// exactly the registry-derived membership: every fn row whose lane keys
/// include an external key (cloud_live / workspace_failpoints) as an
/// anchored last-segment regex filter `test(/(^|::)<fn>$/)` — full nextest
/// names in aggregated binaries carry module paths, and fn names are
/// tree-unique, so the anchor is exact — plus every pure-global site row's
/// host target as a
/// `binary(=<target>)` filter. nextest test-groups are exclusive-membership,
/// so the union group is the only faithful mechanical derivation.
#[test]
fn nextest_groups_toml_matches_generator_and_registry() {
    let committed = std::fs::read_to_string(repo_root().join(".config/nextest.toml"))
        .expect("read .config/nextest.toml");
    let out = Command::new("sh")
        .arg(repo_root().join("tests/NEXTEST_GROUPS.sh"))
        .arg("--stdout")
        .current_dir(repo_root())
        .output()
        .expect("run tests/NEXTEST_GROUPS.sh --stdout");
    assert!(
        out.status.success(),
        "NEXTEST_GROUPS.sh failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let regenerated = String::from_utf8(out.stdout).expect("generator output is UTF-8");
    assert_eq!(
        regenerated, committed,
        ".config/nextest.toml drifted from its generator; \
         run: sh tests/NEXTEST_GROUPS.sh"
    );

    // External = any named key outside the in-process closed set
    // {cwd, env, hash_kind} (the classifier's process model). A newly named
    // key is external by default, so ADR-NP-01's "new external lanes join
    // automatically" holds mechanically (fail-safe: over-serialize at worst).
    fn external_lane(lane: &str) -> bool {
        lane.strip_prefix("lane:").is_some_and(|keys| {
            keys.split('+')
                .any(|k| !matches!(k, "cwd" | "env" | "hash_kind"))
        })
    }
    assert!(external_lane("lane:cloud_live"));
    assert!(external_lane(
        "lane:cloud_live+cwd+env+hash_kind+workspace_failpoints"
    ));
    assert!(
        external_lane("lane:some_future_service+cwd"),
        "a newly named key must be external by default"
    );
    assert!(!external_lane("lane:cwd+env+hash_kind"));
    assert!(!external_lane("lane:cwd"));
    assert!(!external_lane("global"));
    let external_key = external_lane;
    let mut expected_fns = Vec::new();
    let mut expected_bins = Vec::new();
    for (key, (lane, _)) in registry() {
        if let Some(rest) = key.strip_prefix("<site:") {
            if lane == "global" {
                let path = rest.split(':').next().expect("site key has a path");
                let target = path
                    .strip_prefix("tests/")
                    .and_then(|p| p.strip_suffix(".rs"))
                    .unwrap_or_else(|| panic!("unexpected site path shape: {path}"));
                expected_bins.push(target.to_string());
            }
        } else if external_key(&lane) {
            expected_fns.push(key);
        }
    }
    expected_fns.sort();
    expected_bins.sort();

    let mut toml_fns: Vec<String> = committed
        .lines()
        .filter_map(|l| l.strip_prefix("filter = 'test(/(^|::)"))
        .map(|l| l.trim_end_matches("$/)'").to_string())
        .collect();
    let mut toml_bins: Vec<String> = committed
        .lines()
        .filter_map(|l| l.strip_prefix("filter = 'binary(="))
        .map(|l| l.trim_end_matches(")'").to_string())
        .collect();
    toml_fns.sort();
    toml_bins.sort();

    assert_eq!(
        toml_fns, expected_fns,
        "external group test(/(^|::)fn$/) members must equal the registry-derived \
         union of cloud_live/workspace_failpoints fn rows"
    );
    assert_eq!(
        toml_bins, expected_bins,
        "external group binary(=..) members must equal the pure-global site \
         rows' host targets"
    );
    assert_eq!(toml_fns.len(), 210, "union fn member count drifted");
    assert_eq!(toml_bins.len(), 7, "site host target count drifted");
}
