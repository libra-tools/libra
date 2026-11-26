//! Pure commit-message signature parsing primitive sunk from `common_utils`.

use std::sync::LazyLock;

use regex::Regex;

/// Split a commit message that may begin with a `gpgsig` or `gpgsig-sha256`
/// signature block into `(body, Option<signature>)`.
#[doc(hidden)]
pub fn parse_commit_msg(msg_gpg: &str) -> (&str, Option<&str>) {
    // Accept both the SHA-1 (`gpgsig`) and SHA-256 (`gpgsig-sha256`) header
    // names that Git writes for signed commit objects.
    const SIG_PATTERN: &str = r"^gpgsig(?:-sha256)? (-----BEGIN (?:PGP|SSH) SIGNATURE-----[\s\S]*?-----END (?:PGP|SSH) SIGNATURE-----)";
    static SIG_REGEX: LazyLock<Regex> = LazyLock::new(|| {
        // INVARIANT: SIG_PATTERN is a validated regex literal checked in tests.
        Regex::new(SIG_PATTERN).expect("SIG_PATTERN must compile")
    });

    if let Some(caps) = SIG_REGEX.captures(msg_gpg) {
        // INVARIANT: SIG_PATTERN defines capture group 1 for the full signature body.
        let signature = caps
            .get(1)
            .expect("SIG_PATTERN must capture the signature body")
            .as_str();

        // Slice from the end of the whole match (`gpgsig[-sha256] <signature>`),
        // which is robust to the variable-length header prefix.
        let header_end = caps
            .get(0)
            .expect("a successful match always has group 0")
            .end();
        let msg = msg_gpg[header_end..].trim_start();
        (msg, Some(signature))
    } else {
        (msg_gpg.trim_start(), None)
    }
}
