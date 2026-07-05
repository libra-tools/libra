//! `libra auth` — host-scoped HTTP token auth v1 (lore.md §1.6, a Libra
//! extension). Token-only: write / read / expiry detection / revoke close
//! the lifecycle in one surface. There is deliberately NO `--token <value>`
//! flag — argv lands in shell history and /proc; the token arrives via a
//! hidden prompt (TTY) or `--with-token` stdin (scripts). The plaintext
//! token never appears in output, logs, errors, or JSON.

use clap::{Parser, Subcommand};
use serde::Serialize;

use crate::{
    internal::auth::{self, HostScope},
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
    },
};

pub const AUTH_EXAMPLES: &str = "\
EXAMPLES:
    libra auth login --host git.example.com          Prompt for a token (hidden)
    printf '%s' \"$TOKEN\" | libra auth login --host git.example.com --with-token
    libra auth login --host github.com --username x-access-token --expires-in 90d
    printf '%s\\n' \"$GITHUB_PAT\" | libra auth login --host github.com --username x-access-token --with-token
    libra config --global auth.backend keyring && libra auth migrate --to keyring
    libra auth login --host git.example.com:8443 --expires-in 30d
    libra auth status                                All stored tokens (never the secrets)
    libra auth status --host git.example.com         Scriptable: exit 0 iff valid
    libra auth logout --host git.example.com         Revoke one host
    libra auth clear                                 Revoke everything

NOTES:
    GitHub HTTPS PATs are stored as the Basic-auth password for github.com.
    There is no --token flag by design (shell history/proc leak). With the
    default file backend, tokens are encrypted at rest with the 0600 global
    vault key and `config get/list` cannot dump them. The keyring backend stores
    secrets in the OS keychain. Stored tokens attach only to matching https
    hosts (http only for loopback, with the explicit port).";

/// Manage host-scoped HTTP tokens: login, status, logout (Libra extension).
#[derive(Parser, Debug)]
#[command(after_help = AUTH_EXAMPLES)]
pub struct AuthArgs {
    #[command(subcommand)]
    pub command: AuthCommand,
}

#[derive(Subcommand, Debug)]
pub enum AuthCommand {
    /// Store a token for a host (prompt or --with-token stdin; never a flag).
    Login {
        /// Host to authenticate to: `host`, `host:port`, or `https://host`.
        #[arg(long, value_name = "HOST")]
        host: String,
        /// Username sent alongside the token (Basic auth). Default suits
        /// PAT-style servers.
        #[arg(long, default_value = "x-access-token")]
        username: String,
        /// Read the token from the first line of stdin (for scripts/pipes).
        /// Required when stdin is not a TTY.
        #[arg(long)]
        with_token: bool,
        /// Absolute expiry (RFC3339, e.g. 2027-01-01T00:00:00Z).
        #[arg(long, value_name = "WHEN", conflicts_with = "expires_in")]
        expires_at: Option<String>,
        /// Relative expiry: <N>d / <N>h / <N>m / <N>s (single unit).
        #[arg(long, value_name = "DUR")]
        expires_in: Option<String>,
    },
    /// Report stored tokens (never the secrets). With --host: exit 0 iff a
    /// valid (unexpired) token exists.
    Status {
        #[arg(long, value_name = "HOST")]
        host: Option<String>,
    },
    /// Remove a host's token (or every token with --all). Idempotent.
    Logout {
        #[arg(long, value_name = "HOST", conflicts_with = "all")]
        host: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// Remove every stored token (Lore's `auth clear`).
    Clear,
    /// Move stored tokens between backends (file <-> OS keyring) and switch
    /// `auth.backend` (lore.md 2.7). Idempotent: re-running converges.
    Migrate {
        /// Target backend.
        #[arg(long, value_parser = ["file", "keyring"])]
        to: String,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "action", rename_all = "kebab-case")]
enum AuthOutput {
    Login {
        host: String,
        username: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        expires_at: Option<u64>,
    },
    Status {
        tokens: Vec<auth::TokenStatus>,
    },
    Logout {
        removed: usize,
    },
}

fn parse_expires_in(text: &str) -> Result<u64, String> {
    let text = text.trim();
    if text.len() < 2 {
        return Err("expected <N>d/<N>h/<N>m/<N>s".to_string());
    }
    let (number, unit) = text.split_at(text.len() - 1);
    let value: u64 = number.parse().map_err(|_| {
        format!("'{number}' is not a number (combined forms like 1h30m are not supported)")
    })?;
    let seconds_per_unit: u64 = match unit {
        "d" => 86_400,
        "h" => 3_600,
        "m" => 60,
        "s" => 1,
        other => return Err(format!("unknown unit '{other}' (expected d/h/m/s)")),
    };
    value
        .checked_mul(seconds_per_unit)
        .ok_or_else(|| "duration overflows".to_string())
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn parse_expiry(
    expires_at: Option<&str>,
    expires_in: Option<&str>,
) -> Result<Option<u64>, CliError> {
    if let Some(text) = expires_at {
        let when = chrono::DateTime::parse_from_rfc3339(text.trim()).map_err(|error| {
            CliError::command_usage(format!(
                "--expires-at must be RFC3339 (e.g. 2027-01-01T00:00:00Z): {error}"
            ))
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_hint("a bare date like 20270101 is not accepted — spell out the timestamp")
        })?;
        let unix = when.timestamp();
        if unix <= now_unix() as i64 {
            return Err(
                CliError::command_usage("--expires-at is already in the past")
                    .with_stable_code(StableErrorCode::CliInvalidArguments),
            );
        }
        return Ok(Some(unix as u64));
    }
    if let Some(text) = expires_in {
        let seconds = parse_expires_in(text).map_err(|detail| {
            CliError::command_usage(format!("invalid --expires-in: {detail}"))
                .with_stable_code(StableErrorCode::CliInvalidArguments)
        })?;
        let expires = now_unix().checked_add(seconds).ok_or_else(|| {
            CliError::command_usage("--expires-in overflows")
                .with_stable_code(StableErrorCode::CliInvalidArguments)
        })?;
        return Ok(Some(expires));
    }
    Ok(None)
}

/// Read the token: hidden prompt on a TTY, `--with-token` stdin otherwise.
fn read_token(with_token: bool) -> CliResult<String> {
    use std::io::IsTerminal;
    let token = if with_token {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).map_err(|error| {
            CliError::fatal(format!("failed to read the token from stdin: {error}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
        line.trim().to_string()
    } else if std::io::stdin().is_terminal() {
        if std::env::var_os("LIBRA_NO_HIDE_PASSWORD").is_some() {
            let mut line = String::new();
            eprint!("Token: ");
            std::io::stdin().read_line(&mut line).map_err(|error| {
                CliError::fatal(format!("failed to read the token: {error}"))
                    .with_stable_code(StableErrorCode::IoReadFailed)
            })?;
            line.trim().to_string()
        } else {
            rpassword::prompt_password("Token (input hidden): ").map_err(|error| {
                CliError::fatal(format!("failed to read the token: {error}"))
                    .with_stable_code(StableErrorCode::IoReadFailed)
            })?
        }
    } else {
        return Err(CliError::command_usage(
            "stdin is not a TTY; pass --with-token and pipe the token in",
        )
        .with_stable_code(StableErrorCode::CliInvalidArguments)
        .with_hint("printf '%s' \"$TOKEN\" | libra auth login --host <host> --with-token"));
    };
    if token.is_empty() {
        return Err(CliError::command_usage("token must not be empty")
            .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    if token.len() > 8192 || token.chars().any(|c| c.is_control()) {
        return Err(
            CliError::command_usage("token is too long or contains control characters")
                .with_stable_code(StableErrorCode::CliInvalidArguments),
        );
    }
    Ok(token)
}

fn map_store(error: anyhow::Error) -> CliError {
    // Owner-API errors never contain the secret.
    CliError::fatal(format!("auth storage error: {error}"))
        .with_stable_code(StableErrorCode::IoWriteFailed)
}

pub async fn execute(args: AuthArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
    }
}

pub async fn execute_safe(args: AuthArgs, output: &OutputConfig) -> CliResult<()> {
    match args.command {
        AuthCommand::Login {
            host,
            username,
            with_token,
            expires_at,
            expires_in,
        } => {
            let scope = HostScope::parse(&host).map_err(|error| {
                CliError::command_usage(format!("invalid --host: {error}"))
                    .with_stable_code(StableErrorCode::CliInvalidArguments)
            })?;
            if username.contains(':') || username.is_empty() {
                return Err(CliError::command_usage(
                    "username must be non-empty and must not contain ':'",
                )
                .with_stable_code(StableErrorCode::CliInvalidArguments));
            }
            let expiry = parse_expiry(expires_at.as_deref(), expires_in.as_deref())?;
            let token = read_token(with_token)?;
            auth::store_token(&scope, &username, &token, expiry)
                .await
                .map_err(map_store)?;
            let report = AuthOutput::Login {
                host: scope.display(),
                username,
                expires_at: expiry,
            };
            if output.is_json() {
                return emit_json_data("auth", &report, output);
            }
            if !output.quiet {
                println!("Stored token for {}", scope.display());
            }
            Ok(())
        }
        AuthCommand::Status { host } => {
            let tokens = auth::list().await.map_err(map_store)?;
            if let Some(host) = host {
                let scope = HostScope::parse(&host).map_err(|error| {
                    CliError::command_usage(format!("invalid --host: {error}"))
                        .with_stable_code(StableErrorCode::CliInvalidArguments)
                })?;
                let hit = tokens
                    .iter()
                    .find(|row| row.host == scope.host && row.port == scope.port);
                let report = AuthOutput::Status {
                    tokens: hit.cloned().into_iter().collect(),
                };
                if output.is_json() {
                    emit_json_data("auth", &report, output)?;
                } else if !output.quiet {
                    match hit {
                        Some(row) => {
                            println!("{}: {} ({})", row.host_display(), row.state, row.username)
                        }
                        None => println!("no token stored for {}", scope.display()),
                    }
                }
                // Scripting contract: 0 iff present AND valid.
                if !matches!(hit, Some(row) if row.state == "valid") {
                    return Err(CliError::silent_exit(1));
                }
                return Ok(());
            }
            let report = AuthOutput::Status {
                tokens: tokens.clone(),
            };
            if output.is_json() {
                return emit_json_data("auth", &report, output);
            }
            if !output.quiet {
                if tokens.is_empty() {
                    println!("no tokens stored");
                }
                for row in &tokens {
                    let expiry = match row.expires_at {
                        Some(at) => format!(
                            " (expires {})",
                            chrono::DateTime::<chrono::Utc>::from_timestamp(at as i64, 0)
                                .map(|when| when.to_rfc3339())
                                .unwrap_or_else(|| at.to_string())
                        ),
                        None => " (no expiry recorded)".to_string(),
                    };
                    println!(
                        "{}: {} ({}){expiry}",
                        row.host_display(),
                        row.state,
                        row.username
                    );
                }
            }
            Ok(())
        }
        AuthCommand::Logout { host, all } => {
            let removed = if all {
                auth::remove_all().await.map_err(map_store)?
            } else if let Some(host) = host {
                let scope = HostScope::parse(&host).map_err(|error| {
                    CliError::command_usage(format!("invalid --host: {error}"))
                        .with_stable_code(StableErrorCode::CliInvalidArguments)
                })?;
                usize::from(auth::remove(&scope).await.map_err(map_store)?)
            } else {
                return Err(CliError::command_usage("pass --host <host> or --all")
                    .with_stable_code(StableErrorCode::CliInvalidArguments));
            };
            let report = AuthOutput::Logout { removed };
            if output.is_json() {
                return emit_json_data("auth", &report, output);
            }
            if !output.quiet {
                if removed == 0 {
                    println!("nothing to remove");
                } else {
                    println!("removed {removed} token(s)");
                }
            }
            Ok(())
        }
        AuthCommand::Migrate { to } => {
            let target = match to.as_str() {
                "keyring" => auth::BackendKind::Keyring,
                _ => auth::BackendKind::File,
            };
            let moved = auth::migrate_tokens(target).await.map_err(map_store)?;
            if output.is_json() {
                return emit_json_data(
                    "auth",
                    &serde_json::json!({ "action": "migrate", "to": to, "moved": moved }),
                    output,
                );
            }
            if !output.quiet {
                println!("moved {moved} token(s) to the {to} backend");
            }
            Ok(())
        }
        AuthCommand::Clear => {
            let removed = auth::remove_all().await.map_err(map_store)?;
            let report = AuthOutput::Logout { removed };
            if output.is_json() {
                return emit_json_data("auth", &report, output);
            }
            if !output.quiet {
                println!("removed {removed} token(s)");
            }
            Ok(())
        }
    }
}
