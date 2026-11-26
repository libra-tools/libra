//! Internal core library crate for Libra (`publish = false`).
//!
//! Contains database, VCS core, configuration, object storage, and shared
//! utility primitives decoupled from CLI command dispatch (`src/command/**`)
//! and AI services (`src/internal/ai/**`).

#![allow(unknown_lints, clippy::large_unopt_stack_frames)]

pub mod internal;
pub mod utils;

#[cfg(test)]
mod build_version_tests {
    include!("../build_version.rs");

    #[test]
    fn extract_package_version_handles_quotes_comments_and_semver() {
        assert_eq!(
            extract_package_version("[package]\nname = \"libra\"\nversion = \"0.30.472\"\n"),
            Some("0.30.472".to_string())
        );
        assert_eq!(
            extract_package_version("[package]\nversion = \"0.30.472\" # release\n"),
            Some("0.30.472".to_string())
        );
        assert_eq!(
            extract_package_version("[package]\nversion = '1.2.3-rc.1+build.007' # single\n"),
            Some("1.2.3-rc.1+build.007".to_string())
        );
        assert_eq!(
            extract_package_version(
                "[dependencies]\nversion = \"9.9.9\"\n[package]\nversion = \"0.0.1-alpha.0\"\n"
            ),
            Some("0.0.1-alpha.0".to_string())
        );

        for invalid in [
            "1.2.3-",
            "1.2.3+",
            "1.2.3-alpha..1",
            "1.2.3+build..1",
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "1.2.3-01",
            "1.2.3-alpha_1",
            "1.2.3+build_1",
            "1.2",
            "1.2.3.4",
            "",
        ] {
            assert!(
                !is_valid_semver(invalid),
                "expected malformed semver '{invalid}' to be rejected"
            );
            let toml = format!("[package]\nversion = \"{invalid}\"\n");
            assert_eq!(
                extract_package_version(&toml),
                None,
                "expected manifest with malformed version '{invalid}' to fail"
            );
        }

        for valid in [
            "0.0.0",
            "0.30.472",
            "1.2.3-alpha",
            "1.2.3-alpha.1",
            "1.2.3-0.3.7",
            "1.2.3-x.7.z.92",
            "1.2.3-0a",
            "1.2.3+20130313144700",
            "1.2.3-beta+exp.sha.5114f85",
            "1.2.3+001",
        ] {
            assert!(
                is_valid_semver(valid),
                "expected valid semver '{valid}' to be accepted"
            );
        }

        for malformed_toml in [
            "[package]\nversion = 0.30.472\n",
            "[package]\nversion = \"0.30.472\n",
            "[package]\nversion = '0.30.472\n",
            "[package]\nversion = \"0.30.472\" trailing\n",
            "[package]\nversion = \"0.30.\\\"472\"\n",
            "[package]\nversion = \"0.30\\n.472\"\n",
            "[package]\nversion = '0.30'.'472'\n",
            "[package]\nversion = \"0.30.472'\n",
            "[dependencies]\nversion = \"0.30.472\"\n",
        ] {
            assert_eq!(
                extract_package_version(malformed_toml),
                None,
                "expected malformed TOML manifest to be rejected:\n{malformed_toml}"
            );
        }
    }
}
