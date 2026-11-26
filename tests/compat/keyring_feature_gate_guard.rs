//! `tests/compat/keyring_feature_gate_guard.rs` — pins lore.md 2.7's gating:
//! the `keyring` feature stays out of `default` (dev/CI default builds carry
//! no D-Bus machinery), the dep stays optional, and the backend module plus
//! its use-sites stay behind `#[cfg(feature = "keyring")]`. (Release builds
//! opt in explicitly — see .github/workflows/release.yml.)

use std::fs;

#[test]
fn keyring_feature_stays_out_of_default() {
    let cargo = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("read Cargo.toml");
    let default_line = cargo
        .lines()
        .find(|line| line.trim_start().starts_with("default"))
        .expect("default feature line");
    assert!(
        !default_line.contains("keyring"),
        "the keyring feature must never join the default set: {default_line}"
    );
    let dep_line = cargo
        .lines()
        .find(|line| line.trim_start().starts_with("keyring = {"))
        .expect("keyring dep line");
    assert!(
        dep_line.contains("optional = true"),
        "the keyring dep must stay optional: {dep_line}"
    );
    assert!(
        dep_line.contains("vendored"),
        "Linux secret-service must use VENDORED libdbus (static — no runtime \
         dylib dependency on end-user machines): {dep_line}"
    );
}

#[test]
fn keyring_use_sites_stay_cfg_gated() {
    let auth_rs = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/crates/libra-core/src/internal/auth.rs"
    ))
    .expect("read auth.rs");
    assert!(
        auth_rs.contains("#[cfg(feature = \"keyring\")]\nmod keyring_backend {"),
        "the keyring backend module must stay feature-gated"
    );
}
