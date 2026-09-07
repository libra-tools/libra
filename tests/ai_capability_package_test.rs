//! CEX-S2-17 (Step 2.7) — `libra package` capability-package lifecycle.
//!
//! The card's verification target `cargo test ai_capability_package`. Drives the
//! real `command::package` CLI over a temp Libra repo through the full lifecycle
//! — install (vet + record, default-deny), list, diff, update (changed checksum
//! re-prompts), and uninstall — asserting the per-repo store
//! (`.libra/capability_packages.json`) is the single source of truth.

use std::path::Path;

use libra::{
    command::package::{self, PackageCmds},
    internal::ai::{
        agent_run::PackageId,
        capability_package::{
            BundledCapabilities, CapabilityPackageManifest, InstalledPackageStore, MANIFEST_FILE,
            compute_package_checksum,
        },
    },
    utils::{
        output::OutputConfig,
        test::{ChangeDirGuard, setup_with_new_libra_in},
    },
};

/// Write a package directory: `manifest.json` (checksum computed over the
/// content) plus one skill file and, optionally, a mutating source declaration.
fn write_package(dir: &Path, id: &str, body: &str, with_source: bool) {
    std::fs::create_dir_all(dir.join("skills")).unwrap();
    std::fs::write(dir.join("skills/explore.md"), body).unwrap();
    let entries = vec![("skills/explore.md".to_string(), body.as_bytes().to_vec())];
    let manifest = CapabilityPackageManifest {
        package_id: PackageId(id.to_string()),
        version: "1.0.0".to_string(),
        publisher: "acme".to_string(),
        checksum: compute_package_checksum(&entries),
        bundled: BundledCapabilities {
            skills: vec!["explore".to_string()],
            sources: if with_source {
                vec!["acme-src".to_string()]
            } else {
                Vec::new()
            },
            ..BundledCapabilities::default()
        },
        requested_permissions: Vec::new(),
        install_warnings: Vec::new(),
    };
    std::fs::write(
        dir.join(MANIFEST_FILE),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
#[serial_test::serial(cwd)]
async fn package_install_list_diff_uninstall_lifecycle() {
    let repo = tempfile::tempdir().unwrap();
    setup_with_new_libra_in(repo.path()).await;
    let _guard = ChangeDirGuard::new(repo.path());
    let output = OutputConfig::default();

    let pkg_dir = repo.path().join("pkg");
    // A package bundling no mutating capability installs without --yes.
    write_package(&pkg_dir, "acme.skillpack", "explore body", false);

    // diff is read-only and must not touch the store.
    package::execute_safe(
        PackageCmds::Diff {
            path: pkg_dir.clone(),
        },
        &output,
    )
    .await
    .expect("diff");

    let store = InstalledPackageStore::new(repo.path().join(".libra"));
    assert!(store.load().unwrap().is_empty(), "diff must not install");

    // install + enable records the package (default-deny overridden by --enable).
    package::execute_safe(
        PackageCmds::Install {
            path: pkg_dir.clone(),
            yes: true,
            enable: true,
        },
        &output,
    )
    .await
    .expect("install");

    let installed = store.load().unwrap();
    assert_eq!(installed.len(), 1, "the package is recorded");
    assert_eq!(installed[0].manifest.package_id.0, "acme.skillpack");
    assert!(installed[0].enabled, "--enable persisted");

    package::execute_safe(PackageCmds::List, &output)
        .await
        .expect("list");

    // Re-installing identical content is an idempotent update (no duplicate).
    package::execute_safe(
        PackageCmds::Install {
            path: pkg_dir.clone(),
            yes: true,
            enable: true,
        },
        &output,
    )
    .await
    .expect("reinstall");
    assert_eq!(store.load().unwrap().len(), 1, "update must not duplicate");

    // Uninstall drops it from the store.
    package::execute_safe(
        PackageCmds::Uninstall {
            package_id: "acme.skillpack".to_string(),
        },
        &output,
    )
    .await
    .expect("uninstall");
    assert!(
        store.load().unwrap().is_empty(),
        "uninstall clears the store"
    );
}

#[tokio::test]
#[serial_test::serial(cwd)]
async fn package_install_auto_enables_non_mutating_but_defers_mutating() {
    let repo = tempfile::tempdir().unwrap();
    setup_with_new_libra_in(repo.path()).await;
    let _guard = ChangeDirGuard::new(repo.path());
    let output = OutputConfig::default();
    let store = InstalledPackageStore::new(repo.path().join(".libra"));

    // A skills-only package grants no mutating capability → auto-enabled even
    // though the user did not pass --enable (CEX-S2-17 验收 3).
    let safe_dir = repo.path().join("safe");
    write_package(&safe_dir, "acme.skillpack", "explore body", false);
    package::execute_safe(
        PackageCmds::Install {
            path: safe_dir,
            yes: false,
            enable: false,
        },
        &output,
    )
    .await
    .expect("install skills-only");
    let safe = store
        .load()
        .unwrap()
        .into_iter()
        .find(|p| p.manifest.package_id.0 == "acme.skillpack")
        .expect("recorded");
    assert!(
        safe.enabled,
        "a non-mutating package auto-enables on install"
    );

    // A package bundling a mutating source stays default-deny disabled when the
    // user accepts (--yes) but does not pass --enable.
    let mutating_dir = repo.path().join("mutating");
    write_package(&mutating_dir, "acme.toolkit", "explore body", true);
    package::execute_safe(
        PackageCmds::Install {
            path: mutating_dir,
            yes: true,
            enable: false,
        },
        &output,
    )
    .await
    .expect("install mutating");
    let mutating = store
        .load()
        .unwrap()
        .into_iter()
        .find(|p| p.manifest.package_id.0 == "acme.toolkit")
        .expect("recorded");
    assert!(
        !mutating.enabled,
        "a mutating package stays default-deny disabled without --enable",
    );
}

#[tokio::test]
#[serial_test::serial(cwd)]
async fn package_install_rejects_tampered_content() {
    let repo = tempfile::tempdir().unwrap();
    setup_with_new_libra_in(repo.path()).await;
    let _guard = ChangeDirGuard::new(repo.path());
    let output = OutputConfig::default();

    let pkg_dir = repo.path().join("pkg");
    write_package(&pkg_dir, "acme.toolkit", "declared body", true);
    // Tamper with the content after the manifest's checksum was computed.
    std::fs::write(pkg_dir.join("skills/explore.md"), b"TAMPERED body").unwrap();

    let err = package::execute_safe(
        PackageCmds::Install {
            path: pkg_dir,
            yes: true,
            enable: true,
        },
        &output,
    )
    .await
    .expect_err("tampered content must be rejected");
    assert!(
        err.render().to_lowercase().contains("checksum")
            || err.render().to_lowercase().contains("verification"),
        "rejection must cite the integrity failure: {}",
        err.render(),
    );

    // Nothing was recorded.
    let store = InstalledPackageStore::new(repo.path().join(".libra"));
    assert!(
        store.load().unwrap().is_empty(),
        "a tampered package must register nothing",
    );
}
