//! Credential redaction helpers shared across command output and network
//! clients (fetch/remote/push, the D1 client, and the git-over-HTTPS client).
//!
//! The canonical entry point is `redact_url_credentials`, which strips
//! embedded userinfo (`user:token@`) from a URL before it is printed to a
//! terminal, written to a log, or embedded in an error message. Historically
//! this lived in `command::fetch`; it was hoisted here so `utils`-level network
//! clients can reuse it without depending on a command module (which would
//! invert the dependency direction).

use url::Url;

/// Strip embedded credentials (userinfo) from a URL before printing it to the
/// terminal or a log. Falls back to the original string if the URL cannot be
/// parsed (e.g. SCP-style `git@host:path`).
///
/// For SSH URLs, a bare username without a password (e.g. `git@`) is the
/// standard convention and is NOT redacted. Only URLs that carry a password
/// component or an HTTP(S) username (which is typically a token) are stripped.
///
/// # Arguments
/// * `raw` - the URL (or URL-like string) to redact.
///
/// # Returns
/// A URL string with any sensitive userinfo removed.
pub(crate) fn redact_url_credentials(raw: &str) -> String {
    match Url::parse(raw) {
        Ok(mut url) => {
            let raw_userinfo = url_userinfo(raw);
            let has_password = url.password().is_some()
                || raw_userinfo.is_some_and(|userinfo| userinfo.contains(':'));
            let is_http = matches!(url.scheme(), "http" | "https");
            let has_http_username =
                is_http && (!url.username().is_empty() || raw_userinfo.is_some());
            // Redact when there is a password (always sensitive) or when the
            // scheme is HTTP(S) and a username is present (likely a token).
            // For SSH, a bare username like "git" is conventional and harmless.
            if has_password || has_http_username {
                let _ = url.set_username("");
                let _ = url.set_password(None);
                return strip_url_userinfo(url.as_str()).unwrap_or_else(|| url.to_string());
            }
            url.to_string()
        }
        Err(_) => {
            let raw_userinfo = url_userinfo(raw);
            let is_http = url_scheme(raw).is_some_and(|scheme| {
                scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
            });
            if raw_userinfo.is_some_and(|userinfo| userinfo.contains(':'))
                || (is_http && raw_userinfo.is_some())
            {
                strip_url_userinfo(raw).unwrap_or_else(|| raw.to_string())
            } else {
                raw.to_string()
            }
        }
    }
}

fn url_scheme(url: &str) -> Option<&str> {
    url.find("://").map(|scheme_end| &url[..scheme_end])
}

fn url_userinfo(url: &str) -> Option<&str> {
    let authority_start = url.find("://")? + 3;
    let authority_len = url[authority_start..]
        .find(['/', '?', '#'])
        .unwrap_or(url.len() - authority_start);
    let authority_end = authority_start + authority_len;
    let userinfo_end = url[authority_start..authority_end].rfind('@')?;

    Some(&url[authority_start..authority_start + userinfo_end])
}

fn strip_url_userinfo(url: &str) -> Option<String> {
    let authority_start = url.find("://")? + 3;
    let authority_len = url[authority_start..]
        .find(['/', '?', '#'])
        .unwrap_or(url.len() - authority_start);
    let authority_end = authority_start + authority_len;
    let userinfo_end = url[authority_start..authority_end].rfind('@')?;
    let host_start = authority_start + userinfo_end + 1;
    if host_start == authority_end {
        return None;
    }

    let mut redacted = String::with_capacity(url.len());
    redacted.push_str(&url[..authority_start]);
    redacted.push_str(&url[host_start..]);
    Some(redacted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_http_token_userinfo() {
        assert_eq!(
            redact_url_credentials("https://x-access-token:ghp_secret@github.com/o/r.git"),
            "https://github.com/o/r.git"
        );
    }

    #[test]
    fn strips_password_only() {
        assert_eq!(
            redact_url_credentials("https://user:pw@example.com/repo.git"),
            "https://example.com/repo.git"
        );
    }

    #[test]
    fn keeps_bare_ssh_username() {
        // A bare SSH username with no password is conventional and safe.
        assert_eq!(
            redact_url_credentials("ssh://git@example.com/repo.git"),
            "ssh://git@example.com/repo.git"
        );
    }

    #[test]
    fn redacts_ssh_password() {
        assert_eq!(
            redact_url_credentials("ssh://git:secret@example.com/repo.git"),
            "ssh://example.com/repo.git"
        );
    }

    #[test]
    fn unparseable_scp_style_passes_through() {
        // SCP-style has no scheme; without `://` there is nothing to strip.
        assert_eq!(
            redact_url_credentials("git@example.com:org/repo.git"),
            "git@example.com:org/repo.git"
        );
    }

    #[test]
    fn no_userinfo_is_unchanged() {
        assert_eq!(
            redact_url_credentials("https://example.com/repo.git"),
            "https://example.com/repo.git"
        );
    }
}
