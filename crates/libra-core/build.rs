include!("build_version.rs");

fn main() -> std::io::Result<()> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .map(std::path::PathBuf::from)
        .map_err(|err| std::io::Error::other(format!("CARGO_MANIFEST_DIR is not set: {err}")))?;
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .ok_or_else(|| {
            std::io::Error::other(format!(
                "expected crates/libra-core under workspace root, got '{}'",
                manifest_dir.display()
            ))
        })?;
    let root_manifest = workspace_root.join("Cargo.toml");
    if !root_manifest.is_file() {
        return Err(std::io::Error::other(format!(
            "resolved LIBRA_WORKSPACE_ROOT '{}' does not contain Cargo.toml",
            workspace_root.display()
        )));
    }
    let root_manifest_text = std::fs::read_to_string(&root_manifest)?;
    let root_version = extract_package_version(&root_manifest_text).ok_or_else(|| {
        std::io::Error::other(format!(
            "failed to read valid semver [package] version from '{}'",
            root_manifest.display()
        ))
    })?;
    println!(
        "cargo:rustc-env=LIBRA_WORKSPACE_ROOT={}",
        workspace_root.display()
    );
    println!("cargo:rustc-env=LIBRA_PKG_VERSION={root_version}");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=build_version.rs");
    println!("cargo:rerun-if-changed={}", root_manifest.display());
    Ok(())
}
