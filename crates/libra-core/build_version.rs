/// Extract and validate `[package] version = "..."` (or `'...'`) from a Cargo.toml,
/// ignoring inline `# ...` comments and validating SemVer 2.0.0.
fn extract_package_version(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for raw_line in manifest.lines() {
        let line = strip_toml_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "version" {
            continue;
        }
        let parsed = parse_toml_string_literal(value.trim())?;
        if is_valid_semver(&parsed) {
            return Some(parsed);
        }
        return None;
    }
    None
}

fn strip_toml_comment(line: &str) -> &str {
    let mut in_double = false;
    let mut in_single = false;
    let mut escaped = false;
    for (idx, ch) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_double => escaped = true,
            '"' if !in_single => in_double = !in_double,
            '\'' if !in_double => in_single = !in_single,
            '#' if !in_double && !in_single => return &line[..idx],
            _ => {}
        }
    }
    line
}

fn parse_toml_string_literal(raw: &str) -> Option<String> {
    if let Some(inner) = raw.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        if inner.contains('"') || inner.contains('\\') {
            return None;
        }
        return Some(inner.to_string());
    }
    if let Some(inner) = raw.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        if inner.contains('\'') {
            return None;
        }
        return Some(inner.to_string());
    }
    None
}

fn is_valid_semver(version: &str) -> bool {
    let (without_build, build) = match version.split_once('+') {
        Some((prefix, meta)) => (prefix, Some(meta)),
        None => (version, None),
    };
    let (core, prerelease) = match without_build.split_once('-') {
        Some((prefix, pre)) => (prefix, Some(pre)),
        None => (without_build, None),
    };

    let mut parts = core.split('.');
    let (Some(major), Some(minor), Some(patch), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    if ![major, minor, patch]
        .iter()
        .all(|p| is_valid_numeric_ident(p))
    {
        return false;
    }

    if let Some(pre) = prerelease
        && (pre.is_empty()
            || !pre.split('.').all(|ident| {
                !ident.is_empty()
                    && ident
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    && (!ident.bytes().all(|b| b.is_ascii_digit()) || is_valid_numeric_ident(ident))
            }))
    {
        return false;
    }

    if let Some(meta) = build
        && (meta.is_empty()
            || !meta.split('.').all(|ident| {
                !ident.is_empty()
                    && ident
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            }))
    {
        return false;
    }

    true
}

fn is_valid_numeric_ident(s: &str) -> bool {
    !s.is_empty()
        && s.bytes().all(|b| b.is_ascii_digit())
        && (s.len() == 1 || !s.starts_with('0'))
}
