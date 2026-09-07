//! `tests/compat/fastcdc_feature_gate_guard.rs` — pins lore.md §6's hard
//! constraint (lore.md §4.0 "feature gating 必须严格", `:320` as of
//! 2026-08-27): the `fastcdc` FastCDC
//! media-chunking feature must NEVER leak into the default binary. Textual
//! guards (always run, DEFAULT features): the feature stays out of `default`,
//! stays a pure in-tree feature (`fastcdc = []`, no bundled deps), and every
//! media use-site stays behind `#[cfg(feature = "fastcdc")]`.

use std::fs;

#[test]
fn fastcdc_feature_stays_out_of_default() {
    let cargo = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("read Cargo.toml");
    let default_line = cargo
        .lines()
        .find(|line| line.trim_start().starts_with("default"))
        .expect("default feature line");
    assert!(
        !default_line.contains("fastcdc"),
        "the fastcdc feature must never join the default set: {default_line}"
    );
    let fastcdc_line = cargo
        .lines()
        .find(|line| line.trim_start().starts_with("fastcdc"))
        .expect("fastcdc feature line must exist");
    // v1 adds no new crates: the feature stays `fastcdc = []`. If a future phase
    // bundles an optional dep, this guard must be updated to assert it stays
    // `optional = true` rather than joining the default graph.
    assert!(
        fastcdc_line.contains("[]"),
        "the fastcdc feature must stay a pure in-tree feature (no bundled deps): {fastcdc_line}"
    );
}

#[test]
fn fastcdc_use_sites_stay_cfg_gated() {
    let utils_mod = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/utils/mod.rs"))
        .expect("read utils/mod.rs");
    assert!(
        utils_mod.contains("#[cfg(feature = \"fastcdc\")]\npub mod media;"),
        "the utils::media module must stay feature-gated"
    );
    let command_mod =
        fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/command/mod.rs"))
            .expect("read command/mod.rs");
    assert!(
        command_mod.contains("#[cfg(feature = \"fastcdc\")]\npub mod media;"),
        "the command::media module must stay feature-gated"
    );
    // The cli.rs `Media` enum variant and its dispatch arm must be cfg-gated so
    // the default binary has no media command.
    let cli = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/cli.rs"))
        .expect("read cli.rs");
    for site in [
        "Media(command::media::MediaArgs)",
        "Commands::Media(cmd_args)",
    ] {
        for (number, line) in cli.lines().enumerate() {
            if line.contains(site) {
                // Window wide enough to span the multi-line `#[command(…)]`
                // attribute that sits between the `#[cfg]` and the enum variant.
                let window = cli
                    .lines()
                    .skip(number.saturating_sub(7))
                    .take(7)
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(
                    window.contains("#[cfg(feature = \"fastcdc\")]"),
                    "{site} at cli.rs:{} must be cfg-gated:\n{window}",
                    number + 1
                );
            }
        }
    }
}
