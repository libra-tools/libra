//! plan-20260714 PD-00 follow-up guard: the FIVE release version surfaces
//! must stay in lockstep.
//!
//! PD-00 closed a drift where `Cargo.toml`, `web/package.json` and
//! `worker/package.json` were bumped but `install.sh`'s `DEFAULT_VERSION`
//! fallback was left behind — a stale fallback silently installs an OLD
//! binary whenever the release API is unreachable and
//! `LIBRA_ALLOW_FALLBACK=1` is set. The card's explicit follow-up
//! condition ("if it drifts again, add a guard that reads all four files
//! and asserts they agree") is what this target implements.
//! PD-10 later added the Windows installer as a fifth surface, so the guard
//! now includes its `$DefaultVersion` too.

use std::{fs, path::Path};

fn read(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

fn cargo_version() -> String {
    let text = read("Cargo.toml");
    // The workspace package version is the first `version = "…"` under
    // `[package]`, which is the first table in this manifest.
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("version = \"")
            && let Some(value) = rest.strip_suffix('"')
        {
            return value.to_string();
        }
    }
    panic!("Cargo.toml has no package version line");
}

fn package_json_version(relative: &str) -> String {
    let text = read(relative);
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("\"version\": \"")
            && let Some(value) = rest.strip_suffix("\",").or_else(|| rest.strip_suffix('"'))
        {
            return value.to_string();
        }
    }
    panic!("{relative} has no version field");
}

/// The RAW `$DefaultVersion` value from the Windows installer, exactly as
/// `install.ps1` will use it.
///
/// This surface was invisible to the guard until PD-10 and had drifted 66
/// patch releases behind (`v0.18.15` against a 0.19.81 manifest). Like the
/// POSIX one, the value is substituted verbatim into the download URL, so a
/// stale value silently installs an old binary whenever `-Version` is not
/// passed.
fn install_ps1_default_version_raw() -> String {
    let text = read("install.ps1");
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("$DefaultVersion")
            && let Some(rest) = rest.trim_start().strip_prefix('=')
            && let Some(value) = rest.trim().strip_prefix('"')
            && let Some(value) = value.strip_suffix('"')
        {
            return value.to_string();
        }
    }
    panic!("install.ps1 has no $DefaultVersion assignment");
}

/// The RAW `DEFAULT_VERSION` value, exactly as `install.sh` will use it.
///
/// Deliberately not normalized: the shell substitutes this string verbatim
/// into the download URL, so `vv0.19.76` is a broken tag, not a spelling
/// variant. An earlier `trim_start_matches('v')` here stripped ANY number of
/// leading `v`s, which let exactly that typo satisfy the guard.
fn install_default_version_raw() -> String {
    let text = read("install.sh");
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("DEFAULT_VERSION=\"")
            && let Some(value) = rest.strip_suffix('"')
        {
            return value.to_string();
        }
    }
    panic!("install.sh has no DEFAULT_VERSION assignment");
}

#[test]
fn all_release_version_surfaces_agree() {
    let cargo = cargo_version();
    let web = package_json_version("web/package.json");
    let worker = package_json_version("worker/package.json");
    let install = install_default_version_raw();
    let install_ps1 = install_ps1_default_version_raw();

    assert_eq!(
        web, cargo,
        "web/package.json version must match Cargo.toml ({cargo})"
    );
    assert_eq!(
        worker, cargo,
        "worker/package.json version must match Cargo.toml ({cargo})"
    );
    // Exact equality against the ONE canonical spelling, `v<cargo>`. The
    // value is used verbatim by the shell, so anything else — a stale
    // version, a missing prefix, or a doubled one — is a broken download.
    assert_eq!(
        install_ps1,
        format!("v{cargo}"),
        "install.ps1 $DefaultVersion must be exactly \"v{cargo}\" — the Windows \
         installer substitutes it verbatim into the release URL just like the \
         POSIX one, and it is not covered by any other guard"
    );
    assert_eq!(
        install,
        format!("v{cargo}"),
        "install.sh DEFAULT_VERSION must be exactly \"v{cargo}\" — it is \
         substituted verbatim into the release URL, so a stale value installs \
         an old binary when the release API is unreachable and a malformed \
         one installs nothing at all"
    );
}

#[test]
fn install_default_version_carries_the_release_tag_prefix() {
    let text = read("install.sh");
    let line = text
        .lines()
        .find(|line| line.starts_with("DEFAULT_VERSION="))
        .expect("install.sh DEFAULT_VERSION");
    let value = install_default_version_raw();
    let semver = value.strip_prefix('v').unwrap_or_else(|| {
        panic!("DEFAULT_VERSION keeps the `v<semver>` release-tag spelling: {line}")
    });
    assert!(
        !semver.starts_with('v'),
        "exactly ONE `v` — `{value}` would be substituted into the release URL as written: {line}"
    );
    assert!(
        semver.split('.').count() == 3
            && semver
                .split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())),
        "DEFAULT_VERSION is `v<major>.<minor>.<patch>`, not `{value}`: {line}"
    );
}
