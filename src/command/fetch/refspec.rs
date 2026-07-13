//! Fetch refspec parsing and source-to-destination expansion.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FetchRefspec {
    source: String,
    destination: String,
    pub(crate) force: bool,
    pub(crate) merge: bool,
    wildcard: bool,
}

impl FetchRefspec {
    pub(crate) fn parse_cli(value: &str, remote: &str) -> Result<Self, String> {
        Self::parse(value, remote, true)
    }

    pub(crate) fn parse_config(value: &str, remote: &str) -> Result<Self, String> {
        Self::parse(value, remote, false)
    }

    pub(crate) fn default_heads(remote: &str) -> Self {
        Self {
            source: "refs/heads/*".to_string(),
            destination: format!("refs/remotes/{remote}/*"),
            force: true,
            merge: false,
            wildcard: true,
        }
    }

    pub(crate) fn default_merge_requests(remote: &str) -> Self {
        Self {
            source: "refs/mr/*".to_string(),
            destination: format!("refs/remotes/{remote}/mr/*"),
            force: true,
            merge: false,
            wildcard: true,
        }
    }

    fn parse(value: &str, remote: &str, cli: bool) -> Result<Self, String> {
        let value = value.trim();
        if value.is_empty() {
            return Err("fetch refspec must not be empty".to_string());
        }
        if value.starts_with('^') {
            return Err(format!(
                "negative fetch refspec '{value}' is not supported yet"
            ));
        }
        let (force, body) = value
            .strip_prefix('+')
            .map_or((false, value), |body| (true, body));
        let mut parts = body.split(':');
        let source = parts.next().unwrap_or_default();
        let destination = parts.next();
        if parts.next().is_some() {
            return Err(format!(
                "invalid fetch refspec '{value}': too many ':' separators"
            ));
        }
        if source.is_empty() {
            return Err(format!("invalid fetch refspec '{value}': source is empty"));
        }

        let source = normalize_source(source);
        let destination = match destination {
            Some("") => {
                return Err(format!(
                    "invalid fetch refspec '{value}': destination is empty"
                ));
            }
            Some(destination) => normalize_destination(destination),
            None => default_destination(&source, remote)?,
        };

        let source_stars = source.matches('*').count();
        let destination_stars = destination.matches('*').count();
        if source_stars != destination_stars || source_stars > 1 {
            return Err(format!(
                "invalid fetch refspec '{value}': source and destination must contain the same single wildcard"
            ));
        }
        validate_destination(&destination, remote)?;

        Ok(Self {
            source,
            destination,
            force,
            merge: cli && source_stars == 0,
            wildcard: source_stars == 1,
        })
    }

    pub(crate) fn map_source(&self, source_ref: &str) -> Option<String> {
        if !self.wildcard {
            return (source_ref == self.source).then(|| self.destination.clone());
        }
        let (source_prefix, source_suffix) = self.source.split_once('*')?;
        let middle = source_ref
            .strip_prefix(source_prefix)?
            .strip_suffix(source_suffix)?;
        let (destination_prefix, destination_suffix) = self.destination.split_once('*')?;
        Some(format!("{destination_prefix}{middle}{destination_suffix}"))
    }

    pub(crate) fn source(&self) -> &str {
        &self.source
    }
}

fn normalize_source(source: &str) -> String {
    if source.starts_with("refs/") {
        source.to_string()
    } else {
        format!("refs/heads/{source}")
    }
}

fn normalize_destination(destination: &str) -> String {
    if destination.starts_with("refs/") {
        destination.to_string()
    } else {
        format!("refs/heads/{destination}")
    }
}

fn default_destination(source: &str, remote: &str) -> Result<String, String> {
    if let Some(branch) = source.strip_prefix("refs/heads/") {
        return Ok(format!("refs/remotes/{remote}/{branch}"));
    }
    if let Some(mr) = source.strip_prefix("refs/mr/") {
        return Ok(format!("refs/remotes/{remote}/mr/{mr}"));
    }
    Err(format!(
        "fetch refspec source '{source}' requires an explicit destination"
    ))
}

fn validate_destination(destination: &str, remote: &str) -> Result<(), String> {
    if let Some(branch) = destination.strip_prefix("refs/heads/") {
        if branch.is_empty() || branch == "HEAD" {
            return Err(format!("invalid fetch destination '{destination}'"));
        }
        return Ok(());
    }
    if let Some(rest) = destination.strip_prefix("refs/remotes/") {
        let Some((destination_remote, branch)) = rest.split_once('/') else {
            return Err(format!("invalid fetch destination '{destination}'"));
        };
        if destination_remote.is_empty() || branch.is_empty() || branch == "HEAD" {
            return Err(format!("invalid fetch destination '{destination}'"));
        }
        if destination_remote != remote {
            return Err(format!(
                "fetch destination '{destination}' is outside remote '{remote}'"
            ));
        }
        return Ok(());
    }
    Err(format!(
        "unsupported fetch destination '{destination}'; use refs/heads/* or refs/remotes/{remote}/*"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_and_wildcard_specs_map_sources() {
        let exact =
            FetchRefspec::parse_cli("+refs/heads/topic:refs/remotes/origin/review", "origin")
                .expect("parse exact refspec");
        assert_eq!(
            exact.map_source("refs/heads/topic").as_deref(),
            Some("refs/remotes/origin/review")
        );
        assert!(exact.force);
        assert!(exact.merge);

        let wildcard = FetchRefspec::parse_config("+refs/heads/*:refs/remotes/origin/*", "origin")
            .expect("parse wildcard refspec");
        assert_eq!(
            wildcard.map_source("refs/heads/feature/x").as_deref(),
            Some("refs/remotes/origin/feature/x")
        );
        assert!(!wildcard.merge);
    }

    #[test]
    fn invalid_or_cross_remote_destinations_are_rejected() {
        assert!(FetchRefspec::parse_config("refs/heads/*:refs/heads/main", "origin").is_err());
        assert!(
            FetchRefspec::parse_cli("refs/heads/main:refs/remotes/upstream/main", "origin")
                .is_err()
        );
    }
}
