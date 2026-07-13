use super::{DiffArgs, DiffError};

const DEFAULT_CONTEXT: usize = 3;
const DEFAULT_RENAME_SCORE: u32 = 30000;

#[derive(Clone)]
pub(super) struct ResolvedDiffConfig {
    pub(super) context: usize,
    pub(super) rename_threshold: Option<u32>,
    pub(super) prefixes: DiffPrefixes,
}

#[derive(Clone)]
pub(super) struct DiffPrefixes {
    pub(super) source: String,
    pub(super) destination: String,
}

/// Resolve every supported `diff.*` default before progress or diff scanning.
/// Explicit CLI flags bypass the corresponding config key, matching Git's
/// precedence and ensuring invalid unused defaults cannot override the CLI.
pub(super) async fn resolve_diff_config(args: &DiffArgs) -> Result<ResolvedDiffConfig, DiffError> {
    let context = match args.unified {
        Some(context) => context,
        None => configured_diff_context().await?.unwrap_or(DEFAULT_CONTEXT),
    };
    let rename_threshold = match resolve_rename_threshold(args)? {
        Some(threshold) => Some(threshold),
        None if args.no_renames => None,
        None => configured_diff_renames().await?,
    };
    let prefixes = configured_diff_prefixes(args).await?;
    Ok(ResolvedDiffConfig {
        context,
        rename_threshold,
        prefixes,
    })
}

/// Read one `diff.*` config default through the strict local→global→system
/// cascade, mapping read failures fail-closed to [`DiffError`].
async fn read_diff_config(key: &'static str) -> Result<Option<String>, DiffError> {
    crate::internal::config::read_cascaded_config_value_strict(
        crate::internal::config::LocalIdentityTarget::CurrentRepo,
        key,
    )
    .await
    .map(|value| value.map(|v| v.trim().to_string()))
    .map_err(|error| DiffError::DiffConfigRead {
        key,
        detail: format!("{error:#}"),
    })
}

async fn read_raw_diff_config(key: &'static str) -> Result<Option<String>, DiffError> {
    crate::internal::config::read_cascaded_config_value_strict(
        crate::internal::config::LocalIdentityTarget::CurrentRepo,
        key,
    )
    .await
    .map_err(|error| DiffError::DiffConfigRead {
        key,
        detail: format!("{error:#}"),
    })
}

async fn configured_diff_bool(key: &'static str) -> Result<bool, DiffError> {
    let Some(value) = read_diff_config(key).await? else {
        return Ok(false);
    };
    crate::internal::config::parse_git_config_bool(&value)
        .ok_or(DiffError::InvalidDiffConfig { key, value })
}

async fn configured_diff_prefixes(args: &DiffArgs) -> Result<DiffPrefixes, DiffError> {
    if let (Some(source), Some(destination)) = (&args.src_prefix, &args.dst_prefix) {
        let (mut source, mut destination) = (source.clone(), destination.clone());
        if args.reverse {
            std::mem::swap(&mut source, &mut destination);
        }
        return Ok(DiffPrefixes {
            source,
            destination,
        });
    }
    let no_prefix = configured_diff_bool("diff.noPrefix").await?;
    let mnemonic = configured_diff_bool("diff.mnemonicPrefix").await?;
    let configured_source = if args.src_prefix.is_none() && !no_prefix && !mnemonic {
        read_raw_diff_config("diff.srcPrefix").await?
    } else {
        None
    };
    let configured_destination = if args.dst_prefix.is_none() && !no_prefix && !mnemonic {
        read_raw_diff_config("diff.dstPrefix").await?
    } else {
        None
    };

    let (mut source, mut destination) = if no_prefix {
        (String::new(), String::new())
    } else if mnemonic {
        mnemonic_prefixes(args)
    } else {
        (
            configured_source.unwrap_or_else(|| "a/".to_string()),
            configured_destination.unwrap_or_else(|| "b/".to_string()),
        )
    };
    if let Some(explicit) = &args.src_prefix {
        source.clone_from(explicit);
    }
    if let Some(explicit) = &args.dst_prefix {
        destination.clone_from(explicit);
    }
    if args.reverse {
        std::mem::swap(&mut source, &mut destination);
    }
    Ok(DiffPrefixes {
        source,
        destination,
    })
}

fn mnemonic_prefixes(args: &DiffArgs) -> (String, String) {
    let pair = if args.staged {
        ("c/", "i/")
    } else if args.old.is_some() && args.new.is_none() {
        ("c/", "w/")
    } else if args.old.is_some() {
        ("c/", "c/")
    } else {
        ("i/", "w/")
    };
    (pair.0.to_string(), pair.1.to_string())
}

/// `diff.context`: non-negative Git integer, default 3. CLI `-U` wins.
async fn configured_diff_context() -> Result<Option<usize>, DiffError> {
    let Some(value) = read_diff_config("diff.context").await? else {
        return Ok(None);
    };
    let normalized = value.to_ascii_lowercase();
    crate::internal::config::parse_git_config_int(&normalized)
        .and_then(|number| i32::try_from(number).ok())
        .and_then(|number| usize::try_from(number).ok())
        .map(Some)
        .ok_or(DiffError::InvalidDiffConfig {
            key: "diff.context",
            value,
        })
}

/// `diff.renames`: Git boolean or `copies`/`copy`. Unset and truthy values use
/// Git's 50% default; false disables detection. Copy detection degrades to
/// rename detection because Libra does not expose `-C`.
async fn configured_diff_renames() -> Result<Option<u32>, DiffError> {
    let Some(value) = read_diff_config("diff.renames").await? else {
        return Ok(Some(DEFAULT_RENAME_SCORE));
    };
    let normalized = value.to_ascii_lowercase();
    if normalized == "copies" || normalized == "copy" {
        return Ok(Some(DEFAULT_RENAME_SCORE));
    }
    match crate::internal::config::parse_git_config_bool(&value) {
        Some(true) => Ok(Some(DEFAULT_RENAME_SCORE)),
        Some(false) => Ok(None),
        None => Err(DiffError::InvalidDiffConfig {
            key: "diff.renames",
            value,
        }),
    }
}

fn resolve_rename_threshold(args: &DiffArgs) -> Result<Option<u32>, DiffError> {
    if args.no_renames {
        return Ok(None);
    }
    let Some(raw) = args.find_renames.as_ref() else {
        return Ok(None);
    };
    let score = parse_rename_score(raw)?;
    // Git treats a zero minimum score as the 50% default before pairing.
    Ok(Some(if score == 0 {
        DEFAULT_RENAME_SCORE
    } else {
        score
    }))
}

/// Parse Git's `-M` score syntax onto the 0..60000 similarity scale.
pub(super) fn parse_rename_score(raw: &str) -> Result<u32, DiffError> {
    let invalid = || DiffError::InvalidRenameScore(raw.to_string());
    let parse_decimal = |s: &str| -> Option<(u128, u128)> {
        let mut num = 0u128;
        let mut denom = 1u128;
        let mut seen_dot = false;
        let mut any_digit = false;
        const CAP: u128 = 1_000_000_000_000;
        for byte in s.bytes() {
            match byte {
                b'.' if !seen_dot => seen_dot = true,
                b'0'..=b'9' => {
                    any_digit = true;
                    if num < CAP && denom < CAP {
                        num = num * 10 + u128::from(byte - b'0');
                        if seen_dot {
                            denom *= 10;
                        }
                    }
                }
                _ => return None,
            }
        }
        any_digit.then_some((num, denom))
    };
    let (num, denom) = if let Some(body) = raw.strip_suffix('%') {
        let (number, divisor) = parse_decimal(body).ok_or_else(invalid)?;
        (number, divisor * 100)
    } else if raw.contains('.') {
        parse_decimal(raw).ok_or_else(invalid)?
    } else {
        parse_decimal(&format!("0.{raw}")).ok_or_else(invalid)?
    };
    const MAX_SCORE: u128 = 60000;
    let score = if num >= denom {
        MAX_SCORE
    } else {
        MAX_SCORE * num / denom
    };
    u32::try_from(score).map_err(|_| invalid())
}
