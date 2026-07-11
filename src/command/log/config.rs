use super::LogArgs;
use crate::{
    internal::config::{
        LocalIdentityTarget, parse_git_config_bool, read_cascaded_config_value_strict,
    },
    utils::error::{CliError, CliResult, StableErrorCode},
};

pub(super) struct ResolvedLogConfig {
    pub(super) pretty: Option<String>,
    pub(super) date: Option<String>,
    pub(super) follow: bool,
}

pub(super) async fn resolve_log_config(
    args: &LogArgs,
    human_output: bool,
) -> CliResult<ResolvedLogConfig> {
    let pretty = if human_output
        && !args.only_trailers
        && !args.oneline
        && args.pretty.is_none()
        && args.format.is_none()
    {
        configured_pretty().await?
    } else {
        None
    };
    let cli_date = args.date.as_deref().map(resolve_cli_date).transpose()?;
    let date = if human_output && !args.only_trailers {
        match cli_date {
            Some(date) => Some(date),
            None => configured_date().await?,
        }
    } else {
        None
    };
    let follow = if args.follow.is_some() || args.no_follow {
        false
    } else {
        configured_follow().await?
    };

    Ok(ResolvedLogConfig {
        pretty,
        date,
        follow,
    })
}

async fn read_log_config(key: &'static str) -> CliResult<Option<String>> {
    read_cascaded_config_value_strict(LocalIdentityTarget::CurrentRepo, key)
        .await
        .map_err(|error| {
            CliError::fatal(format!("failed to read config '{key}': {error:#}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
                .with_hint(format!("repair or unset '{key}', then retry 'libra log'"))
        })
}

fn invalid_config(key: &'static str, value: &str, hint: &str) -> CliError {
    CliError::command_usage(format!("invalid value for config '{key}': '{value}'"))
        .with_stable_code(StableErrorCode::CliInvalidArguments)
        .with_hint(hint)
}

pub(crate) async fn configured_pretty() -> CliResult<Option<String>> {
    let Some(value) = read_log_config("format.pretty").await? else {
        return Ok(None);
    };
    if value.trim().is_empty() {
        return Err(invalid_config(
            "format.pretty",
            &value,
            "use a supported preset or a non-empty pretty-format template",
        ));
    }
    let trimmed = value.trim();
    let supported_preset = matches!(
        trimmed,
        "oneline" | "medium" | "short" | "full" | "fuller" | "reference" | "raw"
    );
    let custom_template =
        trimmed.starts_with("format:") || trimmed.starts_with("tformat:") || trimmed.contains('%');
    if !supported_preset && !custom_template {
        return Err(invalid_config(
            "format.pretty",
            &value,
            "supported presets: oneline, medium, short, full, fuller, reference, raw; custom templates must use format:, tformat:, or a % placeholder",
        ));
    }
    Ok(Some(trimmed.to_string()))
}

pub(crate) async fn configured_date() -> CliResult<Option<String>> {
    let Some(value) = read_log_config("log.date").await? else {
        return Ok(None);
    };
    let Some(normalized) = supported_date_mode(&value) else {
        return Err(invalid_config(
            "log.date",
            &value,
            "supported modes: default, short, iso, iso-strict, rfc, unix, raw",
        ));
    };
    Ok(Some(normalized))
}

fn supported_date_mode(value: &str) -> Option<String> {
    let normalized = value.trim().to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "default"
            | "short"
            | "iso"
            | "iso8601"
            | "iso-strict"
            | "iso8601-strict"
            | "rfc"
            | "rfc2822"
            | "unix"
            | "raw"
    )
    .then_some(normalized)
}

pub(crate) fn resolve_cli_date(value: &str) -> CliResult<String> {
    supported_date_mode(value).ok_or_else(|| {
        CliError::command_usage(format!("invalid --date option: '{value}'"))
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_hint("supported modes: default, short, iso, iso-strict, rfc, unix, raw")
    })
}

async fn configured_follow() -> CliResult<bool> {
    let Some(value) = read_log_config("log.follow").await? else {
        return Ok(false);
    };
    parse_git_config_bool(&value).ok_or_else(|| {
        invalid_config(
            "log.follow",
            &value,
            "use a Git boolean such as true/false, yes/no, on/off, or an integer",
        )
    })
}
