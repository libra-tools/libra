//! `tests/compat/otlp_feature_gate_guard.rs` — pins lore.md 1.7's hard
//! constraint: the `otlp` feature must NEVER leak into the default binary.
//! Textual guards (always run, default features): the feature stays out of
//! `default`, the four opentelemetry deps stay `optional = true`, and every
//! OTLP use-site stays behind `#[cfg(feature = "otlp")]`.

use std::fs;

#[test]
fn otlp_feature_stays_out_of_default() {
    let cargo = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("read Cargo.toml");
    let default_line = cargo
        .lines()
        .find(|line| line.trim_start().starts_with("default"))
        .expect("default feature line");
    assert!(
        !default_line.contains("otlp"),
        "the otlp feature must never join the default set: {default_line}"
    );
    for dep in [
        "opentelemetry =",
        "opentelemetry_sdk =",
        "opentelemetry-otlp =",
        "tracing-opentelemetry =",
    ] {
        let line = cargo
            .lines()
            .find(|line| line.trim_start().starts_with(dep))
            .unwrap_or_else(|| panic!("{dep} must be declared"));
        assert!(
            line.contains("optional = true"),
            "{dep} must stay optional: {line}"
        );
    }
}

#[test]
fn otlp_use_sites_stay_cfg_gated() {
    let core_utils_mod = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/crates/libra-core/src/utils/mod.rs"
    ))
    .expect("read libra-core utils/mod.rs");
    assert!(
        core_utils_mod.contains("#[cfg(feature = \"otlp\")]\npub mod telemetry;"),
        "the libra-core utils::telemetry module must stay feature-gated"
    );
    let utils_mod = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/utils/mod.rs"))
        .expect("read utils/mod.rs");
    assert!(
        utils_mod.contains("#[cfg(feature = \"otlp\")]\npub use libra_core::utils::telemetry;"),
        "the root utils::telemetry re-export must stay feature-gated"
    );
    let main_rs = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs"))
        .expect("read main.rs");
    for site in ["telemetry::shutdown", "telemetry::try_build_layer"] {
        for (number, line) in main_rs.lines().enumerate() {
            if line.contains(site) {
                // The cfg attribute must appear within the 3 preceding lines.
                let window = main_rs
                    .lines()
                    .skip(number.saturating_sub(3))
                    .take(3)
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(
                    window.contains("#[cfg(feature = \"otlp\")]"),
                    "{site} use at main.rs:{} must be cfg-gated:\n{window}",
                    number + 1
                );
            }
        }
    }
}
