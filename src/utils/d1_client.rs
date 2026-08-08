//! Cloudflare D1 REST API client for database backup and synchronization.
//!
//! This module provides a client for interacting with Cloudflare D1 database
//! via the REST API. It supports executing SQL statements, querying data,
//! and batch operations for efficient cloud backup.
//!
//! Authentication uses a Cloudflare account ID, API token, and D1 database ID,
//! resolved through [`resolve_env`](crate::internal::config::resolve_env) so they
//! can come from local repo `vault.env.*` config, global config, or process env.
//! The default client speaks HTTPS to `api.cloudflare.com`; the constructor enforces
//! `https_only(true)`. Tests can inject a different API base URL through
//! [`D1Client::new_with_api_base_url`].
//!
//! Schema management is conservative: [`D1Client::ensure_object_index_table`]
//! migrates an older single-column unique index into a composite `(repo_id, o_id)`
//! unique index when it detects the legacy shape, so users upgrading from older
//! Libra versions do not need to drop their D1 backup database manually.

use std::{collections::HashSet, time::Duration};

use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};

use crate::{
    internal::config::resolve_env,
    utils::backoff::{RetryOutcome, RetryPolicy, parse_retry_after, retry_idempotent},
};

const DEFAULT_D1_API_BASE_URL: &str = "https://api.cloudflare.com/client/v4";
const AGENT_CAPTURE_PAGE_SIZE: usize = 256;
const AGENT_CAPTURE_MAX_ROWS_PER_TABLE: usize = 100_000;
// A cloud publication is itself bounded to 120 seconds. Giving an active
// writer more than twice that window avoids stealing a slow-but-live write,
// while still making a crashed `publishing` generation recoverable without
// manual D1 surgery.
const AGENT_CAPTURE_GENERATION_LEASE_SECONDS: i64 = 300;
const OBJECT_INDEX_SNAPSHOT_MAX_RETRIES: usize = 3;
const AGENT_CAPTURE_BEGIN_GENERATION_FROM_SQL: &str = r#"
    UPDATE agent_capture_generation SET
        generation = generation + 1,
        state = 'publishing',
        writer_token = ?2,
        object_index_digest = ?3,
        object_index_count = ?4,
        object_index_scope = ?5,
        object_index_generation = ?6,
        traces_head = ?7,
        started_at = CAST(strftime('%s', 'now') AS INTEGER),
        completed_at = NULL
    WHERE repo_id = ?1 AND generation = ?8
      AND (
        state = 'complete'
        OR (
            state = 'publishing'
            AND started_at <= CAST(strftime('%s', 'now') AS INTEGER) - ?9
        )
      )
    RETURNING repo_id, generation, state, writer_token, object_index_digest,
              object_index_count, object_index_scope, object_index_generation,
              traces_head, started_at, completed_at
"#;
const AGENT_CAPTURE_COMPLETE_GENERATION_SQL: &str = r#"
    UPDATE agent_capture_generation
       SET state = 'complete', writer_token = NULL,
           completed_at = CAST(strftime('%s', 'now') AS INTEGER)
     WHERE repo_id = ?1 AND state = 'publishing' AND writer_token = ?2
       AND object_index_generation = ?3
       AND COALESCE((
           SELECT generation FROM object_index_catalog_generation g
           WHERE g.repo_id = ?1
       ), 0) = ?3
    RETURNING repo_id, generation, state, writer_token, object_index_digest,
              object_index_count, object_index_scope, object_index_generation,
              traces_head, started_at, completed_at
"#;
const OBJECT_INDEX_CATALOG_SEED_SQL: &str =
    "INSERT INTO object_index_catalog_generation (repo_id, generation)
     SELECT DISTINCT repo_id, 0 FROM object_index
     WHERE 1
     ON CONFLICT(repo_id) DO NOTHING";
const OBJECT_INDEX_CATALOG_READY_TABLE: &str = "object_index_catalog_generation_ready";
const OBJECT_INDEX_CATALOG_READY_TABLE_SQL: &str =
    "CREATE TABLE IF NOT EXISTS object_index_catalog_generation_ready (
         singleton INTEGER PRIMARY KEY CHECK(singleton = 1)
     )";
const OBJECT_INDEX_CATALOG_INVALIDATE_SQL: &str =
    "DELETE FROM object_index_catalog_generation_ready WHERE singleton = 1";
const OBJECT_INDEX_CATALOG_PUBLISH_READY_SQL: &str =
    "INSERT INTO object_index_catalog_generation_ready (singleton) VALUES (1)
     ON CONFLICT(singleton) DO NOTHING";
const AGENT_SUBAGENT_CLAIM_UPDATE_GUARD: &str =
    "excluded.revision_cursor >= agent_subagent_content_claim.revision_cursor
     AND (excluded.sync_revision > agent_subagent_content_claim.sync_revision
       OR (excluded.sync_revision = agent_subagent_content_claim.sync_revision
           AND excluded.revision_cursor IS agent_subagent_content_claim.revision_cursor
           AND excluded.current_revision IS agent_subagent_content_claim.current_revision
           AND excluded.current_checkpoint_id IS agent_subagent_content_claim.current_checkpoint_id
           AND excluded.current_digest IS agent_subagent_content_claim.current_digest))";
const OBJECT_INDEX_CATALOG_TRIGGERS: [&str; 3] = [
    "CREATE TRIGGER IF NOT EXISTS object_index_catalog_insert
     AFTER INSERT ON object_index BEGIN
       INSERT INTO object_index_catalog_generation (repo_id, generation)
       VALUES (NEW.repo_id, 1)
       ON CONFLICT(repo_id) DO UPDATE SET generation = generation + 1;
     END",
    "CREATE TRIGGER IF NOT EXISTS object_index_catalog_update
     AFTER UPDATE ON object_index BEGIN
       INSERT INTO object_index_catalog_generation (repo_id, generation)
       VALUES (NEW.repo_id, 1)
       ON CONFLICT(repo_id) DO UPDATE SET generation = generation + 1;
       INSERT INTO object_index_catalog_generation (repo_id, generation)
       SELECT OLD.repo_id, 1 WHERE OLD.repo_id <> NEW.repo_id
       ON CONFLICT(repo_id) DO UPDATE SET generation = generation + 1;
     END",
    "CREATE TRIGGER IF NOT EXISTS object_index_catalog_delete
     AFTER DELETE ON object_index BEGIN
       INSERT INTO object_index_catalog_generation (repo_id, generation)
       VALUES (OLD.repo_id, 1)
       ON CONFLICT(repo_id) DO UPDATE SET generation = generation + 1;
     END",
];
const D1_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const D1_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

fn charge_agent_capture_restore_rows(
    remaining_rows: &mut usize,
    row_count: usize,
    label: &str,
) -> Result<(), D1Error> {
    if row_count > *remaining_rows {
        return Err(D1Error {
            code: 2011,
            message: format!(
                "remote agent-capture restore exceeds its aggregate row safety bound while reading {label}"
            ),
        });
    }
    *remaining_rows -= row_count;
    Ok(())
}
const AGENT_CAPTURE_LEGACY_WRITE_BARRIERS: [&str; 6] = [
    "CREATE TRIGGER IF NOT EXISTS agent_session_v2_insert_barrier
     BEFORE INSERT ON agent_session BEGIN
       SELECT RAISE(ABORT, 'legacy agent capture writer is fenced; upgrade Libra');
     END",
    "CREATE TRIGGER IF NOT EXISTS agent_session_v2_update_barrier
     BEFORE UPDATE ON agent_session BEGIN
       SELECT RAISE(ABORT, 'legacy agent capture writer is fenced; upgrade Libra');
     END",
    "CREATE TRIGGER IF NOT EXISTS agent_session_v2_delete_barrier
     BEFORE DELETE ON agent_session BEGIN
       SELECT RAISE(ABORT, 'legacy agent capture writer is fenced; upgrade Libra');
     END",
    "CREATE TRIGGER IF NOT EXISTS agent_checkpoint_v2_insert_barrier
     BEFORE INSERT ON agent_checkpoint BEGIN
       SELECT RAISE(ABORT, 'legacy agent capture writer is fenced; upgrade Libra');
     END",
    "CREATE TRIGGER IF NOT EXISTS agent_checkpoint_v2_update_barrier
     BEFORE UPDATE ON agent_checkpoint BEGIN
       SELECT RAISE(ABORT, 'legacy agent capture writer is fenced; upgrade Libra');
     END",
    "CREATE TRIGGER IF NOT EXISTS agent_checkpoint_v2_delete_barrier
     BEFORE DELETE ON agent_checkpoint BEGIN
       SELECT RAISE(ABORT, 'legacy agent capture writer is fenced; upgrade Libra');
     END",
];
const AGENT_CAPTURE_LEGACY_SESSION_ADOPTION_SQL: &str = r#"
    INSERT INTO agent_capture_session_v2 (
        session_id, repo_id, agent_kind, provider_session_id, state, working_dir,
        worktree_id, parent_commit, parent_session_id, metadata_json,
        redaction_report, started_at, last_event_at, stopped_at, schema_version,
        sync_revision, synced_at
    )
    SELECT session_id, repo_id, agent_kind, provider_session_id, state, working_dir,
           worktree_id, parent_commit, parent_session_id, metadata_json,
           redaction_report, started_at, last_event_at, stopped_at, schema_version,
           0, synced_at
    FROM agent_session
    WHERE NOT EXISTS (
        SELECT 1 FROM agent_capture_schema_migration
        WHERE version = 2 AND state = 'complete'
    )
    ON CONFLICT(repo_id, session_id) DO NOTHING
"#;
const AGENT_CAPTURE_LEGACY_CHECKPOINT_ADOPTION_SQL: &str = r#"
    INSERT INTO agent_capture_checkpoint_v2 (
        checkpoint_id, repo_id, session_id, parent_checkpoint_id, scope,
        parent_commit, tree_oid, metadata_blob_oid, traces_commit, tool_use_id,
        subagent_session_id, description, created_at, sync_revision, synced_at
    )
    SELECT checkpoint_id, repo_id, session_id, parent_checkpoint_id, scope,
           parent_commit, tree_oid, metadata_blob_oid, traces_commit, tool_use_id,
           subagent_session_id, description, created_at, 0, synced_at
    FROM agent_checkpoint
    WHERE NOT EXISTS (
        SELECT 1 FROM agent_capture_schema_migration
        WHERE version = 2 AND state = 'complete'
    )
    ON CONFLICT(repo_id, checkpoint_id) DO NOTHING
"#;
const AGENT_CAPTURE_LEGACY_ORPHAN_CLEANUP_SQL: &str = r#"
    DELETE FROM agent_capture_checkpoint_v2
    WHERE NOT EXISTS (
      SELECT 1 FROM agent_capture_session_v2 s
      WHERE s.repo_id = agent_capture_checkpoint_v2.repo_id
        AND s.session_id = agent_capture_checkpoint_v2.session_id
    )
"#;

fn reject_unfenced_agent_capture_write(label: &str) -> Result<(), D1Error> {
    Err(D1Error {
        code: 3003,
        message: format!(
            "unfenced single-row {label} writes are disabled; publish through an active agent-capture generation"
        ),
    })
}

fn agent_session_compatibility_table(v2_ready: bool) -> &'static str {
    if v2_ready {
        "agent_capture_session_v2"
    } else {
        "agent_session"
    }
}

fn agent_checkpoint_compatibility_table(v2_ready: bool) -> &'static str {
    if v2_ready {
        "agent_capture_checkpoint_v2"
    } else {
        "agent_checkpoint"
    }
}

/// Top-level wrapper for every Cloudflare D1 API response.
///
/// Cloudflare wraps the actual query results in a `Vec<D1QueryResult>`; even single
/// queries are returned as a one-element vector. When `success == false`, the
/// `errors` vector carries the failure details.
#[derive(Debug, Deserialize)]
pub struct D1Response<T> {
    pub success: bool,
    pub errors: Vec<D1Error>,
    pub messages: Vec<D1Message>,
    pub result: Option<Vec<D1QueryResult<T>>>,
}

/// Error structure used both for Cloudflare's API errors *and* for client-side
/// failures (HTTP, JSON parsing, env resolution). Client-side codes use the 1xxx
/// and 2xxx ranges; Cloudflare's API codes occupy the 3xxx+ range.
#[derive(Debug, Deserialize)]
pub struct D1Error {
    /// Numeric error code. Stable enough to match against in tests.
    pub code: i32,
    /// Human-readable failure message.
    pub message: String,
}

/// Build a redacted, human-readable message from a reqwest transport error.
///
/// Deliberately avoids the reqwest `Debug` output (which is verbose and can
/// embed request internals) and never includes the full URL — only the failure
/// class and the host, which is safe to log.
fn redact_request_error(err: &reqwest::Error) -> String {
    let host = err
        .url()
        .and_then(|url| url.host_str())
        .unwrap_or("<unknown host>");
    if err.is_timeout() {
        format!("HTTP request to D1 host {host} timed out")
    } else if err.is_connect() {
        format!("failed to connect to D1 host {host}")
    } else {
        format!("HTTP request to D1 host {host} failed")
    }
}

/// Informational message returned by Cloudflare alongside `result` (e.g. retry hints).
#[derive(Debug, Deserialize)]
pub struct D1Message {
    pub code: Option<i32>,
    pub message: String,
}

/// One element of the `result` array returned by D1.
#[derive(Debug, Deserialize)]
pub struct D1QueryResult<T> {
    /// Row values for SELECT statements; `None` for non-SELECT statements.
    pub results: Option<Vec<T>>,
    /// `true` when the individual statement succeeded (an outer `D1Response`
    /// can succeed overall while one statement fails inside).
    pub success: bool,
    pub meta: Option<D1Meta>,
}

/// Per-statement execution metadata returned by D1.
#[derive(Debug, Deserialize)]
pub struct D1Meta {
    pub changes: Option<i64>,
    pub duration: Option<f64>,
    pub last_row_id: Option<i64>,
    pub rows_read: Option<i64>,
    pub rows_written: Option<i64>,
}

/// One SQL statement plus its bound parameters, ready to be sent over the wire.
///
/// Parameters are positional (`?1`, `?2`, ...). `params` is omitted from the JSON
/// body entirely when `None` so the request matches the single-statement shape that
/// the D1 `/query` endpoint accepts.
#[derive(Debug, Serialize)]
pub struct D1Statement {
    pub sql: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
struct D1TableColumn {
    name: String,
}

#[derive(Debug, Deserialize)]
struct D1ExistenceProbe {
    present: i64,
}

#[derive(Debug, Deserialize)]
struct D1CatalogGeneration {
    generation: i64,
}

/// CAS update input for `publish_sites.latest_revision_oid`.
pub struct PublishSiteLatestUpdate<'a> {
    pub site_id: &'a str,
    pub default_ref: Option<&'a str>,
    pub latest_revision_oid: Option<&'a str>,
    pub next_refs_generation: i64,
    pub expected_refs_generation: i64,
    pub updated_at: &'a str,
    pub force: bool,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum PublishSiteLatestUpdateResult {
    Updated,
    Conflict,
}

/// Cloudflare D1 REST API client.
///
/// `Clone` is cheap (HTTP client is `Arc` internally). Construct via
/// [`D1Client::from_env`] in production code so vault-stored credentials are
/// honoured, or [`D1Client::new`] when credentials are already in scope.
#[derive(Clone)]
pub struct D1Client {
    client: Client,
    account_id: String,
    api_token: String,
    database_id: String,
    api_base_url: Url,
}

impl D1Client {
    /// Create a new D1 client from environment variables, using
    /// [`resolve_env`](crate::internal::config::resolve_env) so that
    /// vault-stored secrets are picked up automatically.
    ///
    /// Resolution order per variable:
    /// 1. Local vault config (`vault.env.<VAR>`)
    /// 2. Global vault config (`~/.libra/config.db`)
    /// 3. System environment variable (`std::env::var`)
    ///
    /// Required variables:
    /// - `LIBRA_D1_ACCOUNT_ID`: Cloudflare Account ID
    /// - `LIBRA_D1_API_TOKEN`: Cloudflare API Token
    /// - `LIBRA_D1_DATABASE_ID`: D1 Database ID
    ///
    /// Boundary conditions:
    /// - Returns a `D1Error` with codes `1001`/`1002`/`1003` when the corresponding
    ///   variable is unset or empty across all scopes.
    /// - Returns a `D1Error` with codes `1101`/`1102`/`1103` when the underlying
    ///   resolver fails (e.g. corrupt config database). Tests rely on these codes
    ///   to differentiate "missing" from "broken".
    /// - See: `d1_client_from_env_reads_values_from_local_config`,
    ///   `d1_client_from_env_surfaces_global_config_connection_errors`.
    pub async fn from_env() -> Result<Self, D1Error> {
        let account_id = Self::resolve_required_env("LIBRA_D1_ACCOUNT_ID", 1001, 1101).await?;
        let api_token = Self::resolve_required_env("LIBRA_D1_API_TOKEN", 1002, 1102).await?;
        let database_id = Self::resolve_required_env("LIBRA_D1_DATABASE_ID", 1003, 1103).await?;

        Ok(Self::new(account_id, api_token, database_id))
    }

    /// Resolve a required env var, mapping the two failure modes into distinct codes.
    ///
    /// Boundary conditions:
    /// - `Ok(Some(""))` is treated as missing — empty strings are not credentials.
    /// - The two error codes (`missing_code`, `resolution_error_code`) let callers
    ///   distinguish "user forgot to configure" from "configuration store is broken".
    async fn resolve_required_env(
        name: &str,
        missing_code: i32,
        resolution_error_code: i32,
    ) -> Result<String, D1Error> {
        match resolve_env(name).await {
            Ok(Some(value)) if !value.is_empty() => Ok(value),
            Ok(Some(_)) | Ok(None) => Err(D1Error {
                code: missing_code,
                message: format!(
                    "{name} is not configured; set vault.env.{name} with `libra config set \
                     vault.env.{name} <value>` or export {name}"
                ),
            }),
            Err(err) => Err(D1Error {
                code: resolution_error_code,
                message: format!("failed to resolve {name} from vault config or env: {err}"),
            }),
        }
    }

    /// Create a new D1 client with explicit credentials.
    ///
    /// Functional scope:
    /// - Builds an HTTPS-only `reqwest::Client`. If TLS configuration is unavailable
    ///   (extremely unlikely in production), falls back to a default `Client::new()`
    ///   so the constructor itself never fails.
    pub fn new(account_id: String, api_token: String, database_id: String) -> Self {
        // INVARIANT: the default Cloudflare API base URL is a checked-in constant.
        let api_base_url = Url::parse(DEFAULT_D1_API_BASE_URL)
            .expect("DEFAULT_D1_API_BASE_URL must be a valid URL");
        Self::with_api_base_url(account_id, api_token, database_id, api_base_url, true)
    }

    /// Create a D1 client that targets a custom Cloudflare-compatible API base URL.
    ///
    /// This seam is intended for local tests that provide a mock D1 endpoint. The
    /// supplied base URL should point at the Cloudflare API root, e.g.
    /// `http://127.0.0.1:8787/client/v4`; the client appends
    /// `/accounts/<account>/d1/database/<db>/query`.
    pub fn new_with_api_base_url(
        account_id: String,
        api_token: String,
        database_id: String,
        api_base_url: &str,
    ) -> Result<Self, D1Error> {
        let api_base_url = Url::parse(api_base_url).map_err(|e| D1Error {
            code: 2005,
            message: format!("Invalid API base URL: {}", e),
        })?;
        Ok(Self::with_api_base_url(
            account_id,
            api_token,
            database_id,
            api_base_url,
            false,
        ))
    }

    fn with_api_base_url(
        account_id: String,
        api_token: String,
        database_id: String,
        api_base_url: Url,
        https_only: bool,
    ) -> Self {
        let mut builder = Client::builder()
            .connect_timeout(D1_CONNECT_TIMEOUT)
            .timeout(D1_REQUEST_TIMEOUT);
        if https_only {
            builder = builder.https_only(true);
        }
        let client = builder.build().unwrap_or_else(|_| Client::new());
        Self {
            client,
            account_id,
            api_token,
            database_id,
            api_base_url,
        }
    }

    /// Build the per-request `/query` endpoint URL for this account/database.
    ///
    /// Boundary conditions:
    /// - Returns a `D1Error` with code `2005` when appending the account/database
    ///   path to the configured API base URL fails.
    fn api_url(&self) -> Result<Url, D1Error> {
        let mut url = self.api_base_url.clone();
        if !url.path().ends_with('/') {
            let path = format!("{}/", url.path());
            url.set_path(&path);
        }
        url = url
            .join(&format!(
                "accounts/{}/d1/database/{}/query",
                self.account_id, self.database_id
            ))
            .map_err(|e| D1Error {
                code: 2005,
                message: format!("Invalid API URL: {}", e),
            })?;

        Ok(url)
    }

    /// Execute a single SQL statement against the D1 database.
    ///
    /// Functional scope:
    /// - Sends a POST to `/query` with the bearer token and JSON-encoded statement.
    /// - Unwraps Cloudflare's outer `D1Response` and returns the first (and usually
    ///   only) `D1QueryResult` element.
    ///
    /// Boundary conditions:
    /// - Returns `D1Error` codes `2001`/`2002`/`2003` for HTTP, response read, and
    ///   JSON parse failures respectively.
    /// - Returns the raw HTTP status as the error code when D1 responds with non-2xx.
    /// - Returns code `3000` (default) when D1 reports failure with an empty error
    ///   list, otherwise the first element's code.
    /// - Returns code `3001` when the response is well-formed but has no result
    ///   payload — this happens after schema migrations that succeed silently.
    pub async fn execute(
        &self,
        sql: &str,
        params: Option<Vec<serde_json::Value>>,
    ) -> Result<D1QueryResult<serde_json::Value>, D1Error> {
        let statement = D1Statement {
            sql: sql.to_string(),
            params,
        };

        let url = self.api_url()?;

        // D1 writes here are all UPSERTs (`INSERT OR REPLACE` / `ON CONFLICT DO
        // UPDATE`) and reads are pure `SELECT`s, so replaying a statement the
        // server never executed is safe. We therefore retry only when the
        // request provably did not mutate state: a connection that never
        // completed, or an `HTTP 429`/`503` rejection. `Retry-After` is honoured
        // and clamped by the policy. See `docs/development/gap/lore.md` §0.2.
        let policy = RetryPolicy::default();
        let client = &self.client;
        let token = &self.api_token;
        let url_ref = &url;
        let statement_ref = &statement;

        retry_idempotent(&policy, move |_attempt| async move {
            let send_result = match tokio::time::timeout(
                D1_REQUEST_TIMEOUT,
                client
                    .post(url_ref.clone())
                    .header("Authorization", format!("Bearer {}", token))
                    .header("Content-Type", "application/json")
                    .json(statement_ref)
                    .send(),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => {
                    return RetryOutcome::Done(Err(D1Error {
                        code: 2001,
                        message: "D1 request exceeded its 30-second deadline".to_string(),
                    }));
                }
            };

            let response = match send_result {
                Ok(response) => response,
                Err(err) => {
                    // Never surface the reqwest `Debug`/full URL (which can leak
                    // request internals); report the failure class and host only.
                    let message = redact_request_error(&err);
                    // A connection-level failure means the request never reached
                    // the server, so retrying cannot double-apply a write.
                    if err.is_connect() {
                        return RetryOutcome::Retry {
                            retry_after: None,
                            last_err: D1Error {
                                code: 2001,
                                message,
                            },
                        };
                    }
                    return RetryOutcome::Done(Err(D1Error {
                        code: 2001,
                        message,
                    }));
                }
            };

            let status = response.status();
            // 429/503 mean the server rate-limited or was unavailable and did
            // NOT execute the statement, so retrying is safe.
            if matches!(status.as_u16(), 429 | 503) {
                let retry_after = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(parse_retry_after);
                return RetryOutcome::Retry {
                    retry_after,
                    last_err: D1Error {
                        code: i32::from(status.as_u16()),
                        message: format!(
                            "D1 API rate-limited or unavailable (HTTP {})",
                            status.as_u16()
                        ),
                    },
                };
            }

            let body = match tokio::time::timeout(D1_REQUEST_TIMEOUT, response.text()).await {
                Ok(Ok(body)) => body,
                Err(_) => {
                    return RetryOutcome::Done(Err(D1Error {
                        code: 2002,
                        message: "D1 response body exceeded its 30-second deadline".to_string(),
                    }));
                }
                Ok(Err(err)) => {
                    // reqwest's Display can embed the full request URL; route it
                    // through the same host-only redactor as the send path.
                    return RetryOutcome::Done(Err(D1Error {
                        code: 2002,
                        message: redact_request_error(&err),
                    }));
                }
            };

            if !status.is_success() {
                // Do NOT echo the response body: it can carry SQL fragments,
                // identifiers, or backend detail that must not reach logs/errors.
                return RetryOutcome::Done(Err(D1Error {
                    code: i32::from(status.as_u16()),
                    message: format!("D1 API error (HTTP {})", status.as_u16()),
                }));
            }

            let d1_response: D1Response<serde_json::Value> = match serde_json::from_str(&body) {
                Ok(parsed) => parsed,
                Err(err) => {
                    // Report the parse error only, not the raw body.
                    return RetryOutcome::Done(Err(D1Error {
                        code: 2003,
                        message: format!("Failed to parse D1 response: {}", err),
                    }));
                }
            };

            if !d1_response.success {
                let error_msg = d1_response
                    .errors
                    .first()
                    .map(|e| e.message.clone())
                    .unwrap_or_else(|| "Unknown D1 error".to_string());
                return RetryOutcome::Done(Err(D1Error {
                    code: d1_response.errors.first().map(|e| e.code).unwrap_or(3000),
                    message: error_msg,
                }));
            }

            RetryOutcome::Done(
                d1_response
                    .result
                    .and_then(|r| r.into_iter().next())
                    .ok_or_else(|| D1Error {
                        code: 3001,
                        message: "Empty result from D1".to_string(),
                    }),
            )
        })
        .await
    }

    /// Query D1 and deserialise each row into `T`.
    ///
    /// Boundary conditions:
    /// - Returns an empty vector when the statement returns zero rows. Callers that
    ///   want to distinguish "no rows" from "no `results` payload" should use
    ///   [`Self::execute`] directly.
    /// - Returns `D1Error` code `2004` if any row fails to deserialise into `T`;
    ///   the message includes the row index implicitly via the underlying
    ///   `serde_json` error.
    pub async fn query<T: for<'de> Deserialize<'de>>(
        &self,
        sql: &str,
        params: Option<Vec<serde_json::Value>>,
    ) -> Result<Vec<T>, D1Error> {
        let result = self.execute(sql, params).await?;

        let results = result.results.unwrap_or_default();
        let mut typed_results = Vec::with_capacity(results.len());

        for v in results {
            let t: T = serde_json::from_value(v).map_err(|e| D1Error {
                code: 2004,
                message: format!("Failed to deserialize result row: {}", e),
            })?;
            typed_results.push(t);
        }

        Ok(typed_results)
    }

    /// Execute multiple SQL statements in a batch.
    ///
    /// Note: This currently executes statements sequentially as a fallback,
    /// due to potential API compatibility issues with array inputs on the `/query` endpoint.
    ///
    /// Boundary conditions:
    /// - Stops at the first failing statement and returns its error; previously
    ///   committed statements are not rolled back. D1 has no transactional batch
    ///   API for this endpoint, so callers that need atomicity must compose a
    ///   single `BEGIN/COMMIT` SQL string.
    pub async fn batch(
        &self,
        statements: Vec<D1Statement>,
    ) -> Result<Vec<D1QueryResult<serde_json::Value>>, D1Error> {
        let mut results = Vec::new();
        for stmt in statements {
            let query_result = self.execute(&stmt.sql, stmt.params).await?;
            results.push(query_result);
        }

        Ok(results)
    }

    /// Create or migrate the `object_index` table on the D1 side.
    ///
    /// Functional scope:
    /// - Creates the table if it does not exist with the new composite UNIQUE
    ///   constraint `(repo_id, o_id)`.
    /// - When an older table exists with `o_id TEXT NOT NULL UNIQUE` (the legacy
    ///   single-tenant shape), copies rows into a new `object_index_v2` table,
    ///   drops the old table, and renames the new one. This is a destructive
    ///   in-place migration but D1 has no transactional DDL, so partial failure
    ///   leaves a `*_v2` table that the next call will re-attempt to consume.
    /// - Always re-runs `CREATE TABLE IF NOT EXISTS` and the supporting indexes so
    ///   missing indexes are healed on every backup.
    ///
    /// Boundary conditions:
    /// - Returns the underlying `D1Error` from any failing statement. There is no
    ///   automatic rollback; an error during migration leaves the database in a
    ///   half-migrated state that is still consistent (the rename is the last step
    ///   and is atomic from D1's perspective).
    pub async fn ensure_object_index_table(&self) -> Result<(), D1Error> {
        #[derive(Deserialize)]
        struct SqlRow {
            sql: Option<String>,
        }

        let create_v2_sql = r#"
            CREATE TABLE IF NOT EXISTS object_index (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                o_id TEXT NOT NULL,
                o_type TEXT NOT NULL,
                o_size INTEGER NOT NULL,
                repo_id TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                is_synced INTEGER DEFAULT 0,
                UNIQUE(repo_id, o_id)
            )
        "#;

        // Readers trust the generation catalog only while this row exists.
        // Invalidate it before inspecting or rebuilding `object_index`; a
        // failed or interrupted schema repair then leaves readers on the
        // conservative legacy double-read path.
        self.execute(OBJECT_INDEX_CATALOG_READY_TABLE_SQL, None)
            .await?;
        self.execute(OBJECT_INDEX_CATALOG_INVALIDATE_SQL, None)
            .await?;

        let existing: Vec<SqlRow> = self
            .query(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name='object_index'",
                None,
            )
            .await?;

        if existing.is_empty() {
            self.execute(create_v2_sql, None).await?;
        } else {
            let table_sql = existing[0].sql.clone().unwrap_or_default();
            let has_bad_unique = table_sql.contains("o_id TEXT NOT NULL UNIQUE");
            let has_composite_unique = table_sql.contains("UNIQUE(repo_id, o_id)")
                || table_sql.contains("UNIQUE (repo_id, o_id)");

            if has_bad_unique && !has_composite_unique {
                self.execute("DROP TABLE IF EXISTS object_index_v2", None)
                    .await?;
                self.execute(
                    r#"
                        CREATE TABLE object_index_v2 (
                            id INTEGER PRIMARY KEY AUTOINCREMENT,
                            o_id TEXT NOT NULL,
                            o_type TEXT NOT NULL,
                            o_size INTEGER NOT NULL,
                            repo_id TEXT NOT NULL,
                            created_at INTEGER NOT NULL,
                            is_synced INTEGER DEFAULT 0,
                            UNIQUE(repo_id, o_id)
                        )
                    "#,
                    None,
                )
                .await?;

                self.execute(
                    r#"
                        INSERT INTO object_index_v2 (o_id, o_type, o_size, repo_id, created_at, is_synced)
                        SELECT o_id, o_type, o_size, repo_id, created_at, is_synced FROM object_index
                    "#,
                    None,
                )
                .await?;

                self.execute("DROP TABLE object_index", None).await?;
                self.execute("ALTER TABLE object_index_v2 RENAME TO object_index", None)
                    .await?;
            }

            self.execute(create_v2_sql, None).await?;
        }

        self.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_d1_object_repo_oid ON object_index (repo_id, o_id)",
            None,
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_d1_object_repo ON object_index (repo_id)",
            None,
        )
        .await?;

        self.ensure_object_index_catalog_generation().await?;

        Ok(())
    }

    async fn ensure_object_index_catalog_generation(&self) -> Result<(), D1Error> {
        self.execute(
            "CREATE TABLE IF NOT EXISTS object_index_catalog_generation (
                repo_id TEXT PRIMARY KEY,
                generation INTEGER NOT NULL
             )",
            None,
        )
        .await?;
        self.execute(OBJECT_INDEX_CATALOG_SEED_SQL, None).await?;
        for statement in OBJECT_INDEX_CATALOG_TRIGGERS {
            self.execute(statement, None).await?;
        }
        // The D1 endpoint commits each schema statement independently. Publish
        // this table only after every mutation trigger exists so concurrent
        // read-only clients never mistake a partially installed fence for a
        // usable generation catalog.
        self.execute(OBJECT_INDEX_CATALOG_PUBLISH_READY_SQL, None)
            .await?;
        Ok(())
    }

    async fn object_index_catalog_generation_is_ready(&self) -> Result<bool, D1Error> {
        if !self
            .remote_table_exists("object_index_catalog_generation")
            .await?
        {
            return Ok(false);
        }
        if !self
            .remote_table_exists(OBJECT_INDEX_CATALOG_READY_TABLE)
            .await?
        {
            return Ok(false);
        }
        let rows: Vec<D1ExistenceProbe> = self
            .query(
                "SELECT EXISTS(
                   SELECT 1 FROM object_index_catalog_generation_ready
                   WHERE singleton = 1
                 ) AS present",
                None,
            )
            .await?;
        rows.first()
            .map(|row| row.present == 1)
            .ok_or_else(|| D1Error {
                code: 2011,
                message: "object-index catalog readiness probe returned no result row".to_string(),
            })
    }

    async fn object_index_catalog_generation(&self, repo_id: &str) -> Result<i64, D1Error> {
        let rows: Vec<D1CatalogGeneration> = self
            .query(
                "SELECT COALESCE((
                    SELECT generation FROM object_index_catalog_generation WHERE repo_id = ?1
                 ), 0) AS generation",
                Some(vec![serde_json::json!(repo_id)]),
            )
            .await?;
        rows.into_iter()
            .next()
            .map(|row| row.generation)
            .ok_or_else(|| D1Error {
                code: 2011,
                message: "object-index catalog generation probe returned no row".to_string(),
            })
    }

    /// Upsert an `object_index` row keyed by `(repo_id, o_id)`.
    ///
    /// Boundary conditions:
    /// - Sets `is_synced = 1` unconditionally — once Cloudflare accepts the row, the
    ///   client treats the object as synced. Callers must still verify the data
    ///   plane copy if they need stronger guarantees.
    /// - On conflict, the stored `o_type`, `o_size`, and `created_at` are
    ///   overwritten with the values from this call so re-uploads keep the row
    ///   fresh.
    pub async fn upsert_object_index(
        &self,
        o_id: &str,
        o_type: &str,
        o_size: i64,
        repo_id: &str,
        created_at: i64,
    ) -> Result<(), D1Error> {
        let sql = r#"
            INSERT INTO object_index (o_id, o_type, o_size, repo_id, created_at, is_synced)
            VALUES (?1, ?2, ?3, ?4, ?5, 1)
            ON CONFLICT(repo_id, o_id) DO UPDATE SET
                o_type = excluded.o_type,
                o_size = excluded.o_size,
                created_at = excluded.created_at,
                is_synced = 1
        "#;
        let params = vec![
            serde_json::json!(o_id),
            serde_json::json!(o_type),
            serde_json::json!(o_size),
            serde_json::json!(repo_id),
            serde_json::json!(created_at),
        ];
        self.execute(sql, Some(params)).await?;
        Ok(())
    }

    /// Fetch every `object_index` row that belongs to `repo_id` in bounded
    /// pages. Full cloud restore needs the complete list, while agent-capture
    /// fencing uses the explicitly bounded projections below to avoid scanning
    /// unrelated repository history.
    ///
    /// Boundary conditions:
    /// - Returns an empty vector for an unknown `repo_id`; this is treated as "no
    ///   prior backup".
    /// - The compatibility API is intentionally not capped at the much lower
    ///   agent-capture catalog bound; large repositories can legitimately have
    ///   more than 100,000 ordinary objects.
    pub async fn get_object_indexes(&self, repo_id: &str) -> Result<Vec<ObjectIndexRow>, D1Error> {
        self.get_object_indexes_with_generation(repo_id)
            .await
            .map(|(rows, _)| rows)
    }

    pub async fn get_object_indexes_with_generation(
        &self,
        repo_id: &str,
    ) -> Result<(Vec<ObjectIndexRow>, i64), D1Error> {
        self.get_object_indexes_inner(repo_id, None).await
    }

    /// Fetch a full repository object-index projection, failing before more
    /// than `max_rows` are retained. Agent-capture's remote-retention fallback
    /// uses this rather than the unbounded general restore compatibility API.
    pub async fn get_object_indexes_bounded(
        &self,
        repo_id: &str,
        max_rows: usize,
    ) -> Result<Vec<ObjectIndexRow>, D1Error> {
        self.get_object_indexes_bounded_with_generation(repo_id, max_rows)
            .await
            .map(|(rows, _)| rows)
    }

    pub async fn get_object_indexes_bounded_with_generation(
        &self,
        repo_id: &str,
        max_rows: usize,
    ) -> Result<(Vec<ObjectIndexRow>, i64), D1Error> {
        self.get_object_indexes_inner(repo_id, Some(max_rows)).await
    }

    async fn get_object_indexes_inner(
        &self,
        repo_id: &str,
        max_rows: Option<usize>,
    ) -> Result<(Vec<ObjectIndexRow>, i64), D1Error> {
        // Restore/status callers may hold a read-only D1 token. Schema
        // creation belongs to sync; reads must not issue DDL or mutate a
        // legacy backup as a side effect. A backup predating object indexes is
        // therefore an empty generation-zero catalog.
        if !self.remote_table_exists("object_index").await? {
            return Ok((Vec::new(), 0));
        }
        let generation_fenced = self.object_index_catalog_generation_is_ready().await?;
        let sql = "SELECT o_id, o_type, o_size, repo_id, created_at, is_synced
                   FROM object_index
                   WHERE repo_id = ?1 AND o_id > ?3
                   ORDER BY o_id LIMIT ?2";
        let mut previous_legacy_rows: Option<Vec<ObjectIndexRow>> = None;
        for attempt in 0..OBJECT_INDEX_SNAPSHOT_MAX_RETRIES {
            let before = if generation_fenced {
                self.object_index_catalog_generation(repo_id).await?
            } else {
                0
            };
            let mut rows = Vec::new();
            let mut cursor = String::new();
            loop {
                let page: Vec<ObjectIndexRow> = self
                    .query(
                        sql,
                        Some(vec![
                            serde_json::json!(repo_id),
                            serde_json::json!(AGENT_CAPTURE_PAGE_SIZE),
                            serde_json::json!(cursor),
                        ]),
                    )
                    .await?;
                let page_len = page.len();
                if let Some(max_rows) = max_rows
                    && rows.len().saturating_add(page_len) > max_rows
                {
                    return Err(D1Error {
                        code: 2011,
                        message: format!(
                            "remote object-index projection exceeds its {max_rows}-row safety bound"
                        ),
                    });
                }
                if let Some(last) = page.last() {
                    cursor = last.o_id.clone();
                }
                rows.extend(page);
                if page_len < AGENT_CAPTURE_PAGE_SIZE {
                    break;
                }
            }
            let after = if generation_fenced {
                self.object_index_catalog_generation(repo_id).await?
            } else {
                0
            };
            if generation_fenced && before == after {
                return Ok((rows, after));
            }
            if !generation_fenced {
                if previous_legacy_rows.as_ref() == Some(&rows) {
                    return Ok((rows, 0));
                }
                previous_legacy_rows = Some(rows);
            }
            if attempt + 1 == OBJECT_INDEX_SNAPSHOT_MAX_RETRIES {
                return Err(D1Error {
                    code: 2011,
                    message: "remote object-index catalog changed during three bounded reads; retry when cloud sync is idle".to_string(),
                });
            }
        }
        Err(D1Error {
            code: 2011,
            message: "remote object-index snapshot retry loop ended unexpectedly".to_string(),
        })
    }

    /// Read only the requested object-index identities, splitting the lookup
    /// into bounded JSON-table requests. Missing OIDs are omitted so callers
    /// can fail closed with context appropriate to their generation fence.
    pub async fn get_object_indexes_by_oids(
        &self,
        repo_id: &str,
        oids: &[String],
    ) -> Result<Vec<ObjectIndexRow>, D1Error> {
        self.get_object_indexes_by_oids_with_generation(repo_id, oids)
            .await
            .map(|(rows, _)| rows)
    }

    pub async fn get_object_indexes_by_oids_with_generation(
        &self,
        repo_id: &str,
        oids: &[String],
    ) -> Result<(Vec<ObjectIndexRow>, i64), D1Error> {
        if oids.len() > AGENT_CAPTURE_MAX_ROWS_PER_TABLE {
            return Err(D1Error {
                code: 2011,
                message: format!(
                    "required remote object-index projection exceeds the {}-row safety bound",
                    AGENT_CAPTURE_MAX_ROWS_PER_TABLE
                ),
            });
        }
        // Keep the projection usable with read-only restore credentials and
        // with pre-fence backups. Sync is responsible for installing current
        // schema; this read path only probes it.
        if !self.remote_table_exists("object_index").await? {
            return Ok((Vec::new(), 0));
        }
        let generation_fenced = self.object_index_catalog_generation_is_ready().await?;
        let mut previous_legacy_rows: Option<Vec<ObjectIndexRow>> = None;
        for attempt in 0..OBJECT_INDEX_SNAPSHOT_MAX_RETRIES {
            let before = if generation_fenced {
                self.object_index_catalog_generation(repo_id).await?
            } else {
                0
            };
            let mut unique = HashSet::with_capacity(oids.len());
            let mut ordered = Vec::with_capacity(oids.len());
            for oid in oids {
                if unique.insert(oid.as_str()) {
                    ordered.push(oid);
                }
            }
            let mut rows = Vec::with_capacity(ordered.len());
            for page in ordered.chunks(AGENT_CAPTURE_PAGE_SIZE) {
                let encoded = serde_json::to_string(page).map_err(|error| D1Error {
                    code: 2004,
                    message: format!("encode required object-index lookup: {error}"),
                })?;
                let mut found: Vec<ObjectIndexRow> = self
                    .query(
                        "SELECT o_id, o_type, o_size, repo_id, created_at, is_synced
                         FROM object_index
                         WHERE repo_id = ?1
                           AND o_id IN (SELECT value FROM json_each(?2))
                         ORDER BY o_id",
                        Some(vec![serde_json::json!(repo_id), serde_json::json!(encoded)]),
                    )
                    .await?;
                rows.append(&mut found);
            }
            rows.sort_by(|left, right| left.o_id.cmp(&right.o_id));
            if rows.windows(2).any(|pair| pair[0].o_id == pair[1].o_id) {
                return Err(D1Error {
                    code: 2011,
                    message: "remote object-index projection contains a duplicate OID".to_string(),
                });
            }
            let after = if generation_fenced {
                self.object_index_catalog_generation(repo_id).await?
            } else {
                0
            };
            if generation_fenced && before == after {
                return Ok((rows, after));
            }
            if !generation_fenced {
                if previous_legacy_rows.as_ref() == Some(&rows) {
                    return Ok((rows, 0));
                }
                previous_legacy_rows = Some(rows);
            }
            if attempt + 1 == OBJECT_INDEX_SNAPSHOT_MAX_RETRIES {
                return Err(D1Error {
                    code: 2011,
                    message: "remote object-index projection changed during three bounded reads; retry when cloud sync is idle".to_string(),
                });
            }
        }
        Err(D1Error {
            code: 2011,
            message: "remote object-index projection retry loop ended unexpectedly".to_string(),
        })
    }

    /// Create the `repositories` table on the D1 side if it does not already exist.
    ///
    /// Boundary conditions:
    /// - Idempotent — safe to call on every backup.
    /// - The `name` column is `UNIQUE`; this is what enables the conflict-detection
    ///   path inside [`Self::upsert_repository`].
    pub async fn ensure_repositories_table(&self) -> Result<(), D1Error> {
        let sql = r#"
            CREATE TABLE IF NOT EXISTS repositories (
                repo_id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            )
        "#;
        self.execute(sql, None).await?;
        Ok(())
    }

    /// Upsert a repository row, gracefully resolving name conflicts.
    ///
    /// This function handles three cases:
    /// 1. New repository: Inserts a new record.
    /// 2. Existing repository (same repo_id): Updates the name and timestamp.
    /// 3. Name conflict (different repo_id): Returns the existing repository row
    ///    that already owns this `name`.
    ///
    /// Callers that need to enforce unique names per logical repository must
    /// compare the returned `repo_id` with the one they attempted to upsert; if
    /// they differ, a logical name conflict has occurred.
    ///
    /// Boundary conditions:
    /// - The conflict path string-matches both `UNIQUE constraint failed:
    ///   repositories.name` and the more generic `SQLITE_CONSTRAINT` so that
    ///   wording differences across D1/SQLite versions do not cause a regression.
    /// - Returns `D1Error` code `3002` if the upsert returns no row (D1 docs allow
    ///   `RETURNING` to come back empty in degenerate cases).
    pub async fn upsert_repository(
        &self,
        repo_id: &str,
        name: &str,
    ) -> Result<RepositoryRow, D1Error> {
        let now = chrono::Utc::now().timestamp();
        // Try to insert or update existing repo_id (renaming project)
        let sql = r#"
            INSERT INTO repositories (repo_id, name, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(repo_id) DO UPDATE SET
                name = excluded.name,
                updated_at = excluded.updated_at
            RETURNING repo_id, name, created_at, updated_at
        "#;
        let params = vec![
            serde_json::json!(repo_id),
            serde_json::json!(name),
            serde_json::json!(now),
            serde_json::json!(now),
        ];

        match self.query(sql, Some(params)).await {
            Ok(rows) => rows.into_iter().next().ok_or_else(|| D1Error {
                code: 3002,
                message: "Failed to upsert repository".to_string(),
            }),
            Err(e) => {
                // Check if error is due to name conflict (UNIQUE constraint on name)
                if e.message
                    .contains("UNIQUE constraint failed: repositories.name")
                    || e.message.contains("SQLITE_CONSTRAINT")
                {
                    // Fetch the existing repository that owns this name
                    let existing_sql = "SELECT repo_id, name, created_at, updated_at FROM repositories WHERE name = ?1";
                    let existing_rows: Vec<RepositoryRow> = self
                        .query(existing_sql, Some(vec![serde_json::json!(name)]))
                        .await?;
                    existing_rows.into_iter().next().ok_or(e)
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Look up a repository's `repo_id` by its human-readable name.
    ///
    /// Boundary conditions:
    /// - Returns `Ok(None)` when no row matches; only forwards database-level
    ///   errors as `Err`.
    pub async fn get_repo_id_by_name(&self, name: &str) -> Result<Option<String>, D1Error> {
        #[derive(Deserialize)]
        struct IdRow {
            repo_id: String,
        }
        let sql = "SELECT repo_id FROM repositories WHERE name = ?1";
        let result: Vec<IdRow> = self.query(sql, Some(vec![serde_json::json!(name)])).await?;
        Ok(result.into_iter().next().map(|r| r.repo_id))
    }

    /// Look up a repository row by stable `repo_id`.
    ///
    /// Cloudflare clone restore uses the `publish_sites.repo_id`
    /// binding to verify that the backing backup metadata exists
    /// before it starts creating a local repository.
    pub async fn find_repository(&self, repo_id: &str) -> Result<Option<RepositoryRow>, D1Error> {
        let sql =
            "SELECT repo_id, name, created_at, updated_at FROM repositories WHERE repo_id = ?1";
        let rows: Vec<RepositoryRow> = self
            .query(sql, Some(vec![serde_json::json!(repo_id)]))
            .await?;
        Ok(rows.into_iter().next())
    }

    // ── CEX-EntireIO §10.2: agent_session / agent_checkpoint mirroring ──

    /// Create the `agent_session` table on the D1 side.
    ///
    /// Mirrors a subset of the local SQLite schema (see
    /// `sql/migrations/2026050303_agent_capture.sql`) — only the columns
    /// `libra cloud sync` needs to round-trip a session listing on a fresh
    /// machine. The `agent_kind` CHECK matches the local CHECK so a
    /// future widening migration on either side stays in lock-step.
    ///
    /// **Intentional divergences from the local schema** (operators
    /// debugging cloud-vs-local drift should know about these). Each
    /// bullet names the responsible team and the planned revisit window
    /// so a future operator can chase the right thread:
    ///
    /// - **No FK to `ai_thread(thread_id)`**. D1 does not host the
    ///   `ai_thread` table; `thread_id` is always NULL in v1
    ///   (`docs/development/commands/_general.md` §11.3). **Owner**: cloud-sync
    ///   path (this module). **Revisit**: Phase 4 migration that
    ///   replicates `ai_thread` to D1; until then, treat any non-NULL
    ///   `thread_id` rows as a local-only join key.
    /// - **No `ON DELETE CASCADE`** between session and checkpoint.
    ///   D1 typically does not enforce FKs, so cascades would be a
    ///   no-op even if declared. Orphan-row reconciliation is the
    ///   caller's responsibility — `libra agent clean` handles the
    ///   local side, and a future Phase 3 follow-up will add the D1
    ///   side.
    /// - **No payload size cap on `metadata_json` / `redaction_report`**.
    ///   D1 has its own row-size cap; we rely on the local
    ///   `Redactor::DEFAULT_RULES` keeping these blobs small in
    ///   practice. **Owner**: redaction module
    ///   (`observed_agents::redaction`). **Revisit**: Phase 4 if D1
    ///   row-size violations are observed in production sync logs;
    ///   Phase 3 already telemeters bytes_redacted via the report so
    ///   the trigger condition is observable from the agent_session
    ///   row itself.
    ///
    /// Idempotent — safe to call on every backup.
    pub async fn ensure_agent_session_table(&self) -> Result<(), D1Error> {
        let sql = r#"
            CREATE TABLE IF NOT EXISTS agent_session (
                session_id TEXT NOT NULL,
                repo_id TEXT NOT NULL,
                agent_kind TEXT NOT NULL CHECK(agent_kind IN (
                    'claude_code', 'cursor', 'codex', 'gemini',
                    'opencode', 'copilot', 'factory_ai'
                )),
                provider_session_id TEXT NOT NULL,
                state TEXT NOT NULL,
                working_dir TEXT NOT NULL,
                worktree_id TEXT,
                parent_commit TEXT,
                parent_session_id TEXT,
                metadata_json TEXT NOT NULL DEFAULT '{}',
                redaction_report TEXT NOT NULL DEFAULT '{}',
                started_at INTEGER NOT NULL,
                last_event_at INTEGER NOT NULL,
                stopped_at INTEGER,
                schema_version INTEGER NOT NULL DEFAULT 1,
                sync_revision INTEGER NOT NULL DEFAULT 1,
                synced_at INTEGER NOT NULL,
                PRIMARY KEY (repo_id, session_id)
            )
        "#;
        self.execute(sql, None).await?;
        self.ensure_remote_column(
            "agent_session",
            "sync_revision",
            "ALTER TABLE agent_session ADD COLUMN sync_revision INTEGER NOT NULL DEFAULT 1",
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_d1_agent_session_repo ON agent_session (repo_id)",
            None,
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_d1_agent_session_kind \
             ON agent_session (repo_id, agent_kind)",
            None,
        )
        .await?;
        self.execute(
            r#"
            CREATE TABLE IF NOT EXISTS agent_capture_session_v2 (
                session_id TEXT NOT NULL,
                repo_id TEXT NOT NULL,
                agent_kind TEXT NOT NULL CHECK(agent_kind IN (
                    'claude_code', 'cursor', 'codex', 'gemini',
                    'opencode', 'copilot', 'factory_ai'
                )),
                provider_session_id TEXT NOT NULL,
                state TEXT NOT NULL,
                working_dir TEXT NOT NULL,
                worktree_id TEXT,
                parent_commit TEXT,
                parent_session_id TEXT,
                metadata_json TEXT NOT NULL DEFAULT '{}',
                redaction_report TEXT NOT NULL DEFAULT '{}',
                started_at INTEGER NOT NULL,
                last_event_at INTEGER NOT NULL,
                stopped_at INTEGER,
                schema_version INTEGER NOT NULL DEFAULT 1,
                sync_revision INTEGER NOT NULL,
                synced_at INTEGER NOT NULL,
                PRIMARY KEY (repo_id, session_id)
            )
            "#,
            None,
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_d1_agent_capture_session_v2_repo \
             ON agent_capture_session_v2 (repo_id, agent_kind)",
            None,
        )
        .await?;
        Ok(())
    }

    /// Create the `agent_checkpoint` table on the D1 side.
    ///
    /// As with `agent_session`, this mirrors the local-side schema minus
    /// the FK constraint (`ON DELETE CASCADE` from session → checkpoint
    /// would require D1 to enforce FKs, which the host typically does
    /// not). Cleanup of orphan checkpoints is therefore the caller's
    /// responsibility — `libra agent clean` handles this on the local
    /// side; D1 garbage rows would persist until a future
    /// `libra cloud sync` reconciliation.
    ///
    /// Idempotent — safe to call on every backup.
    pub async fn ensure_agent_checkpoint_table(&self) -> Result<(), D1Error> {
        let sql = r#"
            CREATE TABLE IF NOT EXISTS agent_checkpoint (
                checkpoint_id TEXT NOT NULL,
                repo_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                parent_checkpoint_id TEXT,
                scope TEXT NOT NULL CHECK(scope IN ('temporary','committed','subagent')),
                parent_commit TEXT,
                tree_oid TEXT NOT NULL,
                metadata_blob_oid TEXT NOT NULL,
                traces_commit TEXT NOT NULL,
                tool_use_id TEXT,
                subagent_session_id TEXT,
                description TEXT,
                created_at INTEGER NOT NULL,
                synced_at INTEGER NOT NULL,
                PRIMARY KEY (repo_id, checkpoint_id)
            )
        "#;
        self.execute(sql, None).await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_d1_agent_checkpoint_session \
             ON agent_checkpoint (repo_id, session_id, created_at)",
            None,
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_d1_agent_checkpoint_scope \
             ON agent_checkpoint (repo_id, scope)",
            None,
        )
        .await?;
        self.execute(
            r#"
            CREATE TABLE IF NOT EXISTS agent_capture_checkpoint_v2 (
                checkpoint_id TEXT NOT NULL,
                repo_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                parent_checkpoint_id TEXT,
                scope TEXT NOT NULL CHECK(scope IN ('temporary','committed','subagent')),
                parent_commit TEXT,
                tree_oid TEXT NOT NULL,
                metadata_blob_oid TEXT NOT NULL,
                traces_commit TEXT NOT NULL,
                tool_use_id TEXT,
                subagent_session_id TEXT,
                description TEXT,
                created_at INTEGER NOT NULL,
                sync_revision INTEGER NOT NULL DEFAULT 0,
                synced_at INTEGER NOT NULL,
                PRIMARY KEY (repo_id, checkpoint_id)
            )
            "#,
            None,
        )
        .await?;
        self.ensure_remote_column(
            "agent_capture_checkpoint_v2",
            "sync_revision",
            "ALTER TABLE agent_capture_checkpoint_v2 ADD COLUMN sync_revision INTEGER NOT NULL DEFAULT 0",
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_d1_agent_capture_checkpoint_v2_session \
             ON agent_capture_checkpoint_v2 (repo_id, session_id, created_at)",
            None,
        )
        .await?;
        Ok(())
    }

    pub async fn ensure_agent_checkpoint_prune_tombstone_table(&self) -> Result<(), D1Error> {
        self.execute(
            r#"
            CREATE TABLE IF NOT EXISTS agent_checkpoint_prune_tombstone (
                repo_id TEXT NOT NULL,
                checkpoint_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                pruned_at INTEGER NOT NULL,
                synced_at INTEGER NOT NULL,
                PRIMARY KEY (repo_id, checkpoint_id)
            )
            "#,
            None,
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_d1_checkpoint_prune_tombstone_session
             ON agent_checkpoint_prune_tombstone (repo_id, session_id, pruned_at)",
            None,
        )
        .await?;
        Ok(())
    }

    /// PD-03: session-erasure tombstones on D1 (delete/restore
    /// idempotent, `erased_at` monotone under replays).
    pub async fn ensure_agent_import_tombstone_table(&self) -> Result<(), D1Error> {
        self.execute(
            r#"
            CREATE TABLE IF NOT EXISTS agent_import_tombstone (
                repo_id TEXT NOT NULL,
                agent_kind TEXT NOT NULL,
                provider_session_id TEXT NOT NULL,
                erased_session_id TEXT NOT NULL,
                source_fingerprint TEXT,
                erased_at INTEGER NOT NULL,
                synced_at INTEGER NOT NULL,
                PRIMARY KEY (repo_id, agent_kind, provider_session_id)
            )
            "#,
            None,
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_d1_agent_import_tombstone_session
             ON agent_import_tombstone (repo_id, erased_session_id)",
            None,
        )
        .await?;
        Ok(())
    }

    /// Generation fence for the multi-request agent-capture mirror. A writer
    /// publishes under a unique token; takeover advances the generation and
    /// fences every older request in the same SQL statement that applies rows.
    pub async fn ensure_agent_capture_generation_table(&self) -> Result<(), D1Error> {
        self.execute(
            r#"
            CREATE TABLE IF NOT EXISTS agent_capture_generation (
                repo_id TEXT PRIMARY KEY,
                generation INTEGER NOT NULL,
                state TEXT NOT NULL CHECK(state IN ('publishing', 'complete')),
                writer_token TEXT,
                object_index_digest TEXT,
                object_index_count INTEGER,
                object_index_scope TEXT,
                object_index_generation INTEGER,
                traces_head TEXT,
                started_at INTEGER NOT NULL,
                completed_at INTEGER
            )
            "#,
            None,
        )
        .await?;
        self.ensure_remote_column(
            "agent_capture_generation",
            "object_index_digest",
            "ALTER TABLE agent_capture_generation ADD COLUMN object_index_digest TEXT",
        )
        .await?;
        self.ensure_remote_column(
            "agent_capture_generation",
            "object_index_count",
            "ALTER TABLE agent_capture_generation ADD COLUMN object_index_count INTEGER",
        )
        .await?;
        self.ensure_remote_column(
            "agent_capture_generation",
            "object_index_scope",
            "ALTER TABLE agent_capture_generation ADD COLUMN object_index_scope TEXT",
        )
        .await?;
        self.ensure_remote_column(
            "agent_capture_generation",
            "object_index_generation",
            "ALTER TABLE agent_capture_generation ADD COLUMN object_index_generation INTEGER",
        )
        .await?;
        self.ensure_remote_column(
            "agent_capture_generation",
            "traces_head",
            "ALTER TABLE agent_capture_generation ADD COLUMN traces_head TEXT",
        )
        .await?;
        self.execute(
            r#"
            CREATE TABLE IF NOT EXISTS agent_capture_schema_migration (
                version INTEGER PRIMARY KEY,
                state TEXT NOT NULL CHECK(state IN ('copying', 'complete')),
                completed_at INTEGER
            )
            "#,
            None,
        )
        .await?;
        // Fence every legacy writer before taking the one-time adoption
        // snapshot. Once these triggers exist, an older client can neither
        // race a mutation between the copy and the completion marker nor
        // append state that v2 will never adopt. If adoption is interrupted,
        // the triggers remain installed and the current client safely resumes
        // the copy on its next attempt.
        for statement in AGENT_CAPTURE_LEGACY_WRITE_BARRIERS {
            self.execute(statement, None).await?;
        }
        self.execute(
            "INSERT INTO agent_capture_schema_migration (version, state, completed_at)
             VALUES (2, 'copying', NULL)
             ON CONFLICT(version) DO NOTHING",
            None,
        )
        .await?;
        // Legacy rows are adopted once at generation 0. A current local row
        // starts at generation 1, so the first fenced sync can deterministically
        // supersede divergent legacy state. The complete marker prevents any
        // later legacy writer from being silently imported into the v2 snapshot.
        self.execute(AGENT_CAPTURE_LEGACY_SESSION_ADOPTION_SQL, None)
            .await?;
        self.execute(AGENT_CAPTURE_LEGACY_CHECKPOINT_ADOPTION_SQL, None)
            .await?;
        // The previous best-effort mirror deliberately continued checkpoint
        // uploads after a session-row failure, so a legitimate legacy backup
        // can contain checkpoint orphans. They were never restorable. Remove
        // them from the adopted v2 snapshot before enabling strict dependency
        // validation; current local rows can then republish a coherent pair.
        self.execute(AGENT_CAPTURE_LEGACY_ORPHAN_CLEANUP_SQL, None)
            .await?;
        self.execute(
            "UPDATE agent_capture_schema_migration
             SET state = 'complete', completed_at = CAST(strftime('%s', 'now') AS INTEGER)
             WHERE version = 2 AND state = 'copying'",
            None,
        )
        .await?;
        Ok(())
    }

    pub async fn begin_agent_capture_generation(
        &self,
        repo_id: &str,
        writer_token: &str,
        manifest: AgentCaptureGenerationManifest<'_>,
    ) -> Result<AgentCaptureGenerationRow, D1Error> {
        let expected_generation = self
            .get_agent_capture_generation(repo_id)
            .await?
            .map(|generation| generation.generation);
        self.begin_agent_capture_generation_from(
            repo_id,
            writer_token,
            expected_generation,
            manifest,
        )
        .await
    }

    /// Start a publication only if the remote generation still equals the
    /// caller's observed generation. A completed generation can advance
    /// immediately; an abandoned `publishing` generation can advance only
    /// after its server-timestamped lease expires. `None` is valid only for a
    /// repository that still has no generation row. The source-compatible
    /// convenience entry point above observes the current generation and then
    /// delegates here, so both APIs enforce the same compare-and-swap fence.
    pub async fn begin_agent_capture_generation_from(
        &self,
        repo_id: &str,
        writer_token: &str,
        expected_generation: Option<i64>,
        manifest: AgentCaptureGenerationManifest<'_>,
    ) -> Result<AgentCaptureGenerationRow, D1Error> {
        let AgentCaptureGenerationManifest {
            object_index_digest,
            object_index_count,
            object_index_scope,
            object_index_generation,
            traces_head,
        } = manifest;
        self.ensure_object_index_table().await?;
        let values = vec![
            serde_json::json!(repo_id),
            serde_json::json!(writer_token),
            serde_json::json!(object_index_digest),
            serde_json::json!(object_index_count),
            serde_json::json!(object_index_scope),
            serde_json::json!(object_index_generation),
            serde_json::json!(traces_head),
        ];
        let rows: Vec<AgentCaptureGenerationRow> =
            if let Some(expected_generation) = expected_generation {
                let mut values = values;
                values.push(serde_json::json!(expected_generation));
                self.query(AGENT_CAPTURE_BEGIN_GENERATION_FROM_SQL, {
                    values.push(serde_json::json!(AGENT_CAPTURE_GENERATION_LEASE_SECONDS));
                    Some(values)
                })
                .await?
            } else {
                self.query(
                    r#"
                INSERT INTO agent_capture_generation (
                    repo_id, generation, state, writer_token, object_index_digest,
                    object_index_count, object_index_scope, object_index_generation,
                    traces_head, started_at, completed_at
                ) VALUES (?1, 1, 'publishing', ?2, ?3, ?4, ?5, ?6, ?7,
                          CAST(strftime('%s', 'now') AS INTEGER), NULL)
                ON CONFLICT(repo_id) DO NOTHING
                RETURNING repo_id, generation, state, writer_token, object_index_digest,
                          object_index_count, object_index_scope, object_index_generation,
                          traces_head, started_at, completed_at
                "#,
                    Some(values),
                )
                .await?
            };
        rows.into_iter().next().ok_or_else(|| D1Error {
            code: 3004,
            message: "agent-capture remote generation changed or still has an active publisher; retry after the current 5-minute publication lease expires"
                .to_string(),
        })
    }

    pub async fn get_agent_capture_generation(
        &self,
        repo_id: &str,
    ) -> Result<Option<AgentCaptureGenerationRow>, D1Error> {
        let rows: Vec<AgentCaptureGenerationRow> = self
            .query(
                "SELECT repo_id, generation, state, writer_token, object_index_digest,
                        object_index_count, object_index_scope, object_index_generation,
                        traces_head, started_at, completed_at
                 FROM agent_capture_generation WHERE repo_id = ?1",
                Some(vec![serde_json::json!(repo_id)]),
            )
            .await?;
        Ok(rows.into_iter().next())
    }

    pub async fn complete_agent_capture_generation(
        &self,
        repo_id: &str,
        writer_token: &str,
        expected_object_index_generation: i64,
    ) -> Result<AgentCaptureGenerationRow, D1Error> {
        let rows: Vec<AgentCaptureGenerationRow> = self
            .query(
                AGENT_CAPTURE_COMPLETE_GENERATION_SQL,
                Some(vec![
                    serde_json::json!(repo_id),
                    serde_json::json!(writer_token),
                    serde_json::json!(expected_object_index_generation),
                ]),
            )
            .await?;
        rows.into_iter().next().ok_or_else(|| D1Error {
            code: 3003,
            message: "agent-capture publication lost its remote generation fence".to_string(),
        })
    }

    /// Retained source-compatible single-row API. Fenced v2 capture
    /// generations cannot safely accept an unscoped write, so callers receive
    /// an upgrade error and must use the generation batch workflow.
    pub async fn upsert_agent_session(
        &self,
        repo_id: &str,
        row: &AgentSessionRow,
    ) -> Result<(), D1Error> {
        // Retain this public compatibility surface as a fail-closed API. The
        // v2 snapshot may only change through the token-fenced batch methods;
        // accepting an unscoped single-row write would let restore observe a
        // mutation under an unchanged completed manifest.
        let _ = (repo_id, row);
        reject_unfenced_agent_capture_write("agent session")
    }

    /// Read every `agent_session` row for a repo. Used by
    /// external compatibility callers that consume the pre-v2 projection.
    /// Current cloud restore uses [`Self::list_agent_session_v2_rows`].
    pub async fn list_agent_sessions(
        &self,
        repo_id: &str,
    ) -> Result<Vec<AgentSessionRow>, D1Error> {
        let table =
            agent_session_compatibility_table(self.agent_capture_v2_projection_is_ready().await?);
        let sql = format!(
            r#"
            SELECT session_id, agent_kind, provider_session_id, state, working_dir,
                   worktree_id, parent_commit, parent_session_id, metadata_json,
                   redaction_report, started_at, last_event_at, stopped_at, schema_version
            FROM {table}
            WHERE repo_id = ?1
            ORDER BY session_id
            LIMIT ?2 OFFSET ?3
        "#
        );
        self.collect_agent_capture_pages(&sql, repo_id, "agent session")
            .await
    }

    /// Versioned session projection used by token-fenced capture generations.
    pub async fn list_agent_session_v2_rows(
        &self,
        repo_id: &str,
    ) -> Result<Vec<AgentSessionV2Row>, D1Error> {
        let mut remaining_rows = AGENT_CAPTURE_MAX_ROWS_PER_TABLE;
        self.list_agent_session_v2_rows_with_budget(repo_id, &mut remaining_rows)
            .await
    }

    async fn list_agent_session_v2_rows_with_budget(
        &self,
        repo_id: &str,
        remaining_rows: &mut usize,
    ) -> Result<Vec<AgentSessionV2Row>, D1Error> {
        let sql = r#"
            SELECT session_id, agent_kind, provider_session_id, state, working_dir,
                   worktree_id, parent_commit, parent_session_id, metadata_json,
                   redaction_report, started_at, last_event_at, stopped_at, schema_version,
                   sync_revision
            FROM agent_capture_session_v2
            WHERE repo_id = ?1
            ORDER BY session_id
            LIMIT ?2 OFFSET ?3
        "#;
        self.collect_agent_capture_pages_with_budget(
            sql,
            repo_id,
            "agent session v2",
            remaining_rows,
        )
        .await
    }

    /// Read every `agent_checkpoint` row for a repo. Used by
    /// `libra cloud restore` together with
    /// [`Self::list_agent_sessions`].
    pub async fn list_agent_checkpoints(
        &self,
        repo_id: &str,
    ) -> Result<Vec<AgentCheckpointRow>, D1Error> {
        let table = agent_checkpoint_compatibility_table(
            self.agent_capture_v2_projection_is_ready().await?,
        );
        let sql = format!(
            r#"
            SELECT checkpoint_id, session_id, parent_checkpoint_id, scope, parent_commit,
                   tree_oid, metadata_blob_oid, traces_commit, tool_use_id,
                   subagent_session_id, description, created_at
            FROM {table}
            WHERE repo_id = ?1
            ORDER BY created_at ASC, checkpoint_id ASC
            LIMIT ?2 OFFSET ?3
        "#
        );
        self.collect_agent_capture_pages(&sql, repo_id, "agent checkpoint")
            .await
    }

    /// Versioned checkpoint projection with the monotonic rewrite generation
    /// used by the fenced cloud protocol.
    pub async fn list_agent_checkpoint_v2_rows(
        &self,
        repo_id: &str,
    ) -> Result<Vec<AgentCheckpointV2Row>, D1Error> {
        let mut remaining_rows = AGENT_CAPTURE_MAX_ROWS_PER_TABLE;
        self.list_agent_checkpoint_v2_rows_with_budget(repo_id, &mut remaining_rows)
            .await
    }

    async fn list_agent_checkpoint_v2_rows_with_budget(
        &self,
        repo_id: &str,
        remaining_rows: &mut usize,
    ) -> Result<Vec<AgentCheckpointV2Row>, D1Error> {
        let sql = r#"
            SELECT checkpoint_id, session_id, parent_checkpoint_id, scope, parent_commit,
                   tree_oid, metadata_blob_oid, traces_commit, tool_use_id,
                   subagent_session_id, description, created_at, sync_revision
            FROM agent_capture_checkpoint_v2
            WHERE repo_id = ?1
            ORDER BY created_at ASC, checkpoint_id ASC
            LIMIT ?2 OFFSET ?3
        "#;
        self.collect_agent_capture_pages_with_budget(
            sql,
            repo_id,
            "agent checkpoint v2",
            remaining_rows,
        )
        .await
    }

    /// Return the subset of checkpoint ids present in the v2 remote catalog.
    /// Restore preflight uses this bounded lookup to detect local prune fences
    /// without materializing the entire remote checkpoint table first.
    pub async fn find_agent_checkpoint_ids_by_ids(
        &self,
        repo_id: &str,
        checkpoint_ids: &[String],
    ) -> Result<HashSet<String>, D1Error> {
        if checkpoint_ids.len() > AGENT_CAPTURE_MAX_ROWS_PER_TABLE {
            return Err(D1Error {
                code: 2011,
                message: format!(
                    "local checkpoint prune preflight exceeds the {}-row safety bound",
                    AGENT_CAPTURE_MAX_ROWS_PER_TABLE
                ),
            });
        }
        let mut matches = HashSet::with_capacity(checkpoint_ids.len());
        for page in checkpoint_ids.chunks(AGENT_CAPTURE_PAGE_SIZE) {
            let encoded = serde_json::to_string(page).map_err(|error| D1Error {
                code: 2004,
                message: format!("encode checkpoint prune preflight lookup: {error}"),
            })?;
            let rows: Vec<AgentCheckpointIdRow> = self
                .query(
                    "SELECT checkpoint_id FROM agent_capture_checkpoint_v2
                     WHERE repo_id = ?1
                       AND checkpoint_id IN (SELECT value FROM json_each(?2))
                     ORDER BY checkpoint_id",
                    Some(vec![serde_json::json!(repo_id), serde_json::json!(encoded)]),
                )
                .await?;
            for row in rows {
                if !matches.insert(row.checkpoint_id.clone()) {
                    return Err(D1Error {
                        code: 2011,
                        message: format!(
                            "remote checkpoint prune preflight returned duplicate id {}",
                            row.checkpoint_id
                        ),
                    });
                }
            }
        }
        Ok(matches)
    }

    pub async fn list_agent_checkpoint_prune_tombstones(
        &self,
        repo_id: &str,
    ) -> Result<Vec<AgentCheckpointPruneTombstoneRow>, D1Error> {
        let mut remaining_rows = AGENT_CAPTURE_MAX_ROWS_PER_TABLE;
        self.list_agent_checkpoint_prune_tombstones_with_budget(repo_id, &mut remaining_rows)
            .await
    }

    async fn list_agent_checkpoint_prune_tombstones_with_budget(
        &self,
        repo_id: &str,
        remaining_rows: &mut usize,
    ) -> Result<Vec<AgentCheckpointPruneTombstoneRow>, D1Error> {
        self.collect_agent_capture_pages_with_budget(
            "SELECT checkpoint_id, session_id, pruned_at
             FROM agent_checkpoint_prune_tombstone WHERE repo_id = ?1
             ORDER BY checkpoint_id LIMIT ?2 OFFSET ?3",
            repo_id,
            "checkpoint prune tombstone",
            remaining_rows,
        )
        .await
    }

    async fn list_agent_import_tombstones_with_budget(
        &self,
        repo_id: &str,
        remaining_rows: &mut usize,
    ) -> Result<Vec<AgentImportTombstoneRow>, D1Error> {
        self.collect_agent_capture_pages_with_budget(
            "SELECT agent_kind, provider_session_id, erased_session_id,
                    source_fingerprint, erased_at
             FROM agent_import_tombstone WHERE repo_id = ?1
             ORDER BY agent_kind, provider_session_id LIMIT ?2 OFFSET ?3",
            repo_id,
            "agent import tombstone",
            remaining_rows,
        )
        .await
    }

    async fn collect_agent_capture_pages<T: for<'de> Deserialize<'de>>(
        &self,
        sql: &str,
        repo_id: &str,
        label: &str,
    ) -> Result<Vec<T>, D1Error> {
        let mut remaining_rows = AGENT_CAPTURE_MAX_ROWS_PER_TABLE;
        self.collect_agent_capture_pages_with_budget(sql, repo_id, label, &mut remaining_rows)
            .await
    }

    async fn collect_agent_capture_pages_with_budget<T: for<'de> Deserialize<'de>>(
        &self,
        sql: &str,
        repo_id: &str,
        label: &str,
        remaining_rows: &mut usize,
    ) -> Result<Vec<T>, D1Error> {
        let mut rows = Vec::new();
        let mut offset = 0_usize;
        loop {
            // Ask for one row beyond the remaining aggregate allowance when
            // possible, so an exhausted budget distinguishes an empty table
            // from a remote snapshot that would cross the restore-wide cap.
            let page_size = remaining_rows
                .saturating_add(1)
                .clamp(1, AGENT_CAPTURE_PAGE_SIZE);
            let page: Vec<T> = self
                .query(
                    sql,
                    Some(vec![
                        serde_json::json!(repo_id),
                        serde_json::json!(page_size),
                        serde_json::json!(offset),
                    ]),
                )
                .await?;
            let page_len = page.len();
            charge_agent_capture_restore_rows(remaining_rows, page_len, label)?;
            rows.extend(page);
            if page_len < page_size {
                break;
            }
            offset = offset.saturating_add(page_len);
        }
        Ok(rows)
    }

    /// Read one coherent capture-catalog projection under a single aggregate
    /// row budget. Cloud restore uses this instead of applying the safety cap
    /// independently to each companion table.
    pub async fn list_agent_capture_restore_catalog_rows(
        &self,
        repo_id: &str,
        include_subagent_content: bool,
        max_rows: usize,
    ) -> Result<AgentCaptureRestoreCatalogRows, D1Error> {
        let mut remaining_rows = max_rows;
        let sessions = self
            .list_agent_session_v2_rows_with_budget(repo_id, &mut remaining_rows)
            .await?;
        let checkpoints = self
            .list_agent_checkpoint_v2_rows_with_budget(repo_id, &mut remaining_rows)
            .await?;
        let prune_tombstones = self
            .list_agent_checkpoint_prune_tombstones_with_budget(repo_id, &mut remaining_rows)
            .await?;
        // Pre-PD-03 remotes have no tombstone table; restore is a
        // read-only consumer and must not create it.
        let import_tombstones = if self.agent_import_tombstone_table_exists().await? {
            self.list_agent_import_tombstones_with_budget(repo_id, &mut remaining_rows)
                .await?
        } else {
            Vec::new()
        };
        let (claims, revisions, links) = if include_subagent_content {
            (
                self.list_agent_subagent_content_claims_with_budget(repo_id, &mut remaining_rows)
                    .await?,
                self.list_agent_subagent_content_revisions_with_budget(
                    repo_id,
                    &mut remaining_rows,
                )
                .await?,
                self.list_agent_subagent_links_with_budget(repo_id, &mut remaining_rows)
                    .await?,
            )
        } else {
            (Vec::new(), Vec::new(), Vec::new())
        };
        Ok(AgentCaptureRestoreCatalogRows {
            sessions,
            checkpoints,
            prune_tombstones,
            import_tombstones,
            claims,
            revisions,
            links,
            remaining_rows,
        })
    }

    async fn remote_column_exists(&self, table: &str, column: &str) -> Result<bool, D1Error> {
        if !table
            .chars()
            .chain(column.chars())
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            return Err(D1Error {
                code: 2012,
                message: "invalid D1 schema identifier".to_string(),
            });
        }
        let rows: Vec<D1TableColumn> = self
            .query(&format!("PRAGMA table_info({table})"), None)
            .await?;
        Ok(rows.iter().any(|row| row.name == column))
    }

    async fn remote_table_exists(&self, table: &str) -> Result<bool, D1Error> {
        if !table
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            return Err(D1Error {
                code: 2012,
                message: "invalid D1 schema identifier".to_string(),
            });
        }
        let rows: Vec<D1ExistenceProbe> = self
            .query(
                "SELECT EXISTS(
                   SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1
                 ) AS present",
                Some(vec![serde_json::json!(table)]),
            )
            .await?;
        rows.first()
            .map(|row| row.present == 1)
            .ok_or_else(|| D1Error {
                code: 2011,
                message: "D1 schema probe returned no result row".to_string(),
            })
    }

    /// Read-only probe used by restore. Unlike
    /// [`Self::ensure_agent_capture_generation_table`], this never installs
    /// legacy-writer barriers or adopts rows.
    pub async fn agent_capture_generation_table_exists(&self) -> Result<bool, D1Error> {
        self.remote_table_exists("agent_capture_generation").await
    }

    /// PD-03: whether the remote carries the session-erasure tombstone
    /// table (absent on pre-PD-03 remotes; restore must stay read-only).
    pub async fn agent_import_tombstone_table_exists(&self) -> Result<bool, D1Error> {
        self.remote_table_exists("agent_import_tombstone").await
    }

    async fn agent_capture_v2_projection_is_ready(&self) -> Result<bool, D1Error> {
        if !self
            .remote_table_exists("agent_capture_schema_migration")
            .await?
        {
            return Ok(false);
        }
        let rows: Vec<D1ExistenceProbe> = self
            .query(
                "SELECT EXISTS(
                   SELECT 1 FROM agent_capture_schema_migration
                   WHERE version = 2 AND state = 'complete'
                 ) AS present",
                None,
            )
            .await?;
        rows.first()
            .map(|row| row.present == 1)
            .ok_or_else(|| D1Error {
                code: 2011,
                message: "agent-capture schema readiness probe returned no result row".to_string(),
            })
    }

    /// Return whether any legacy or v2 capture catalog row exists for this
    /// repository without creating or migrating remote schema.
    pub async fn agent_capture_catalog_has_rows(&self, repo_id: &str) -> Result<bool, D1Error> {
        for table in [
            "agent_session",
            "agent_checkpoint",
            "agent_capture_session_v2",
            "agent_capture_checkpoint_v2",
        ] {
            if !self.remote_table_exists(table).await? {
                continue;
            }
            let sql = format!("SELECT 1 AS present FROM {table} WHERE repo_id = ?1 LIMIT 1");
            let rows: Vec<D1ExistenceProbe> = self
                .query(&sql, Some(vec![serde_json::json!(repo_id)]))
                .await?;
            if !rows.is_empty() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Probe the three M5 companion tables as one schema unit. A partial
    /// remote shape is corruption and must not be treated as an empty layer.
    pub async fn agent_subagent_content_tables_exist(&self) -> Result<bool, D1Error> {
        let mut present = 0_usize;
        for table in [
            "agent_subagent_content_claim",
            "agent_subagent_content_revision",
            "agent_subagent_link",
        ] {
            present += usize::from(self.remote_table_exists(table).await?);
        }
        match present {
            0 => Ok(false),
            3 => Ok(true),
            _ => Err(D1Error {
                code: 2011,
                message: "remote subagent-content schema is incomplete".to_string(),
            }),
        }
    }

    async fn ensure_remote_column(
        &self,
        table: &str,
        column: &str,
        ddl: &str,
    ) -> Result<(), D1Error> {
        if self.remote_column_exists(table, column).await? {
            return Ok(());
        }
        if let Err(error) = self.execute(ddl, None).await
            && !self.remote_column_exists(table, column).await?
        {
            return Err(error);
        }
        Ok(())
    }

    /// Upsert one `agent_checkpoint` row keyed by `(repo_id, checkpoint_id)`.
    ///
    /// `synced_at` is stamped server-side via `strftime('%s', 'now')` for
    /// the same reason as [`Self::upsert_agent_session`].
    pub async fn upsert_agent_checkpoint(
        &self,
        repo_id: &str,
        row: &AgentCheckpointRow,
    ) -> Result<(), D1Error> {
        reject_unfenced_agent_capture_write("agent checkpoint")?;
        let sql = r#"
            INSERT INTO agent_capture_checkpoint_v2 (
                checkpoint_id, repo_id, session_id, parent_checkpoint_id, scope,
                parent_commit, tree_oid, metadata_blob_oid, traces_commit, tool_use_id,
                subagent_session_id, description, created_at, synced_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                CAST(strftime('%s', 'now') AS INTEGER)
            )
            ON CONFLICT(repo_id, checkpoint_id) DO UPDATE SET
                synced_at = CAST(strftime('%s', 'now') AS INTEGER)
            WHERE excluded.session_id IS agent_capture_checkpoint_v2.session_id
              AND excluded.parent_checkpoint_id IS agent_capture_checkpoint_v2.parent_checkpoint_id
              AND excluded.scope IS agent_capture_checkpoint_v2.scope
              AND excluded.parent_commit IS agent_capture_checkpoint_v2.parent_commit
              AND excluded.tree_oid IS agent_capture_checkpoint_v2.tree_oid
              AND excluded.metadata_blob_oid IS agent_capture_checkpoint_v2.metadata_blob_oid
              AND excluded.traces_commit IS agent_capture_checkpoint_v2.traces_commit
              AND excluded.tool_use_id IS agent_capture_checkpoint_v2.tool_use_id
              AND excluded.subagent_session_id IS agent_capture_checkpoint_v2.subagent_session_id
              AND excluded.description IS agent_capture_checkpoint_v2.description
              AND excluded.created_at IS agent_capture_checkpoint_v2.created_at
        "#;
        let params = vec![
            serde_json::json!(row.checkpoint_id),
            serde_json::json!(repo_id),
            serde_json::json!(row.session_id),
            serde_json::json!(row.parent_checkpoint_id),
            serde_json::json!(row.scope),
            serde_json::json!(row.parent_commit),
            serde_json::json!(row.tree_oid),
            serde_json::json!(row.metadata_blob_oid),
            serde_json::json!(row.traces_commit),
            serde_json::json!(row.tool_use_id),
            serde_json::json!(row.subagent_session_id),
            serde_json::json!(row.description),
            serde_json::json!(row.created_at),
        ];
        let result = self.execute(sql, Some(params)).await?;
        if result.meta.and_then(|meta| meta.changes) != Some(1) {
            return Err(D1Error {
                code: 3003,
                message: "agent checkpoint conflicts with immutable remote state".to_string(),
            });
        }
        Ok(())
    }

    async fn execute_agent_capture_json_batch<T: Serialize>(
        &self,
        sql: &str,
        repo_id: &str,
        publish_token: &str,
        rows: &[T],
        label: &str,
    ) -> Result<(), D1Error> {
        if rows.is_empty() {
            return Ok(());
        }
        let statement =
            Self::agent_capture_json_batch_statement(sql, repo_id, publish_token, rows, label)?;
        let result = self.execute(&statement.sql, statement.params).await?;
        let changes = result
            .meta
            .and_then(|meta| meta.changes)
            .ok_or_else(|| D1Error {
                code: 3002,
                message: format!("D1 returned no change count for {label} batch"),
            })?;
        let expected = i64::try_from(rows.len()).map_err(|error| D1Error {
            code: 2010,
            message: format!("{label} batch size cannot be represented: {error}"),
        })?;
        if changes != expected {
            return Err(D1Error {
                code: 3003,
                message: format!(
                    "{label} batch was fenced by newer or conflicting remote state ({changes}/{expected} rows applied)"
                ),
            });
        }
        Ok(())
    }

    fn agent_capture_json_batch_statement<T: Serialize>(
        sql: &str,
        repo_id: &str,
        publish_token: &str,
        rows: &[T],
        label: &str,
    ) -> Result<D1Statement, D1Error> {
        if rows.len() > AGENT_CAPTURE_PAGE_SIZE {
            return Err(D1Error {
                code: 2010,
                message: format!(
                    "{label} batch contains {} rows, exceeding the {}-row bound",
                    rows.len(),
                    AGENT_CAPTURE_PAGE_SIZE
                ),
            });
        }
        let payload = serde_json::to_string(rows).map_err(|error| D1Error {
            code: 2004,
            message: format!("Failed to encode {label} batch: {error}"),
        })?;
        Ok(D1Statement {
            sql: sql.to_string(),
            params: Some(vec![
                serde_json::json!(repo_id),
                serde_json::json!(payload),
                serde_json::json!(publish_token),
            ]),
        })
    }

    /// Publish a bounded session batch. `sync_revision` is the generation:
    /// an older clone cannot overwrite a newer remote row, and equal-generation
    /// divergence is rejected rather than silently choosing a winner.
    pub async fn sync_agent_sessions_batch(
        &self,
        repo_id: &str,
        publish_token: &str,
        rows: &[AgentSessionV2Row],
    ) -> Result<(), D1Error> {
        self.execute_agent_capture_json_batch(
            r#"
            WITH incoming(value) AS (SELECT value FROM json_each(?2))
            INSERT INTO agent_capture_session_v2 (
                session_id, repo_id, agent_kind, provider_session_id, state, working_dir,
                worktree_id, parent_commit, parent_session_id, metadata_json,
                redaction_report, started_at, last_event_at, stopped_at, schema_version,
                sync_revision, synced_at
            )
            SELECT
                json_extract(value, '$.session_id'), ?1,
                json_extract(value, '$.agent_kind'),
                json_extract(value, '$.provider_session_id'),
                json_extract(value, '$.state'), json_extract(value, '$.working_dir'),
                json_extract(value, '$.worktree_id'), json_extract(value, '$.parent_commit'),
                json_extract(value, '$.parent_session_id'),
                json_extract(value, '$.metadata_json'),
                json_extract(value, '$.redaction_report'),
                CAST(json_extract(value, '$.started_at') AS INTEGER),
                CAST(json_extract(value, '$.last_event_at') AS INTEGER),
                CAST(json_extract(value, '$.stopped_at') AS INTEGER),
                CAST(json_extract(value, '$.schema_version') AS INTEGER),
                CAST(json_extract(value, '$.sync_revision') AS INTEGER),
                CAST(strftime('%s', 'now') AS INTEGER)
            FROM incoming
            WHERE EXISTS (
                SELECT 1 FROM agent_capture_generation
                WHERE repo_id = ?1 AND state = 'publishing' AND writer_token = ?3
            )
            ON CONFLICT(repo_id, session_id) DO UPDATE SET
                agent_kind = excluded.agent_kind,
                provider_session_id = excluded.provider_session_id,
                state = excluded.state, working_dir = excluded.working_dir,
                worktree_id = excluded.worktree_id, parent_commit = excluded.parent_commit,
                parent_session_id = excluded.parent_session_id,
                metadata_json = excluded.metadata_json,
                redaction_report = excluded.redaction_report,
                started_at = excluded.started_at, last_event_at = excluded.last_event_at,
                stopped_at = excluded.stopped_at, schema_version = excluded.schema_version,
                sync_revision = excluded.sync_revision,
                synced_at = CAST(strftime('%s', 'now') AS INTEGER)
            WHERE excluded.sync_revision > agent_capture_session_v2.sync_revision
               OR (excluded.sync_revision = agent_capture_session_v2.sync_revision
                   AND excluded.agent_kind IS agent_capture_session_v2.agent_kind
                   AND excluded.provider_session_id IS agent_capture_session_v2.provider_session_id
                   AND excluded.state IS agent_capture_session_v2.state
                   AND excluded.working_dir IS agent_capture_session_v2.working_dir
                   AND excluded.worktree_id IS agent_capture_session_v2.worktree_id
                   AND excluded.parent_commit IS agent_capture_session_v2.parent_commit
                   AND excluded.parent_session_id IS agent_capture_session_v2.parent_session_id
                   AND excluded.metadata_json IS agent_capture_session_v2.metadata_json
                   AND excluded.redaction_report IS agent_capture_session_v2.redaction_report
                   AND excluded.started_at IS agent_capture_session_v2.started_at
                   AND excluded.last_event_at IS agent_capture_session_v2.last_event_at
                   AND excluded.stopped_at IS agent_capture_session_v2.stopped_at
                   AND excluded.schema_version IS agent_capture_session_v2.schema_version)
            "#,
            repo_id,
            publish_token,
            rows,
            "agent session",
        )
        .await
    }

    /// Publish immutable checkpoint identities in one bounded request.
    pub async fn sync_agent_checkpoints_batch(
        &self,
        repo_id: &str,
        publish_token: &str,
        rows: &[AgentCheckpointV2Row],
    ) -> Result<(), D1Error> {
        self.execute_agent_capture_json_batch(
            r#"
            WITH incoming(value) AS (SELECT value FROM json_each(?2))
            INSERT INTO agent_capture_checkpoint_v2 (
                checkpoint_id, repo_id, session_id, parent_checkpoint_id, scope,
                parent_commit, tree_oid, metadata_blob_oid, traces_commit, tool_use_id,
                subagent_session_id, description, created_at, sync_revision, synced_at
            )
            SELECT json_extract(value, '$.checkpoint_id'), ?1,
                json_extract(value, '$.session_id'),
                json_extract(value, '$.parent_checkpoint_id'),
                json_extract(value, '$.scope'), json_extract(value, '$.parent_commit'),
                json_extract(value, '$.tree_oid'), json_extract(value, '$.metadata_blob_oid'),
                json_extract(value, '$.traces_commit'), json_extract(value, '$.tool_use_id'),
                json_extract(value, '$.subagent_session_id'), json_extract(value, '$.description'),
                CAST(json_extract(value, '$.created_at') AS INTEGER),
                CAST(json_extract(value, '$.sync_revision') AS INTEGER),
                CAST(strftime('%s', 'now') AS INTEGER)
            FROM incoming
            WHERE EXISTS (
                SELECT 1 FROM agent_capture_generation
                WHERE repo_id = ?1 AND state = 'publishing' AND writer_token = ?3
            )
              AND NOT EXISTS (
                SELECT 1 FROM agent_checkpoint_prune_tombstone t
                WHERE t.repo_id = ?1
                  AND t.checkpoint_id = json_extract(value, '$.checkpoint_id')
              )
            ON CONFLICT(repo_id, checkpoint_id) DO UPDATE SET
                tree_oid = excluded.tree_oid,
                metadata_blob_oid = excluded.metadata_blob_oid,
                traces_commit = excluded.traces_commit,
                sync_revision = excluded.sync_revision,
                synced_at = CAST(strftime('%s', 'now') AS INTEGER)
            WHERE (excluded.session_id IS agent_capture_checkpoint_v2.session_id
              AND excluded.parent_checkpoint_id IS agent_capture_checkpoint_v2.parent_checkpoint_id
              AND excluded.scope IS agent_capture_checkpoint_v2.scope
              AND excluded.parent_commit IS agent_capture_checkpoint_v2.parent_commit
              AND excluded.tool_use_id IS agent_capture_checkpoint_v2.tool_use_id
              AND excluded.subagent_session_id IS agent_capture_checkpoint_v2.subagent_session_id
              AND excluded.description IS agent_capture_checkpoint_v2.description
              AND excluded.created_at IS agent_capture_checkpoint_v2.created_at
              AND excluded.sync_revision > agent_capture_checkpoint_v2.sync_revision)
               OR (excluded.sync_revision = agent_capture_checkpoint_v2.sync_revision
              AND excluded.session_id IS agent_capture_checkpoint_v2.session_id
              AND excluded.parent_checkpoint_id IS agent_capture_checkpoint_v2.parent_checkpoint_id
              AND excluded.scope IS agent_capture_checkpoint_v2.scope
              AND excluded.parent_commit IS agent_capture_checkpoint_v2.parent_commit
              AND excluded.tree_oid IS agent_capture_checkpoint_v2.tree_oid
              AND excluded.metadata_blob_oid IS agent_capture_checkpoint_v2.metadata_blob_oid
              AND excluded.traces_commit IS agent_capture_checkpoint_v2.traces_commit
              AND excluded.tool_use_id IS agent_capture_checkpoint_v2.tool_use_id
              AND excluded.subagent_session_id IS agent_capture_checkpoint_v2.subagent_session_id
              AND excluded.description IS agent_capture_checkpoint_v2.description
              AND excluded.created_at IS agent_capture_checkpoint_v2.created_at)
            "#,
            repo_id,
            publish_token,
            rows,
            "agent checkpoint",
        )
        .await
    }

    /// Publish durable ordinary-retention fences, then remove the fenced
    /// checkpoint and its dependent immutable companion rows. Session erasure
    /// never creates these rows, preserving the documented local-only erasure
    /// boundary.
    pub async fn sync_agent_checkpoint_prune_tombstones_batch(
        &self,
        repo_id: &str,
        publish_token: &str,
        rows: &[AgentCheckpointPruneTombstoneRow],
    ) -> Result<(), D1Error> {
        self.execute_agent_capture_json_batch(
            r#"
            WITH incoming(value) AS (SELECT value FROM json_each(?2))
            INSERT INTO agent_checkpoint_prune_tombstone (
                repo_id, checkpoint_id, session_id, pruned_at, synced_at
            )
            SELECT ?1, json_extract(value, '$.checkpoint_id'),
                   json_extract(value, '$.session_id'),
                   CAST(json_extract(value, '$.pruned_at') AS INTEGER),
                   CAST(strftime('%s', 'now') AS INTEGER)
            FROM incoming
            WHERE EXISTS (
                SELECT 1 FROM agent_capture_generation
                WHERE repo_id = ?1 AND state = 'publishing' AND writer_token = ?3
            )
            ON CONFLICT(repo_id, checkpoint_id) DO UPDATE SET
                session_id = excluded.session_id,
                pruned_at = MAX(agent_checkpoint_prune_tombstone.pruned_at, excluded.pruned_at),
                synced_at = CAST(strftime('%s', 'now') AS INTEGER)
            WHERE excluded.session_id IS agent_checkpoint_prune_tombstone.session_id
            "#,
            repo_id,
            publish_token,
            rows,
            "checkpoint prune tombstone",
        )
        .await?;

        // Every statement boundary remains valid under Publishing-mode
        // validation so an interrupted generation can be taken over. Remove
        // the mutable claim first, then its immutable revision, association,
        // and checkpoint. A later local claim batch recreates a rewound leaf;
        // pruning the final leaf intentionally leaves the claim absent.
        for sql in [
            "WITH incoming(value) AS (SELECT value FROM json_each(?2))
             DELETE FROM agent_subagent_content_claim
              WHERE repo_id = ?1
                AND current_checkpoint_id IN (
                    SELECT json_extract(value, '$.checkpoint_id') FROM incoming
                )
                AND EXISTS (
                    SELECT 1 FROM agent_capture_generation
                    WHERE repo_id = ?1 AND state = 'publishing' AND writer_token = ?3
                )",
            "WITH incoming(value) AS (SELECT value FROM json_each(?2))
             DELETE FROM agent_subagent_content_revision
              WHERE repo_id = ?1
                AND checkpoint_id IN (
                    SELECT json_extract(value, '$.checkpoint_id') FROM incoming
                )
                AND EXISTS (
                    SELECT 1 FROM agent_capture_generation
                    WHERE repo_id = ?1 AND state = 'publishing' AND writer_token = ?3
                )",
            "WITH incoming(value) AS (SELECT value FROM json_each(?2))
             DELETE FROM agent_subagent_link
              WHERE repo_id = ?1
                AND content_checkpoint_id IN (
                    SELECT json_extract(value, '$.checkpoint_id') FROM incoming
                )
                AND EXISTS (
                    SELECT 1 FROM agent_capture_generation
                    WHERE repo_id = ?1 AND state = 'publishing' AND writer_token = ?3
                )",
            "WITH incoming(value) AS (SELECT value FROM json_each(?2))
             DELETE FROM agent_capture_checkpoint_v2
              WHERE repo_id = ?1
                AND checkpoint_id IN (
                    SELECT json_extract(value, '$.checkpoint_id') FROM incoming
                )
                AND EXISTS (
                    SELECT 1 FROM agent_capture_generation
                    WHERE repo_id = ?1 AND state = 'publishing' AND writer_token = ?3
                )",
        ] {
            let statement = Self::agent_capture_json_batch_statement(
                sql,
                repo_id,
                publish_token,
                rows,
                "checkpoint prune cleanup",
            )?;
            self.execute(&statement.sql, statement.params).await?;
        }
        Ok(())
    }

    /// PD-03: publish session-erasure tombstones under the generation
    /// fence, then cascade-delete every mirror row of each erased
    /// session (claims -> revisions -> links -> checkpoints -> the
    /// session itself). Idempotent: replays keep the newest `erased_at`.
    pub async fn sync_agent_import_tombstones_batch(
        &self,
        repo_id: &str,
        publish_token: &str,
        rows: &[AgentImportTombstoneRow],
    ) -> Result<(), D1Error> {
        self.execute_agent_capture_json_batch(
            r#"
            WITH incoming(value) AS (SELECT value FROM json_each(?2))
            INSERT INTO agent_import_tombstone (
                repo_id, agent_kind, provider_session_id, erased_session_id,
                source_fingerprint, erased_at, synced_at
            )
            SELECT ?1, json_extract(value, '$.agent_kind'),
                   json_extract(value, '$.provider_session_id'),
                   json_extract(value, '$.erased_session_id'),
                   json_extract(value, '$.source_fingerprint'),
                   CAST(json_extract(value, '$.erased_at') AS INTEGER),
                   CAST(strftime('%s', 'now') AS INTEGER)
            FROM incoming
            WHERE EXISTS (
                SELECT 1 FROM agent_capture_generation
                WHERE repo_id = ?1 AND state = 'publishing' AND writer_token = ?3
            )
            ON CONFLICT(repo_id, agent_kind, provider_session_id) DO UPDATE SET
                erased_session_id = excluded.erased_session_id,
                source_fingerprint = COALESCE(
                    excluded.source_fingerprint,
                    agent_import_tombstone.source_fingerprint
                ),
                erased_at = MAX(agent_import_tombstone.erased_at, excluded.erased_at),
                synced_at = CAST(strftime('%s', 'now') AS INTEGER)
            "#,
            repo_id,
            publish_token,
            rows,
            "agent import tombstone",
        )
        .await?;

        // Cascade under the same fence. Every statement keeps the
        // Publishing-mode graph valid so an interrupted generation can be
        // taken over: mutable claims first, then immutable revisions and
        // associations, then checkpoints, then the session row.
        for sql in [
            "WITH incoming(value) AS (SELECT value FROM json_each(?2))
             DELETE FROM agent_subagent_content_claim
              WHERE repo_id = ?1
                AND parent_session_id IN (
                    SELECT json_extract(value, '$.erased_session_id') FROM incoming
                )
                AND EXISTS (
                    SELECT 1 FROM agent_capture_generation
                    WHERE repo_id = ?1 AND state = 'publishing' AND writer_token = ?3
                )",
            "WITH incoming(value) AS (SELECT value FROM json_each(?2))
             DELETE FROM agent_subagent_content_revision
              WHERE repo_id = ?1
                AND checkpoint_id IN (
                    SELECT checkpoint_id FROM agent_capture_checkpoint_v2
                    WHERE repo_id = ?1 AND session_id IN (
                        SELECT json_extract(value, '$.erased_session_id') FROM incoming
                    )
                )
                AND EXISTS (
                    SELECT 1 FROM agent_capture_generation
                    WHERE repo_id = ?1 AND state = 'publishing' AND writer_token = ?3
                )",
            "WITH incoming(value) AS (SELECT value FROM json_each(?2))
             DELETE FROM agent_subagent_link
              WHERE repo_id = ?1
                AND content_checkpoint_id IN (
                    SELECT checkpoint_id FROM agent_capture_checkpoint_v2
                    WHERE repo_id = ?1 AND session_id IN (
                        SELECT json_extract(value, '$.erased_session_id') FROM incoming
                    )
                )
                AND EXISTS (
                    SELECT 1 FROM agent_capture_generation
                    WHERE repo_id = ?1 AND state = 'publishing' AND writer_token = ?3
                )",
            "WITH incoming(value) AS (SELECT value FROM json_each(?2))
             DELETE FROM agent_capture_checkpoint_v2
              WHERE repo_id = ?1
                AND session_id IN (
                    SELECT json_extract(value, '$.erased_session_id') FROM incoming
                )
                AND EXISTS (
                    SELECT 1 FROM agent_capture_generation
                    WHERE repo_id = ?1 AND state = 'publishing' AND writer_token = ?3
                )",
            "WITH incoming(value) AS (SELECT value FROM json_each(?2))
             DELETE FROM agent_capture_session_v2
              WHERE repo_id = ?1
                AND session_id IN (
                    SELECT json_extract(value, '$.erased_session_id') FROM incoming
                )
                AND EXISTS (
                    SELECT 1 FROM agent_capture_generation
                    WHERE repo_id = ?1 AND state = 'publishing' AND writer_token = ?3
                )",
        ] {
            let statement = Self::agent_capture_json_batch_statement(
                sql,
                repo_id,
                publish_token,
                rows,
                "agent import tombstone cascade",
            )?;
            // Cascade deletes remove however many mirror rows exist (often
            // zero on a replay) — no per-row change-count expectation.
            self.execute(&statement.sql, statement.params).await?;
        }
        Ok(())
    }

    pub async fn ensure_agent_subagent_content_tables(&self) -> Result<(), D1Error> {
        self.execute(
            r#"
            CREATE TABLE IF NOT EXISTS agent_subagent_content_claim (
                repo_id TEXT NOT NULL,
                parent_session_id TEXT NOT NULL,
                provider_kind TEXT NOT NULL,
                source_key TEXT NOT NULL,
                content_schema_version INTEGER NOT NULL,
                revision_cursor INTEGER NOT NULL,
                sync_revision INTEGER NOT NULL DEFAULT 1,
                current_revision INTEGER NOT NULL,
                current_checkpoint_id TEXT,
                current_digest TEXT,
                fence_token INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                synced_at INTEGER NOT NULL,
                PRIMARY KEY (
                    repo_id, parent_session_id, provider_kind, source_key,
                    content_schema_version
                )
            )
            "#,
            None,
        )
        .await?;
        self.ensure_remote_column(
            "agent_subagent_content_claim",
            "sync_revision",
            "ALTER TABLE agent_subagent_content_claim ADD COLUMN sync_revision INTEGER NOT NULL DEFAULT 1",
        )
        .await?;
        self.execute(
            r#"
            CREATE TABLE IF NOT EXISTS agent_subagent_content_revision (
                repo_id TEXT NOT NULL,
                parent_session_id TEXT NOT NULL,
                provider_kind TEXT NOT NULL,
                source_key TEXT NOT NULL,
                content_schema_version INTEGER NOT NULL,
                revision INTEGER NOT NULL,
                checkpoint_id TEXT NOT NULL,
                content_digest TEXT NOT NULL,
                source_channel TEXT NOT NULL,
                partial INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                synced_at INTEGER NOT NULL,
                PRIMARY KEY (
                    repo_id, parent_session_id, provider_kind, source_key,
                    content_schema_version, revision
                ),
                UNIQUE(repo_id, checkpoint_id)
            )
            "#,
            None,
        )
        .await?;
        self.execute(
            r#"
            CREATE TABLE IF NOT EXISTS agent_subagent_link (
                repo_id TEXT NOT NULL,
                content_checkpoint_id TEXT NOT NULL,
                parent_session_id TEXT NOT NULL,
                link_state TEXT NOT NULL,
                boundary_checkpoint_id TEXT,
                stable_subagent_id TEXT,
                sync_revision INTEGER NOT NULL DEFAULT 1,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                synced_at INTEGER NOT NULL,
                PRIMARY KEY (repo_id, content_checkpoint_id)
            )
            "#,
            None,
        )
        .await?;
        self.ensure_remote_column(
            "agent_subagent_link",
            "sync_revision",
            "ALTER TABLE agent_subagent_link ADD COLUMN sync_revision INTEGER NOT NULL DEFAULT 1",
        )
        .await?;
        Ok(())
    }

    /// Create the durable M5 subagent-content companion tables in D1.
    /// Transient reservation owner/lease/attempt fields are intentionally not
    /// mirrored; restore always reconstructs an idle claim with the same
    /// monotonic revision/fence high-water marks.
    pub async fn upsert_agent_subagent_content_claim(
        &self,
        repo_id: &str,
        row: &AgentSubagentContentClaimRow,
    ) -> Result<(), D1Error> {
        reject_unfenced_agent_capture_write("subagent content claim")?;
        let sql = r#"
            INSERT INTO agent_subagent_content_claim (
                repo_id, parent_session_id, provider_kind, source_key,
                content_schema_version, revision_cursor, sync_revision, current_revision,
                current_checkpoint_id, current_digest, fence_token, created_at,
                updated_at, synced_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                      CAST(strftime('%s', 'now') AS INTEGER))
            ON CONFLICT(repo_id, parent_session_id, provider_kind, source_key,
                        content_schema_version) DO UPDATE SET
                revision_cursor = excluded.revision_cursor,
                sync_revision = excluded.sync_revision,
                current_revision = excluded.current_revision,
                current_checkpoint_id = excluded.current_checkpoint_id,
                current_digest = excluded.current_digest,
                fence_token = MAX(agent_subagent_content_claim.fence_token, excluded.fence_token),
                created_at = excluded.created_at,
                updated_at = MAX(agent_subagent_content_claim.updated_at, excluded.updated_at),
                synced_at = CAST(strftime('%s', 'now') AS INTEGER)
            WHERE excluded.sync_revision > agent_subagent_content_claim.sync_revision
               OR (excluded.sync_revision = agent_subagent_content_claim.sync_revision
                   AND excluded.revision_cursor IS agent_subagent_content_claim.revision_cursor
                   AND excluded.current_revision IS agent_subagent_content_claim.current_revision
                   AND excluded.current_checkpoint_id IS agent_subagent_content_claim.current_checkpoint_id
                   AND excluded.current_digest IS agent_subagent_content_claim.current_digest)
        "#;
        let result = self
            .execute(
                sql,
                Some(vec![
                    serde_json::json!(repo_id),
                    serde_json::json!(row.parent_session_id),
                    serde_json::json!(row.provider_kind),
                    serde_json::json!(row.source_key),
                    serde_json::json!(row.content_schema_version),
                    serde_json::json!(row.revision_cursor),
                    serde_json::json!(row.sync_revision),
                    serde_json::json!(row.current_revision),
                    serde_json::json!(row.current_checkpoint_id),
                    serde_json::json!(row.current_digest),
                    serde_json::json!(row.fence_token),
                    serde_json::json!(row.created_at),
                    serde_json::json!(row.updated_at),
                ]),
            )
            .await?;
        if result.meta.and_then(|meta| meta.changes) != Some(1) {
            return Err(D1Error {
                code: 3003,
                message: "subagent claim was fenced by newer or conflicting remote state"
                    .to_string(),
            });
        }
        Ok(())
    }

    pub async fn upsert_agent_subagent_content_revision(
        &self,
        repo_id: &str,
        row: &AgentSubagentContentRevisionRow,
    ) -> Result<(), D1Error> {
        reject_unfenced_agent_capture_write("subagent content revision")?;
        let sql = r#"
            INSERT INTO agent_subagent_content_revision (
                repo_id, parent_session_id, provider_kind, source_key,
                content_schema_version, revision, checkpoint_id, content_digest,
                source_channel, partial, created_at, synced_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                      CAST(strftime('%s', 'now') AS INTEGER))
            ON CONFLICT(repo_id, parent_session_id, provider_kind, source_key,
                        content_schema_version, revision) DO UPDATE SET
                synced_at = CAST(strftime('%s', 'now') AS INTEGER)
            WHERE excluded.checkpoint_id IS agent_subagent_content_revision.checkpoint_id
              AND excluded.content_digest IS agent_subagent_content_revision.content_digest
              AND excluded.source_channel IS agent_subagent_content_revision.source_channel
              AND excluded.partial IS agent_subagent_content_revision.partial
              AND excluded.created_at IS agent_subagent_content_revision.created_at
        "#;
        let result = self
            .execute(
                sql,
                Some(vec![
                    serde_json::json!(repo_id),
                    serde_json::json!(row.parent_session_id),
                    serde_json::json!(row.provider_kind),
                    serde_json::json!(row.source_key),
                    serde_json::json!(row.content_schema_version),
                    serde_json::json!(row.revision),
                    serde_json::json!(row.checkpoint_id),
                    serde_json::json!(row.content_digest),
                    serde_json::json!(row.source_channel),
                    serde_json::json!(row.partial),
                    serde_json::json!(row.created_at),
                ]),
            )
            .await?;
        if result.meta.and_then(|meta| meta.changes) != Some(1) {
            return Err(D1Error {
                code: 3003,
                message: "subagent revision conflicts with immutable remote state".to_string(),
            });
        }
        Ok(())
    }

    pub async fn upsert_agent_subagent_link(
        &self,
        repo_id: &str,
        row: &AgentSubagentLinkRow,
    ) -> Result<(), D1Error> {
        reject_unfenced_agent_capture_write("subagent link")?;
        let sql = r#"
            INSERT INTO agent_subagent_link (
                repo_id, content_checkpoint_id, parent_session_id, link_state,
                boundary_checkpoint_id, stable_subagent_id, sync_revision, created_at, updated_at,
                synced_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                      CAST(strftime('%s', 'now') AS INTEGER))
            ON CONFLICT(repo_id, content_checkpoint_id) DO UPDATE SET
                parent_session_id = excluded.parent_session_id,
                link_state = excluded.link_state,
                boundary_checkpoint_id = excluded.boundary_checkpoint_id,
                stable_subagent_id = excluded.stable_subagent_id,
                sync_revision = excluded.sync_revision,
                created_at = excluded.created_at,
                updated_at = excluded.updated_at,
                synced_at = CAST(strftime('%s', 'now') AS INTEGER)
            WHERE excluded.sync_revision > agent_subagent_link.sync_revision
               OR (excluded.sync_revision = agent_subagent_link.sync_revision
                   AND excluded.parent_session_id IS agent_subagent_link.parent_session_id
                   AND excluded.link_state IS agent_subagent_link.link_state
                   AND excluded.boundary_checkpoint_id IS agent_subagent_link.boundary_checkpoint_id
                   AND excluded.stable_subagent_id IS agent_subagent_link.stable_subagent_id
                   AND excluded.created_at IS agent_subagent_link.created_at
                   AND excluded.updated_at IS agent_subagent_link.updated_at)
        "#;
        let result = self
            .execute(
                sql,
                Some(vec![
                    serde_json::json!(repo_id),
                    serde_json::json!(row.content_checkpoint_id),
                    serde_json::json!(row.parent_session_id),
                    serde_json::json!(row.link_state),
                    serde_json::json!(row.boundary_checkpoint_id),
                    serde_json::json!(row.stable_subagent_id),
                    serde_json::json!(row.sync_revision),
                    serde_json::json!(row.created_at),
                    serde_json::json!(row.updated_at),
                ]),
            )
            .await?;
        if result.meta.and_then(|meta| meta.changes) != Some(1) {
            return Err(D1Error {
                code: 3003,
                message: "subagent link was fenced by newer or conflicting remote state"
                    .to_string(),
            });
        }
        Ok(())
    }

    /// Append immutable source revisions. A same-key row may only be touched
    /// when every immutable field matches, making concurrent divergent clones
    /// fail closed.
    pub async fn sync_agent_subagent_revisions_batch(
        &self,
        repo_id: &str,
        publish_token: &str,
        rows: &[AgentSubagentContentRevisionRow],
    ) -> Result<(), D1Error> {
        self.execute_agent_capture_json_batch(
            r#"
            WITH incoming(value) AS (SELECT value FROM json_each(?2))
            INSERT INTO agent_subagent_content_revision (
                repo_id, parent_session_id, provider_kind, source_key,
                content_schema_version, revision, checkpoint_id, content_digest,
                source_channel, partial, created_at, synced_at
            )
            SELECT ?1, json_extract(value, '$.parent_session_id'),
                json_extract(value, '$.provider_kind'), json_extract(value, '$.source_key'),
                CAST(json_extract(value, '$.content_schema_version') AS INTEGER),
                CAST(json_extract(value, '$.revision') AS INTEGER),
                json_extract(value, '$.checkpoint_id'), json_extract(value, '$.content_digest'),
                json_extract(value, '$.source_channel'),
                CAST(json_extract(value, '$.partial') AS INTEGER),
                CAST(json_extract(value, '$.created_at') AS INTEGER),
                CAST(strftime('%s', 'now') AS INTEGER)
            FROM incoming
            WHERE EXISTS (
                SELECT 1 FROM agent_capture_generation
                WHERE repo_id = ?1 AND state = 'publishing' AND writer_token = ?3
            )
              AND NOT EXISTS (
                SELECT 1 FROM agent_checkpoint_prune_tombstone t
                WHERE t.repo_id = ?1
                  AND t.checkpoint_id = json_extract(value, '$.checkpoint_id')
              )
            ON CONFLICT(repo_id, parent_session_id, provider_kind, source_key,
                        content_schema_version, revision) DO UPDATE SET
                synced_at = CAST(strftime('%s', 'now') AS INTEGER)
            WHERE excluded.checkpoint_id IS agent_subagent_content_revision.checkpoint_id
              AND excluded.content_digest IS agent_subagent_content_revision.content_digest
              AND excluded.source_channel IS agent_subagent_content_revision.source_channel
              AND excluded.partial IS agent_subagent_content_revision.partial
              AND excluded.created_at IS agent_subagent_content_revision.created_at
            "#,
            repo_id,
            publish_token,
            rows,
            "subagent content revision",
        )
        .await
    }

    /// Publish association links by monotonic `sync_revision`; equal-generation
    /// divergence is rejected and an older clone cannot undo a newer boundary
    /// resolution/deletion observation.
    pub async fn sync_agent_subagent_links_batch(
        &self,
        repo_id: &str,
        publish_token: &str,
        rows: &[AgentSubagentLinkRow],
    ) -> Result<(), D1Error> {
        self.execute_agent_capture_json_batch(
            r#"
            WITH incoming(value) AS (SELECT value FROM json_each(?2))
            INSERT INTO agent_subagent_link (
                repo_id, content_checkpoint_id, parent_session_id, link_state,
                boundary_checkpoint_id, stable_subagent_id, sync_revision, created_at, updated_at,
                synced_at
            )
            SELECT ?1, json_extract(value, '$.content_checkpoint_id'),
                json_extract(value, '$.parent_session_id'), json_extract(value, '$.link_state'),
                json_extract(value, '$.boundary_checkpoint_id'),
                json_extract(value, '$.stable_subagent_id'),
                CAST(json_extract(value, '$.sync_revision') AS INTEGER),
                CAST(json_extract(value, '$.created_at') AS INTEGER),
                CAST(json_extract(value, '$.updated_at') AS INTEGER),
                CAST(strftime('%s', 'now') AS INTEGER)
            FROM incoming
            WHERE EXISTS (
                SELECT 1 FROM agent_capture_generation
                WHERE repo_id = ?1 AND state = 'publishing' AND writer_token = ?3
            )
              AND NOT EXISTS (
                SELECT 1 FROM agent_checkpoint_prune_tombstone t
                WHERE t.repo_id = ?1
                  AND t.checkpoint_id = json_extract(value, '$.content_checkpoint_id')
              )
            ON CONFLICT(repo_id, content_checkpoint_id) DO UPDATE SET
                parent_session_id = excluded.parent_session_id,
                link_state = excluded.link_state,
                boundary_checkpoint_id = excluded.boundary_checkpoint_id,
                stable_subagent_id = excluded.stable_subagent_id,
                sync_revision = excluded.sync_revision,
                created_at = excluded.created_at,
                updated_at = excluded.updated_at,
                synced_at = CAST(strftime('%s', 'now') AS INTEGER)
            WHERE excluded.sync_revision > agent_subagent_link.sync_revision
               OR (excluded.sync_revision = agent_subagent_link.sync_revision
                   AND excluded.parent_session_id IS agent_subagent_link.parent_session_id
                   AND excluded.link_state IS agent_subagent_link.link_state
                   AND excluded.boundary_checkpoint_id IS agent_subagent_link.boundary_checkpoint_id
                   AND excluded.stable_subagent_id IS agent_subagent_link.stable_subagent_id
                   AND excluded.created_at IS agent_subagent_link.created_at
                   AND excluded.updated_at IS agent_subagent_link.updated_at)
            "#,
            repo_id,
            publish_token,
            rows,
            "subagent association link",
        )
        .await
    }

    /// Publish source claims last. The D1-side predicate verifies that the
    /// advertised current revision, checkpoint, digest, and link already
    /// exist, then advances only the monotonic revision cursor. Fence tokens
    /// are merged by maximum for an otherwise identical generation.
    pub async fn sync_agent_subagent_claims_batch(
        &self,
        repo_id: &str,
        publish_token: &str,
        rows: &[AgentSubagentContentClaimRow],
    ) -> Result<(), D1Error> {
        let sql = format!(
            r#"
            WITH incoming(value) AS (SELECT value FROM json_each(?2))
            INSERT INTO agent_subagent_content_claim (
                repo_id, parent_session_id, provider_kind, source_key,
                content_schema_version, revision_cursor, sync_revision, current_revision,
                current_checkpoint_id, current_digest, fence_token, created_at,
                updated_at, synced_at
            )
            SELECT ?1, json_extract(value, '$.parent_session_id'),
                json_extract(value, '$.provider_kind'), json_extract(value, '$.source_key'),
                CAST(json_extract(value, '$.content_schema_version') AS INTEGER),
                CAST(json_extract(value, '$.revision_cursor') AS INTEGER),
                CAST(json_extract(value, '$.sync_revision') AS INTEGER),
                CAST(json_extract(value, '$.current_revision') AS INTEGER),
                json_extract(value, '$.current_checkpoint_id'),
                json_extract(value, '$.current_digest'),
                CAST(json_extract(value, '$.fence_token') AS INTEGER),
                CAST(json_extract(value, '$.created_at') AS INTEGER),
                CAST(json_extract(value, '$.updated_at') AS INTEGER),
                CAST(strftime('%s', 'now') AS INTEGER)
            FROM incoming
            WHERE EXISTS (
                    SELECT 1 FROM agent_capture_generation
                    WHERE repo_id = ?1 AND state = 'publishing' AND writer_token = ?3
                  )
              AND (CAST(json_extract(value, '$.current_revision') AS INTEGER) = 0
               OR (EXISTS (
                    SELECT 1 FROM agent_subagent_content_revision r
                    WHERE r.repo_id = ?1
                      AND r.parent_session_id = json_extract(value, '$.parent_session_id')
                      AND r.provider_kind = json_extract(value, '$.provider_kind')
                      AND r.source_key = json_extract(value, '$.source_key')
                      AND r.content_schema_version = CAST(json_extract(value, '$.content_schema_version') AS INTEGER)
                      AND r.revision = CAST(json_extract(value, '$.current_revision') AS INTEGER)
                      AND r.checkpoint_id = json_extract(value, '$.current_checkpoint_id')
                      AND r.content_digest = json_extract(value, '$.current_digest')
                  ) AND EXISTS (
                    SELECT 1 FROM agent_capture_checkpoint_v2 c
                    WHERE c.repo_id = ?1
                      AND c.checkpoint_id = json_extract(value, '$.current_checkpoint_id')
                  ) AND EXISTS (
                    SELECT 1 FROM agent_subagent_link l
                    WHERE l.repo_id = ?1
                      AND l.content_checkpoint_id = json_extract(value, '$.current_checkpoint_id')
                      AND l.parent_session_id = json_extract(value, '$.parent_session_id')
                  )))
            ON CONFLICT(repo_id, parent_session_id, provider_kind, source_key,
                        content_schema_version) DO UPDATE SET
                revision_cursor = excluded.revision_cursor,
                sync_revision = excluded.sync_revision,
                current_revision = excluded.current_revision,
                current_checkpoint_id = excluded.current_checkpoint_id,
                current_digest = excluded.current_digest,
                fence_token = MAX(agent_subagent_content_claim.fence_token, excluded.fence_token),
                created_at = excluded.created_at,
                updated_at = MAX(agent_subagent_content_claim.updated_at, excluded.updated_at),
                synced_at = CAST(strftime('%s', 'now') AS INTEGER)
            WHERE {AGENT_SUBAGENT_CLAIM_UPDATE_GUARD}
            "#
        );
        self.execute_agent_capture_json_batch(
            &sql,
            repo_id,
            publish_token,
            rows,
            "subagent content claim",
        )
        .await
    }

    pub async fn list_agent_subagent_content_claims(
        &self,
        repo_id: &str,
    ) -> Result<Vec<AgentSubagentContentClaimRow>, D1Error> {
        let mut remaining_rows = AGENT_CAPTURE_MAX_ROWS_PER_TABLE;
        self.list_agent_subagent_content_claims_with_budget(repo_id, &mut remaining_rows)
            .await
    }

    async fn list_agent_subagent_content_claims_with_budget(
        &self,
        repo_id: &str,
        remaining_rows: &mut usize,
    ) -> Result<Vec<AgentSubagentContentClaimRow>, D1Error> {
        self.collect_agent_capture_pages_with_budget(
            "SELECT parent_session_id, provider_kind, source_key, content_schema_version,
                    revision_cursor, sync_revision, current_revision, current_checkpoint_id,
                    current_digest, fence_token, created_at, updated_at
             FROM agent_subagent_content_claim WHERE repo_id = ?1
             ORDER BY parent_session_id, provider_kind, source_key, content_schema_version
             LIMIT ?2 OFFSET ?3",
            repo_id,
            "subagent content claim",
            remaining_rows,
        )
        .await
    }

    pub async fn list_agent_subagent_content_revisions(
        &self,
        repo_id: &str,
    ) -> Result<Vec<AgentSubagentContentRevisionRow>, D1Error> {
        let mut remaining_rows = AGENT_CAPTURE_MAX_ROWS_PER_TABLE;
        self.list_agent_subagent_content_revisions_with_budget(repo_id, &mut remaining_rows)
            .await
    }

    async fn list_agent_subagent_content_revisions_with_budget(
        &self,
        repo_id: &str,
        remaining_rows: &mut usize,
    ) -> Result<Vec<AgentSubagentContentRevisionRow>, D1Error> {
        self.collect_agent_capture_pages_with_budget(
            "SELECT parent_session_id, provider_kind, source_key, content_schema_version,
                    revision, checkpoint_id, content_digest, source_channel, partial,
                    created_at
             FROM agent_subagent_content_revision WHERE repo_id = ?1
             ORDER BY parent_session_id, provider_kind, source_key,
                      content_schema_version, revision
             LIMIT ?2 OFFSET ?3",
            repo_id,
            "subagent content revision",
            remaining_rows,
        )
        .await
    }

    pub async fn list_agent_subagent_links(
        &self,
        repo_id: &str,
    ) -> Result<Vec<AgentSubagentLinkRow>, D1Error> {
        let mut remaining_rows = AGENT_CAPTURE_MAX_ROWS_PER_TABLE;
        self.list_agent_subagent_links_with_budget(repo_id, &mut remaining_rows)
            .await
    }

    async fn list_agent_subagent_links_with_budget(
        &self,
        repo_id: &str,
        remaining_rows: &mut usize,
    ) -> Result<Vec<AgentSubagentLinkRow>, D1Error> {
        self.collect_agent_capture_pages_with_budget(
            "SELECT content_checkpoint_id, parent_session_id, link_state,
                    boundary_checkpoint_id, stable_subagent_id, sync_revision,
                    created_at, updated_at
             FROM agent_subagent_link WHERE repo_id = ?1
             ORDER BY created_at, content_checkpoint_id
             LIMIT ?2 OFFSET ?3",
            repo_id,
            "subagent association link",
            remaining_rows,
        )
        .await
    }

    // ─────────────────────────────────────────────────────────────
    // Phase 2 (publish.md) — D1 publish schema + upsert/list.
    //
    // The publish schema source-of-truth lives at
    // `sql/publish/0001_publish.sql` + later migrations under
    // `sql/publish/`. `ensure_publish_schema` reads every `*.sql`
    // file via `include_str!` and applies them in numeric order;
    // each statement is run individually because D1's REST `execute`
    // does not accept multi-statement payloads.
    //
    // Upsert/list helpers below are the typed access surface the
    // CLI snapshot builder (Phase 3) and the publish CLI (Phase 4)
    // call into. They never `println!`/`eprintln!` — the caller
    // owns user-facing output.
    // ─────────────────────────────────────────────────────────────

    /// Apply every publish schema migration in `sql/publish/` to the
    /// remote D1 database. Idempotent — every migration uses
    /// `CREATE TABLE IF NOT EXISTS` / `CREATE TRIGGER IF NOT
    /// EXISTS` / `DROP TRIGGER IF EXISTS` so repeat calls converge
    /// to the same state. Phase 6+7 reviewers (passes 6 + 11)
    /// pinned the migration chain via the
    /// `publish_schema_contract_worker_mirror_is_byte_equal` test;
    /// the strings below MUST stay byte-equal mirrors of the on-
    /// disk SQL files, which is enforced by `include_str!`.
    pub async fn ensure_publish_schema(&self) -> Result<(), D1Error> {
        // Order matches numeric migration prefix.
        let migrations: &[(&str, &str)] = &[
            (
                "0001_publish.sql",
                include_str!("../../sql/publish/0001_publish.sql"),
            ),
            (
                "0002_publish_digest_check.sql",
                include_str!("../../sql/publish/0002_publish_digest_check.sql"),
            ),
            (
                "0003_publish_max_preview_trigger_replace.sql",
                include_str!("../../sql/publish/0003_publish_max_preview_trigger_replace.sql"),
            ),
            (
                "0004_publish_refs_index.sql",
                include_str!("../../sql/publish/0004_publish_refs_index.sql"),
            ),
        ];
        for (label, sql) in migrations {
            for statement in split_sql_statements(sql) {
                self.execute(&statement, None).await.map_err(|e| D1Error {
                    code: e.code,
                    message: format!(
                        "publish migration {label} failed at statement {statement:?}: {}",
                        e.message
                    ),
                })?;
            }
        }
        Ok(())
    }

    /// Insert or update a `publish_sites` row.
    ///
    /// `default_ref` and `latest_revision_oid` may be NULL on first
    /// insert (the chicken-and-egg insert order described in
    /// `sql/publish/0001_publish.sql`); update them in a follow-up
    /// call once the refs/revisions exist.
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_publish_site(&self, row: &PublishSiteRow) -> Result<(), D1Error> {
        let sql = r#"
            INSERT INTO publish_sites (
                site_id, repo_id, clone_domain, slug, display_origin,
                name, visibility, status, worker_name, default_ref,
                latest_revision_oid, refs_generation, max_preview_bytes,
                schema_version, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
            ON CONFLICT(site_id) DO UPDATE SET
                repo_id = excluded.repo_id,
                clone_domain = excluded.clone_domain,
                slug = excluded.slug,
                display_origin = excluded.display_origin,
                name = excluded.name,
                visibility = excluded.visibility,
                status = excluded.status,
                worker_name = excluded.worker_name,
                default_ref = excluded.default_ref,
                latest_revision_oid = excluded.latest_revision_oid,
                refs_generation = excluded.refs_generation,
                max_preview_bytes = excluded.max_preview_bytes,
                schema_version = excluded.schema_version,
                updated_at = excluded.updated_at
        "#;
        let params = vec![
            serde_json::json!(row.site_id),
            serde_json::json!(row.repo_id),
            serde_json::json!(row.clone_domain),
            serde_json::json!(row.slug),
            serde_json::json!(row.display_origin),
            serde_json::json!(row.name),
            serde_json::json!(row.visibility),
            serde_json::json!(row.status),
            serde_json::json!(row.worker_name),
            serde_json::json!(row.default_ref),
            serde_json::json!(row.latest_revision_oid),
            serde_json::json!(row.refs_generation),
            serde_json::json!(row.max_preview_bytes),
            serde_json::json!(row.schema_version),
            serde_json::json!(row.created_at),
            serde_json::json!(row.updated_at),
        ];
        self.execute(sql, Some(params)).await?;
        Ok(())
    }

    /// CAS-update a site's latest/default publish pointers.
    ///
    /// Without `force`, the update only applies when the stored
    /// `refs_generation` matches `expected_refs_generation`; a
    /// zero-row update is reported as [`PublishSiteLatestUpdateResult::Conflict`]
    /// so callers can surface a clear retry-or-`--force` error. With
    /// `force`, the generation guard is intentionally omitted.
    pub async fn update_publish_site_latest(
        &self,
        update: PublishSiteLatestUpdate<'_>,
    ) -> Result<PublishSiteLatestUpdateResult, D1Error> {
        let D1Statement { sql, params } = publish_site_latest_update_statement(&update);
        let result = self.execute(&sql, params).await?;
        Ok(publish_site_latest_update_result_from_changes(
            result.meta.and_then(|meta| meta.changes).unwrap_or(0),
        ))
    }

    /// Insert or update a `publish_sync_runs` row.
    pub async fn upsert_publish_sync_run(&self, row: &PublishSyncRunRow) -> Result<(), D1Error> {
        let sql = r#"
            INSERT INTO publish_sync_runs (
                sync_run_id, site_id, status, started_at, finished_at,
                refs_count, revision_count, file_count, ai_object_count,
                ai_bundle_count, warnings_json, error_message,
                cli_version, schema_version
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
            ON CONFLICT(sync_run_id) DO UPDATE SET
                status = excluded.status,
                finished_at = excluded.finished_at,
                refs_count = excluded.refs_count,
                revision_count = excluded.revision_count,
                file_count = excluded.file_count,
                ai_object_count = excluded.ai_object_count,
                ai_bundle_count = excluded.ai_bundle_count,
                warnings_json = excluded.warnings_json,
                error_message = excluded.error_message,
                schema_version = excluded.schema_version
        "#;
        let params = vec![
            serde_json::json!(row.sync_run_id),
            serde_json::json!(row.site_id),
            serde_json::json!(row.status),
            serde_json::json!(row.started_at),
            serde_json::json!(row.finished_at),
            serde_json::json!(row.refs_count),
            serde_json::json!(row.revision_count),
            serde_json::json!(row.file_count),
            serde_json::json!(row.ai_object_count),
            serde_json::json!(row.ai_bundle_count),
            serde_json::json!(row.warnings_json),
            serde_json::json!(row.error_message),
            serde_json::json!(row.cli_version),
            serde_json::json!(row.schema_version),
        ];
        self.execute(sql, Some(params)).await?;
        Ok(())
    }

    /// Insert or update a `publish_revisions` row.
    pub async fn upsert_publish_revision(&self, row: &PublishRevisionRow) -> Result<(), D1Error> {
        let sql = r#"
            INSERT INTO publish_revisions (
                site_id, revision_oid, status, code_manifest_key, ai_index_key,
                file_count, ai_object_count, ai_bundle_count, redaction_mode,
                redaction_rules_version, sync_run_id, schema_version,
                created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
            ON CONFLICT(site_id, revision_oid) DO UPDATE SET
                status = excluded.status,
                code_manifest_key = excluded.code_manifest_key,
                ai_index_key = excluded.ai_index_key,
                file_count = excluded.file_count,
                ai_object_count = excluded.ai_object_count,
                ai_bundle_count = excluded.ai_bundle_count,
                redaction_mode = excluded.redaction_mode,
                redaction_rules_version = excluded.redaction_rules_version,
                sync_run_id = excluded.sync_run_id,
                schema_version = excluded.schema_version,
                updated_at = excluded.updated_at
        "#;
        let params = vec![
            serde_json::json!(row.site_id),
            serde_json::json!(row.revision_oid),
            serde_json::json!(row.status),
            serde_json::json!(row.code_manifest_key),
            serde_json::json!(row.ai_index_key),
            serde_json::json!(row.file_count),
            serde_json::json!(row.ai_object_count),
            serde_json::json!(row.ai_bundle_count),
            serde_json::json!(row.redaction_mode),
            serde_json::json!(row.redaction_rules_version),
            serde_json::json!(row.sync_run_id),
            serde_json::json!(row.schema_version),
            serde_json::json!(row.created_at),
            serde_json::json!(row.updated_at),
        ];
        self.execute(sql, Some(params)).await?;
        Ok(())
    }

    /// Insert or update a `publish_refs` row.
    pub async fn upsert_publish_ref(&self, row: &PublishRefRow) -> Result<(), D1Error> {
        let sql = r#"
            INSERT INTO publish_refs (
                site_id, ref_name, ref_type, short_name, target_oid,
                revision_oid, is_default, sync_run_id, schema_version, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(site_id, ref_name) DO UPDATE SET
                ref_type = excluded.ref_type,
                short_name = excluded.short_name,
                target_oid = excluded.target_oid,
                revision_oid = excluded.revision_oid,
                is_default = excluded.is_default,
                sync_run_id = excluded.sync_run_id,
                schema_version = excluded.schema_version,
                updated_at = excluded.updated_at
        "#;
        let params = vec![
            serde_json::json!(row.site_id),
            serde_json::json!(row.ref_name),
            serde_json::json!(row.ref_type),
            serde_json::json!(row.short_name),
            serde_json::json!(row.target_oid),
            serde_json::json!(row.revision_oid),
            serde_json::json!(row.is_default),
            serde_json::json!(row.sync_run_id),
            serde_json::json!(row.schema_version),
            serde_json::json!(row.updated_at),
        ];
        self.execute(sql, Some(params)).await?;
        Ok(())
    }

    /// Delete stale `publish_refs` rows left by older all-refs sync runs.
    ///
    /// Callers run this only after a full sync has upserted every current
    /// local branch/tag ref with `current_sync_run_id` and after the site
    /// latest/default CAS succeeds. That order prevents deleting the previous
    /// default ref while `publish_sites.default_ref` still points at it.
    pub async fn delete_publish_refs_for_other_sync_runs(
        &self,
        site_id: &str,
        current_sync_run_id: &str,
    ) -> Result<i64, D1Error> {
        let D1Statement { sql, params } =
            delete_publish_refs_for_other_sync_runs_statement(site_id, current_sync_run_id);
        let result = self.execute(&sql, params).await?;
        Ok(result.meta.and_then(|meta| meta.changes).unwrap_or(0))
    }

    /// Insert or update a `publish_files` row.
    pub async fn upsert_publish_file(&self, row: &PublishFileRow) -> Result<(), D1Error> {
        let sql = r#"
            INSERT INTO publish_files (
                site_id, revision_oid, path, display_mode, content_sha256,
                r2_key, size_bytes, language, schema_version
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(site_id, revision_oid, path) DO UPDATE SET
                display_mode = excluded.display_mode,
                content_sha256 = excluded.content_sha256,
                r2_key = excluded.r2_key,
                size_bytes = excluded.size_bytes,
                language = excluded.language,
                schema_version = excluded.schema_version
        "#;
        let params = vec![
            serde_json::json!(row.site_id),
            serde_json::json!(row.revision_oid),
            serde_json::json!(row.path),
            serde_json::json!(row.display_mode),
            serde_json::json!(row.content_sha256),
            serde_json::json!(row.r2_key),
            serde_json::json!(row.size_bytes),
            serde_json::json!(row.language),
            serde_json::json!(row.schema_version),
        ];
        self.execute(sql, Some(params)).await?;
        Ok(())
    }

    /// Insert or update a `publish_ai_objects` row.
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_publish_ai_object(&self, row: &PublishAiObjectRow) -> Result<(), D1Error> {
        let sql = r#"
            INSERT INTO publish_ai_objects (
                site_id, revision_oid, object_type, object_id, layer,
                r2_key, redaction_mode, payload_sha256, schema_version, created_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(site_id, revision_oid, object_type, object_id) DO UPDATE SET
                layer = excluded.layer,
                r2_key = excluded.r2_key,
                redaction_mode = excluded.redaction_mode,
                payload_sha256 = excluded.payload_sha256,
                schema_version = excluded.schema_version
        "#;
        let params = vec![
            serde_json::json!(row.site_id),
            serde_json::json!(row.revision_oid),
            serde_json::json!(row.object_type),
            serde_json::json!(row.object_id),
            serde_json::json!(row.layer),
            serde_json::json!(row.r2_key),
            serde_json::json!(row.redaction_mode),
            serde_json::json!(row.payload_sha256),
            serde_json::json!(row.schema_version),
            serde_json::json!(row.created_at),
        ];
        self.execute(sql, Some(params)).await?;
        Ok(())
    }

    /// List AI object rows for one published revision.
    pub async fn list_publish_ai_objects(
        &self,
        site_id: &str,
        revision_oid: &str,
    ) -> Result<Vec<PublishAiObjectRow>, D1Error> {
        let sql = r#"
            SELECT site_id, revision_oid, object_type, object_id, layer,
                   r2_key, redaction_mode, payload_sha256, schema_version, created_at
              FROM publish_ai_objects
             WHERE site_id = ?1 AND revision_oid = ?2
             ORDER BY layer, object_type, object_id
        "#;
        self.query(
            sql,
            Some(vec![
                serde_json::json!(site_id),
                serde_json::json!(revision_oid),
            ]),
        )
        .await
    }

    /// List AI version rows for one published revision.
    pub async fn list_publish_ai_versions(
        &self,
        site_id: &str,
        revision_oid: &str,
    ) -> Result<Vec<PublishAiVersionRow>, D1Error> {
        let sql = r#"
            SELECT site_id, ai_version_id, revision_oid, bundle_key, bundle_sha256,
                   object_count, redaction_mode, redaction_rules_version,
                   schema_version, created_at
              FROM publish_ai_versions
             WHERE site_id = ?1 AND revision_oid = ?2
             ORDER BY created_at, ai_version_id
        "#;
        self.query(
            sql,
            Some(vec![
                serde_json::json!(site_id),
                serde_json::json!(revision_oid),
            ]),
        )
        .await
    }

    /// Insert or update a `publish_ai_versions` row.
    pub async fn upsert_publish_ai_version(
        &self,
        row: &PublishAiVersionRow,
    ) -> Result<(), D1Error> {
        let sql = r#"
            INSERT INTO publish_ai_versions (
                site_id, ai_version_id, revision_oid, bundle_key, bundle_sha256,
                object_count, redaction_mode, redaction_rules_version,
                schema_version, created_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(site_id, ai_version_id) DO UPDATE SET
                revision_oid = excluded.revision_oid,
                bundle_key = excluded.bundle_key,
                bundle_sha256 = excluded.bundle_sha256,
                object_count = excluded.object_count,
                redaction_mode = excluded.redaction_mode,
                redaction_rules_version = excluded.redaction_rules_version,
                schema_version = excluded.schema_version
        "#;
        let params = vec![
            serde_json::json!(row.site_id),
            serde_json::json!(row.ai_version_id),
            serde_json::json!(row.revision_oid),
            serde_json::json!(row.bundle_key),
            serde_json::json!(row.bundle_sha256),
            serde_json::json!(row.object_count),
            serde_json::json!(row.redaction_mode),
            serde_json::json!(row.redaction_rules_version),
            serde_json::json!(row.schema_version),
            serde_json::json!(row.created_at),
        ];
        self.execute(sql, Some(params)).await?;
        Ok(())
    }

    /// List all `publish_refs` rows for one site.
    pub async fn list_publish_refs(&self, site_id: &str) -> Result<Vec<PublishRefRow>, D1Error> {
        let sql = "SELECT site_id, ref_name, ref_type, short_name, target_oid, \
                          revision_oid, is_default, sync_run_id, schema_version, updated_at \
                   FROM publish_refs WHERE site_id = ?1 \
                   ORDER BY ref_type, short_name";
        self.query(sql, Some(vec![serde_json::json!(site_id)]))
            .await
    }

    /// Find one revision row by `(site_id, revision_oid)`, regardless
    /// of status. Used for state inspection (publish status command);
    /// the Worker side filters `status = 'published'` separately so
    /// in-progress `syncing` rows never leak into reads.
    ///
    /// Codex Phase 2 P3 (closed): the earlier name was
    /// `find_publish_revision` which implied a published-only filter
    /// the SQL didn't have. Renamed to `find_publish_revision_any`
    /// to make the broader semantic explicit; new
    /// `find_published_revision` carries the published filter.
    pub async fn find_publish_revision_any(
        &self,
        site_id: &str,
        revision_oid: &str,
    ) -> Result<Option<PublishRevisionRow>, D1Error> {
        let sql = "SELECT site_id, revision_oid, status, code_manifest_key, ai_index_key, \
                          file_count, ai_object_count, ai_bundle_count, redaction_mode, \
                          redaction_rules_version, sync_run_id, schema_version, \
                          created_at, updated_at \
                   FROM publish_revisions WHERE site_id = ?1 AND revision_oid = ?2";
        let rows: Vec<PublishRevisionRow> = self
            .query(
                sql,
                Some(vec![
                    serde_json::json!(site_id),
                    serde_json::json!(revision_oid),
                ]),
            )
            .await?;
        Ok(rows.into_iter().next())
    }

    /// Find one revision row that is in `status = 'published'`.
    /// Mirror of the Worker-side semantic: in-flight `syncing` rows
    /// are invisible.
    pub async fn find_published_revision(
        &self,
        site_id: &str,
        revision_oid: &str,
    ) -> Result<Option<PublishRevisionRow>, D1Error> {
        let sql = "SELECT site_id, revision_oid, status, code_manifest_key, ai_index_key, \
                          file_count, ai_object_count, ai_bundle_count, redaction_mode, \
                          redaction_rules_version, sync_run_id, schema_version, \
                          created_at, updated_at \
                   FROM publish_revisions \
                   WHERE site_id = ?1 AND revision_oid = ?2 AND status = 'published'";
        let rows: Vec<PublishRevisionRow> = self
            .query(
                sql,
                Some(vec![
                    serde_json::json!(site_id),
                    serde_json::json!(revision_oid),
                ]),
            )
            .await?;
        Ok(rows.into_iter().next())
    }

    /// Find one publish_sites row by site_id.
    pub async fn find_publish_site(
        &self,
        site_id: &str,
    ) -> Result<Option<PublishSiteRow>, D1Error> {
        let sql = publish_site_select_sql("site_id = ?1");
        let rows: Vec<PublishSiteRow> = self
            .query(&sql, Some(vec![serde_json::json!(site_id)]))
            .await?;
        Ok(rows.into_iter().next())
    }

    /// Resolve a publish site by its clone-domain slug.
    ///
    /// The schema pins `(clone_domain, slug)` as unique, so callers
    /// receive at most one row. Slug lookup is the human-facing
    /// `libra+cloud://<clone-domain>/<slug>` path.
    pub async fn find_publish_site_by_clone_slug(
        &self,
        clone_domain: &str,
        slug: &str,
    ) -> Result<Option<PublishSiteRow>, D1Error> {
        let sql = publish_site_select_sql("clone_domain = ?1 AND slug = ?2");
        let rows: Vec<PublishSiteRow> = self
            .query(
                &sql,
                Some(vec![
                    serde_json::json!(clone_domain),
                    serde_json::json!(slug),
                ]),
            )
            .await?;
        Ok(rows.into_iter().next())
    }

    /// Resolve a publish site by its stable repo id.
    ///
    /// The `repo/<repo_id>` URL form survives slug renames because
    /// the schema pins `(clone_domain, repo_id)` as unique.
    pub async fn find_publish_site_by_clone_repo_id(
        &self,
        clone_domain: &str,
        repo_id: &str,
    ) -> Result<Option<PublishSiteRow>, D1Error> {
        let sql = publish_site_select_sql("clone_domain = ?1 AND repo_id = ?2");
        let rows: Vec<PublishSiteRow> = self
            .query(
                &sql,
                Some(vec![
                    serde_json::json!(clone_domain),
                    serde_json::json!(repo_id),
                ]),
            )
            .await?;
        Ok(rows.into_iter().next())
    }
}

fn publish_site_select_sql(where_clause: &str) -> String {
    format!(
        "SELECT site_id, repo_id, clone_domain, slug, display_origin, \
                name, visibility, status, worker_name, default_ref, \
                latest_revision_oid, refs_generation, max_preview_bytes, \
                schema_version, created_at, updated_at \
         FROM publish_sites WHERE {where_clause} LIMIT 1"
    )
}

fn publish_site_latest_update_statement(update: &PublishSiteLatestUpdate<'_>) -> D1Statement {
    let mut sql = "UPDATE publish_sites \
                   SET default_ref = ?2, latest_revision_oid = ?3, refs_generation = ?4, \
                       updated_at = ?5 \
                   WHERE site_id = ?1"
        .to_string();
    let mut params = vec![
        serde_json::json!(update.site_id),
        serde_json::json!(update.default_ref),
        serde_json::json!(update.latest_revision_oid),
        serde_json::json!(update.next_refs_generation),
        serde_json::json!(update.updated_at),
    ];
    if !update.force {
        sql.push_str(" AND refs_generation = ?6");
        params.push(serde_json::json!(update.expected_refs_generation));
    }
    D1Statement {
        sql,
        params: Some(params),
    }
}

fn publish_site_latest_update_result_from_changes(changes: i64) -> PublishSiteLatestUpdateResult {
    if changes > 0 {
        PublishSiteLatestUpdateResult::Updated
    } else {
        PublishSiteLatestUpdateResult::Conflict
    }
}

fn delete_publish_refs_for_other_sync_runs_statement(
    site_id: &str,
    current_sync_run_id: &str,
) -> D1Statement {
    D1Statement {
        sql: "DELETE FROM publish_refs WHERE site_id = ?1 AND sync_run_id != ?2".to_string(),
        params: Some(vec![
            serde_json::json!(site_id),
            serde_json::json!(current_sync_run_id),
        ]),
    }
}

/// Split a multi-statement SQL script into individual statements.
///
/// SQLite's REST `execute` accepts one statement per call, but the
/// publish migrations are written as multi-statement files for
/// readability. This helper splits on top-level `;` boundaries
/// while ignoring `;` inside string literals (`'…'`) and inside
/// SQL `CREATE TRIGGER … BEGIN …; …; END;` blocks. Line comments
/// (`--…\n`) and block comments (`/* … */`) are stripped so the
/// final statement count stays stable across `cargo +nightly fmt`
/// reflow.
///
/// Codex Phase 2 P1 (closed): the earlier draft processed `;`
/// before flushing the running `prev_word` into the BEGIN/END
/// counter, so `END;` collapsed an entire trigger block into a
/// single multi-statement payload. The fix flushes the pending
/// keyword on EVERY non-alphanumeric boundary (whitespace,
/// punctuation, end-of-input) before checking for `;`.
fn split_sql_statements(input: &str) -> Vec<String> {
    let mut statements: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    let mut depth_begin_end: i32 = 0;
    let mut prev_word = String::new();

    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        // Strip line comments outside of string literals.
        if !in_string && ch == '-' && chars.peek() == Some(&'-') {
            for next_ch in chars.by_ref() {
                if next_ch == '\n' {
                    current.push('\n');
                    break;
                }
            }
            flush_keyword(&mut prev_word, &mut depth_begin_end);
            continue;
        }
        // Strip block comments outside of string literals.
        if !in_string && ch == '/' && chars.peek() == Some(&'*') {
            // Consume the leading '*'.
            chars.next();
            // Walk until we see the closing '*/'.
            while let Some(next_ch) = chars.next() {
                if next_ch == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    break;
                }
            }
            current.push(' ');
            flush_keyword(&mut prev_word, &mut depth_begin_end);
            continue;
        }

        if ch == '\'' && !in_string {
            in_string = true;
            current.push(ch);
            flush_keyword(&mut prev_word, &mut depth_begin_end);
            continue;
        }
        if ch == '\'' && in_string {
            // Handle SQL `''` escape inside a string. Codex Phase 2
            // P1 (closed): use `if let Some(...)` instead of
            // `chars.next().unwrap()` so the splitter never
            // panics on a malformed input mid-stream.
            if chars.peek() == Some(&'\'') {
                current.push(ch);
                if let Some(escaped) = chars.next() {
                    current.push(escaped);
                }
                continue;
            }
            in_string = false;
            current.push(ch);
            flush_keyword(&mut prev_word, &mut depth_begin_end);
            continue;
        }

        if !in_string && ch.is_alphanumeric() {
            prev_word.push(ch.to_ascii_lowercase());
        } else if !in_string {
            // Codex Phase 2 P1 (closed): flush the pending keyword
            // on any non-alphanumeric boundary BEFORE we check
            // whether the current char is `;`. This means `END;`
            // increments the depth-tracker for the `END` token,
            // closing the BEGIN/END block, and the subsequent `;`
            // check sees `depth_begin_end == 0` so the trigger
            // statement closes correctly.
            flush_keyword(&mut prev_word, &mut depth_begin_end);
        }

        if ch == ';' && !in_string && depth_begin_end == 0 {
            let trimmed = current.trim().to_string();
            if !trimmed.is_empty() {
                statements.push(trimmed);
            }
            current.clear();
            continue;
        }

        current.push(ch);
    }
    // Flush any trailing keyword at end-of-input so a file that
    // ends with `END` (no trailing semicolon) still updates the
    // depth tracker before we capture the final statement.
    flush_keyword(&mut prev_word, &mut depth_begin_end);
    let trailing = current.trim().to_string();
    if !trailing.is_empty() {
        statements.push(trailing);
    }
    statements
}

/// Apply the BEGIN/END nesting effect of the keyword that just
/// ended at a word boundary, then clear the buffer.
fn flush_keyword(prev_word: &mut String, depth_begin_end: &mut i32) {
    match prev_word.as_str() {
        "begin" => *depth_begin_end += 1,
        "end" if *depth_begin_end > 0 => {
            *depth_begin_end -= 1;
        }
        _ => {}
    }
    prev_word.clear();
}

/// Local view of a `publish_sites` row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishSiteRow {
    pub site_id: String,
    pub repo_id: String,
    pub clone_domain: String,
    pub slug: String,
    pub display_origin: String,
    pub name: String,
    pub visibility: String,
    pub status: String,
    pub worker_name: String,
    pub default_ref: Option<String>,
    pub latest_revision_oid: Option<String>,
    pub refs_generation: i64,
    pub max_preview_bytes: i64,
    pub schema_version: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// Local view of a `publish_revisions` row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishRevisionRow {
    pub site_id: String,
    pub revision_oid: String,
    pub status: String,
    pub code_manifest_key: Option<String>,
    pub ai_index_key: Option<String>,
    pub file_count: i64,
    pub ai_object_count: i64,
    pub ai_bundle_count: i64,
    pub redaction_mode: String,
    pub redaction_rules_version: String,
    pub sync_run_id: String,
    pub schema_version: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// Local view of a `publish_refs` row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishRefRow {
    pub site_id: String,
    pub ref_name: String,
    pub ref_type: String,
    pub short_name: String,
    pub target_oid: String,
    pub revision_oid: String,
    pub is_default: i64,
    pub sync_run_id: String,
    pub schema_version: i64,
    pub updated_at: String,
}

/// Local view of a `publish_files` row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishFileRow {
    pub site_id: String,
    pub revision_oid: String,
    pub path: String,
    pub display_mode: String,
    pub content_sha256: Option<String>,
    pub r2_key: Option<String>,
    pub size_bytes: i64,
    pub language: Option<String>,
    pub schema_version: i64,
}

/// Local view of a `publish_ai_objects` row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishAiObjectRow {
    pub site_id: String,
    pub revision_oid: String,
    pub object_type: String,
    pub object_id: String,
    pub layer: String,
    pub r2_key: String,
    pub redaction_mode: String,
    pub payload_sha256: String,
    pub schema_version: i64,
    pub created_at: String,
}

/// Local view of a `publish_ai_versions` row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishAiVersionRow {
    pub site_id: String,
    pub ai_version_id: String,
    pub revision_oid: String,
    pub bundle_key: String,
    pub bundle_sha256: String,
    pub object_count: i64,
    pub redaction_mode: String,
    pub redaction_rules_version: String,
    pub schema_version: i64,
    pub created_at: String,
}

/// Local view of a `publish_sync_runs` row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishSyncRunRow {
    pub sync_run_id: String,
    pub site_id: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub refs_count: i64,
    pub revision_count: i64,
    pub file_count: i64,
    pub ai_object_count: i64,
    pub ai_bundle_count: i64,
    pub warnings_json: String,
    pub error_message: Option<String>,
    pub cli_version: String,
    pub schema_version: i64,
}

/// Local view of an `agent_session` row prepared for D1 mirroring.
///
/// Source-compatible legacy session projection retained for embedders. The
/// token-fenced cloud writer uses [`AgentSessionV2Row`] so adding the monotonic
/// sync generation does not add a required field to this public struct.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentSessionRow {
    pub session_id: String,
    pub agent_kind: String,
    pub provider_session_id: String,
    pub state: String,
    pub working_dir: String,
    pub worktree_id: Option<String>,
    pub parent_commit: Option<String>,
    pub parent_session_id: Option<String>,
    pub metadata_json: String,
    pub redaction_report: String,
    pub started_at: i64,
    pub last_event_at: i64,
    pub stopped_at: Option<i64>,
    pub schema_version: i64,
}

/// Versioned remote-capture projection with an explicit monotonic generation.
/// The original [`AgentSessionRow`] remains source-compatible for embedders.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentSessionV2Row {
    pub session_id: String,
    pub agent_kind: String,
    pub provider_session_id: String,
    pub state: String,
    pub working_dir: String,
    pub worktree_id: Option<String>,
    pub parent_commit: Option<String>,
    pub parent_session_id: Option<String>,
    pub metadata_json: String,
    pub redaction_report: String,
    pub started_at: i64,
    pub last_event_at: i64,
    pub stopped_at: Option<i64>,
    pub schema_version: i64,
    pub sync_revision: i64,
}

/// Local view of an `agent_checkpoint` row prepared for D1 mirroring.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentCheckpointRow {
    pub checkpoint_id: String,
    pub session_id: String,
    pub parent_checkpoint_id: Option<String>,
    pub scope: String,
    /// Nullable per the `2026050501` follow-up migration.
    pub parent_commit: Option<String>,
    pub tree_oid: String,
    pub metadata_blob_oid: String,
    pub traces_commit: String,
    pub tool_use_id: Option<String>,
    pub subagent_session_id: Option<String>,
    pub description: Option<String>,
    pub created_at: i64,
}

/// Versioned checkpoint projection used by fenced capture generations.
///
/// Keep [`AgentCheckpointRow`] unchanged for source compatibility with
/// callers that construct the original public row type directly.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentCheckpointV2Row {
    pub checkpoint_id: String,
    pub session_id: String,
    pub parent_checkpoint_id: Option<String>,
    pub scope: String,
    pub parent_commit: Option<String>,
    pub tree_oid: String,
    pub metadata_blob_oid: String,
    pub traces_commit: String,
    pub tool_use_id: Option<String>,
    pub subagent_session_id: Option<String>,
    pub description: Option<String>,
    pub created_at: i64,
    pub sync_revision: i64,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentCheckpointPruneTombstoneRow {
    pub checkpoint_id: String,
    pub session_id: String,
    pub pruned_at: i64,
}

/// PD-03: a SESSION-level erasure tombstone mirrored to D1. Keyed by the
/// provider identity `(agent_kind, provider_session_id)` — the same
/// anti-resurrection key `erase_session_local` writes locally.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentImportTombstoneRow {
    pub agent_kind: String,
    pub provider_session_id: String,
    pub erased_session_id: String,
    pub source_fingerprint: Option<String>,
    pub erased_at: i64,
}

#[derive(Debug, Deserialize)]
struct AgentCheckpointIdRow {
    checkpoint_id: String,
}

impl From<AgentCheckpointV2Row> for AgentCheckpointRow {
    fn from(row: AgentCheckpointV2Row) -> Self {
        Self {
            checkpoint_id: row.checkpoint_id,
            session_id: row.session_id,
            parent_checkpoint_id: row.parent_checkpoint_id,
            scope: row.scope,
            parent_commit: row.parent_commit,
            tree_oid: row.tree_oid,
            metadata_blob_oid: row.metadata_blob_oid,
            traces_commit: row.traces_commit,
            tool_use_id: row.tool_use_id,
            subagent_session_id: row.subagent_session_id,
            description: row.description,
            created_at: row.created_at,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentSubagentContentClaimRow {
    pub parent_session_id: String,
    pub provider_kind: String,
    pub source_key: String,
    pub content_schema_version: i64,
    pub revision_cursor: i64,
    pub sync_revision: i64,
    pub current_revision: i64,
    pub current_checkpoint_id: Option<String>,
    pub current_digest: Option<String>,
    pub fence_token: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentSubagentContentRevisionRow {
    pub parent_session_id: String,
    pub provider_kind: String,
    pub source_key: String,
    pub content_schema_version: i64,
    pub revision: i64,
    pub checkpoint_id: String,
    pub content_digest: String,
    pub source_channel: String,
    pub partial: i64,
    pub created_at: i64,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentSubagentLinkRow {
    pub content_checkpoint_id: String,
    pub parent_session_id: String,
    pub link_state: String,
    pub boundary_checkpoint_id: Option<String>,
    pub stable_subagent_id: Option<String>,
    pub sync_revision: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Default)]
pub struct AgentCaptureRestoreCatalogRows {
    pub sessions: Vec<AgentSessionV2Row>,
    pub checkpoints: Vec<AgentCheckpointV2Row>,
    pub prune_tombstones: Vec<AgentCheckpointPruneTombstoneRow>,
    /// PD-03 session-erasure tombstones (tombstone-first on restore).
    pub import_tombstones: Vec<AgentImportTombstoneRow>,
    pub claims: Vec<AgentSubagentContentClaimRow>,
    pub revisions: Vec<AgentSubagentContentRevisionRow>,
    pub links: Vec<AgentSubagentLinkRow>,
    /// Remaining capacity from the caller's aggregate restore budget. Object
    /// indexes are charged by the cloud restore layer after this catalog read.
    pub remaining_rows: usize,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentCaptureGenerationRow {
    pub repo_id: String,
    pub generation: i64,
    pub state: String,
    pub writer_token: Option<String>,
    pub object_index_digest: Option<String>,
    pub object_index_count: Option<i64>,
    pub object_index_scope: Option<String>,
    pub object_index_generation: Option<i64>,
    pub traces_head: Option<String>,
    pub started_at: i64,
    pub completed_at: Option<i64>,
}

#[derive(Debug, Clone, Copy)]
pub struct AgentCaptureGenerationManifest<'a> {
    pub object_index_digest: &'a str,
    pub object_index_count: i64,
    pub object_index_scope: &'a str,
    pub object_index_generation: i64,
    pub traces_head: Option<&'a str>,
}

/// One row of the `object_index` table.
///
/// Mirrors the on-disk SQLite columns one-to-one so that local and remote rows can
/// be diffed without translation.
#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct ObjectIndexRow {
    pub o_id: String,
    pub o_type: String,
    pub o_size: i64,
    pub repo_id: String,
    pub created_at: i64,
    /// `0` when only stored locally; `1` once synced to D1.
    pub is_synced: i32,
}

/// One row of the `repositories` table.
#[derive(Debug, Deserialize, Serialize)]
pub struct RepositoryRow {
    pub repo_id: String,
    pub name: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[cfg(test)]
mod tests {
    use std::{env, ffi::OsString};

    use serial_test::serial;
    use tempfile::tempdir;

    use super::*;
    use crate::{
        internal::config::ConfigKv,
        utils::test::{ChangeDirGuard, ScopedEnvVar, setup_with_new_libra_in},
    };

    #[test]
    fn agent_capture_restore_budget_is_shared_across_tables() {
        let mut remaining = 5;
        charge_agent_capture_restore_rows(&mut remaining, 3, "sessions")
            .expect("first table fits aggregate budget");
        charge_agent_capture_restore_rows(&mut remaining, 2, "checkpoints")
            .expect("second table exactly consumes aggregate budget");
        let error = charge_agent_capture_restore_rows(&mut remaining, 1, "subagent revisions")
            .expect_err("third table must not receive a fresh per-table budget");
        assert_eq!(remaining, 0);
        assert_eq!(error.code, 2011);
        assert!(error.message.contains("aggregate row safety bound"));
        assert!(error.message.contains("subagent revisions"));
    }

    /// RAII guard that removes an env var on construction and restores it on drop.
    /// Local copy of the helper used elsewhere — kept self-contained so the test
    /// module here has no cross-module dependency on `client_storage.rs`.
    struct ClearedEnvVarGuard {
        key: String,
        previous: Option<OsString>,
    }

    impl ClearedEnvVarGuard {
        fn new(key: &str) -> Self {
            let previous = env::var_os(key);
            // SAFETY: unit tests mutate process env in a controlled serial context.
            unsafe {
                env::remove_var(key);
            }
            Self {
                key: key.to_string(),
                previous,
            }
        }
    }

    impl Drop for ClearedEnvVarGuard {
        fn drop(&mut self) {
            // SAFETY: this restores the exact previous value for the same env key.
            unsafe {
                if let Some(value) = &self.previous {
                    env::set_var(&self.key, value);
                } else {
                    env::remove_var(&self.key);
                }
            }
        }
    }

    /// Scenario: a `D1Statement` with parameters must serialise both `sql` and
    /// `params` fields. Pins the wire format so an accidental `serde` rename does
    /// not silently break the Cloudflare API contract.
    #[test]
    fn test_d1_statement_serialization() {
        let stmt = D1Statement {
            sql: "SELECT * FROM test WHERE id = ?1".to_string(),
            params: Some(vec![serde_json::json!(1)]),
        };
        let json = serde_json::to_string(&stmt).unwrap();
        assert!(json.contains("SELECT"));
        assert!(json.contains("params"));
    }

    /// Scenario: a `D1Statement` without parameters must omit the `params` key
    /// entirely. The single-statement `/query` endpoint rejects requests where
    /// `params` is present but null, so omission is required, not optional.
    #[test]
    fn test_d1_statement_no_params() {
        let stmt = D1Statement {
            sql: "SELECT * FROM test".to_string(),
            params: None,
        };
        let json = serde_json::to_string(&stmt).unwrap();
        assert!(json.contains("SELECT"));
        assert!(!json.contains("params"));
    }

    #[test]
    fn d1_client_default_api_url_uses_cloudflare_query_endpoint() {
        let client = D1Client::new(
            "account-123".to_string(),
            "token-123".to_string(),
            "database-123".to_string(),
        );

        let url = client.api_url().expect("default D1 API URL should parse");

        assert_eq!(
            url.as_str(),
            "https://api.cloudflare.com/client/v4/accounts/account-123/d1/database/database-123/query"
        );
    }

    #[test]
    fn d1_client_custom_api_base_url_uses_query_endpoint() {
        let client = D1Client::new_with_api_base_url(
            "account-123".to_string(),
            "token-123".to_string(),
            "database-123".to_string(),
            "http://127.0.0.1:8787/client/v4",
        )
        .expect("custom D1 API base URL should parse");

        let url = client.api_url().expect("custom D1 API URL should parse");

        assert_eq!(
            url.as_str(),
            "http://127.0.0.1:8787/client/v4/accounts/account-123/d1/database/database-123/query"
        );
    }

    #[test]
    fn agent_capture_batch_wire_shape_scales_by_bounded_requests() {
        let rows = (0..257)
            .map(|revision| AgentSubagentContentRevisionRow {
                parent_session_id: "parent".to_string(),
                provider_kind: "claude_code".to_string(),
                source_key: format!("source/sha256/{}", "a".repeat(64)),
                content_schema_version: 1,
                revision,
                checkpoint_id: format!("checkpoint-{revision}"),
                content_digest: format!("digest-{revision}"),
                source_channel: "import".to_string(),
                partial: 0,
                created_at: revision,
            })
            .collect::<Vec<_>>();
        let statements = rows
            .chunks(128)
            .map(|page| {
                D1Client::agent_capture_json_batch_statement(
                    "WITH incoming(value) AS (SELECT value FROM json_each(?2)) \
                     SELECT value FROM incoming WHERE ?3 = ?3",
                    "repo",
                    "writer-token",
                    page,
                    "revision",
                )
                .expect("build bounded D1 statement")
            })
            .collect::<Vec<_>>();
        assert_eq!(statements.len(), 3);
        let sizes =
            statements
                .iter()
                .map(|statement| {
                    let wire = serde_json::to_value(statement).expect("serialize D1 statement");
                    assert_eq!(wire["params"][0], "repo");
                    assert_eq!(wire["params"][2], "writer-token");
                    assert!(wire["sql"].as_str().is_some_and(|sql| {
                        sql.contains("json_each(?2)") && sql.contains("?3")
                    }));
                    let payload = wire["params"][1]
                        .as_str()
                        .expect("batch parameter is JSON text");
                    serde_json::from_str::<Vec<AgentSubagentContentRevisionRow>>(payload)
                        .expect("batch JSON text decodes")
                        .len()
                })
                .collect::<Vec<_>>();
        assert_eq!(sizes, [128, 128, 1]);
    }

    #[tokio::test]
    async fn agent_capture_json_text_batch_executes_with_sqlite_json_each() {
        use sea_orm::{ConnectionTrait, Database, Statement};

        let rows = (1..=2)
            .map(|revision| AgentSubagentContentRevisionRow {
                parent_session_id: "parent".to_string(),
                provider_kind: "claude_code".to_string(),
                source_key: "source".to_string(),
                content_schema_version: 1,
                revision,
                checkpoint_id: format!("checkpoint-{revision}"),
                content_digest: format!("digest-{revision}"),
                source_channel: "import".to_string(),
                partial: 0,
                created_at: revision,
            })
            .collect::<Vec<_>>();
        let batch = D1Client::agent_capture_json_batch_statement(
            "WITH incoming(value) AS (SELECT value FROM json_each(?2))
             INSERT INTO batch_values(value)
             SELECT CAST(json_extract(value, '$.revision') AS INTEGER)
             FROM incoming WHERE ?1 = 'repo' AND ?3 = 'writer-token'",
            "repo",
            "writer-token",
            &rows,
            "revision",
        )
        .expect("build D1-compatible batch");
        let params = batch.params.expect("batch params");
        let payload = params[1].as_str().expect("JSON text parameter").to_string();
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open SQLite JSON fixture");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "CREATE TABLE batch_values (value INTEGER NOT NULL)".to_string(),
        ))
        .await
        .expect("create batch target");
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            batch.sql,
            ["repo".into(), payload.into(), "writer-token".into()],
        ))
        .await
        .expect("execute JSON-text batch through SQLite json_each");
        let values = conn
            .query_all_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT value FROM batch_values ORDER BY value".to_string(),
            ))
            .await
            .expect("read batch values")
            .into_iter()
            .map(|row| row.try_get_by::<i64, _>("value").expect("batch value"))
            .collect::<Vec<_>>();
        assert_eq!(values, [1, 2]);
    }

    #[tokio::test]
    async fn subagent_claim_batch_guard_rejects_revision_cursor_regression() {
        use sea_orm::{ConnectionTrait, Database, Statement};

        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open claim high-water fixture");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "CREATE TABLE agent_subagent_content_claim (
                source_key TEXT PRIMARY KEY, revision_cursor INTEGER NOT NULL,
                sync_revision INTEGER NOT NULL, current_revision INTEGER NOT NULL,
                current_checkpoint_id TEXT, current_digest TEXT
             )"
            .to_string(),
        ))
        .await
        .expect("create claim high-water fixture");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "INSERT INTO agent_subagent_content_claim VALUES
             ('source', 9, 3, 9, 'checkpoint-9', 'digest-9')"
                .to_string(),
        ))
        .await
        .expect("seed claim high-water");
        let sql = format!(
            "INSERT INTO agent_subagent_content_claim VALUES
             ('source', 8, 4, 8, 'checkpoint-8', 'digest-8')
             ON CONFLICT(source_key) DO UPDATE SET
                revision_cursor = excluded.revision_cursor,
                sync_revision = excluded.sync_revision,
                current_revision = excluded.current_revision,
                current_checkpoint_id = excluded.current_checkpoint_id,
                current_digest = excluded.current_digest
             WHERE {AGENT_SUBAGENT_CLAIM_UPDATE_GUARD}"
        );
        let result = conn
            .execute_raw(Statement::from_string(conn.get_database_backend(), sql))
            .await
            .expect("apply guarded claim update");
        assert_eq!(result.rows_affected(), 0);
        let row = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT revision_cursor, sync_revision FROM agent_subagent_content_claim"
                    .to_string(),
            ))
            .await
            .expect("read guarded claim")
            .expect("claim row");
        assert_eq!(
            row.try_get_by::<i64, _>("revision_cursor")
                .expect("revision cursor"),
            9
        );
        assert_eq!(
            row.try_get_by::<i64, _>("sync_revision")
                .expect("sync revision"),
            3
        );
    }

    #[tokio::test]
    async fn object_index_catalog_seed_accepts_existing_rows_and_preserves_generations() {
        use sea_orm::{ConnectionTrait, Database, Statement};

        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open object-index seed fixture");
        for sql in [
            "CREATE TABLE object_index (repo_id TEXT NOT NULL)",
            "CREATE TABLE object_index_catalog_generation (
                repo_id TEXT PRIMARY KEY, generation INTEGER NOT NULL
             )",
            "INSERT INTO object_index VALUES ('existing'), ('existing'), ('new')",
            "INSERT INTO object_index_catalog_generation VALUES ('existing', 7)",
            OBJECT_INDEX_CATALOG_SEED_SQL,
        ] {
            conn.execute_raw(Statement::from_string(
                conn.get_database_backend(),
                sql.to_string(),
            ))
            .await
            .expect("apply object-index generation seed SQL");
        }
        let generations = conn
            .query_all_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT repo_id, generation FROM object_index_catalog_generation ORDER BY repo_id"
                    .to_string(),
            ))
            .await
            .expect("query seeded object-index generations")
            .into_iter()
            .map(|row| {
                (
                    row.try_get_by::<String, _>("repo_id")
                        .expect("seeded repository id"),
                    row.try_get_by::<i64, _>("generation")
                        .expect("seeded generation"),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            generations,
            [("existing".to_string(), 7), ("new".to_string(), 0)]
        );
    }

    #[tokio::test]
    async fn object_index_catalog_readiness_is_published_after_triggers() {
        use sea_orm::{ConnectionTrait, Database, Statement};

        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open object-index readiness fixture");
        for sql in [
            "CREATE TABLE object_index (
                id INTEGER PRIMARY KEY, o_id TEXT NOT NULL, o_type TEXT NOT NULL,
                o_size INTEGER NOT NULL, repo_id TEXT NOT NULL, created_at INTEGER NOT NULL,
                is_synced INTEGER NOT NULL, UNIQUE(repo_id, o_id)
             )",
            "CREATE TABLE object_index_catalog_generation (
                repo_id TEXT PRIMARY KEY, generation INTEGER NOT NULL
             )",
            OBJECT_INDEX_CATALOG_READY_TABLE_SQL,
            OBJECT_INDEX_CATALOG_INVALIDATE_SQL,
            OBJECT_INDEX_CATALOG_SEED_SQL,
            "INSERT INTO object_index VALUES (1, 'before', 'blob', 1, 'repo', 1, 1)",
        ] {
            conn.execute_raw(Statement::from_string(
                conn.get_database_backend(),
                sql.to_string(),
            ))
            .await
            .expect("prepare object-index readiness fixture");
        }
        let ready_before = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT EXISTS(
                    SELECT 1 FROM object_index_catalog_generation_ready WHERE singleton = 1
                 ) AS present"
                    .to_string(),
            ))
            .await
            .expect("probe readiness before trigger installation")
            .expect("readiness probe row")
            .try_get_by::<i64, _>("present")
            .expect("readiness value");
        assert_eq!(ready_before, 0);

        for sql in OBJECT_INDEX_CATALOG_TRIGGERS
            .into_iter()
            .chain(std::iter::once(OBJECT_INDEX_CATALOG_PUBLISH_READY_SQL))
        {
            conn.execute_raw(Statement::from_string(
                conn.get_database_backend(),
                sql.to_string(),
            ))
            .await
            .expect("publish object-index readiness");
        }
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "INSERT INTO object_index VALUES (2, 'after', 'blob', 1, 'repo', 1, 1)".to_string(),
        ))
        .await
        .expect("mutate catalog after readiness");
        let generation = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT generation FROM object_index_catalog_generation WHERE repo_id = 'repo'"
                    .to_string(),
            ))
            .await
            .expect("query ready generation")
            .expect("generation row")
            .try_get_by::<i64, _>("generation")
            .expect("generation value");
        assert_eq!(generation, 1);
    }

    #[tokio::test]
    async fn object_index_catalog_generation_tracks_every_page_invalidating_mutation() {
        use sea_orm::{ConnectionTrait, Database, Statement};

        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open object-index generation fixture");
        for ddl in [
            "CREATE TABLE object_index (
                id INTEGER PRIMARY KEY, o_id TEXT NOT NULL, o_type TEXT NOT NULL,
                o_size INTEGER NOT NULL, repo_id TEXT NOT NULL, created_at INTEGER NOT NULL,
                is_synced INTEGER NOT NULL, UNIQUE(repo_id, o_id)
             )",
            "CREATE TABLE object_index_catalog_generation (
                repo_id TEXT PRIMARY KEY, generation INTEGER NOT NULL
             )",
        ] {
            conn.execute_raw(Statement::from_string(
                conn.get_database_backend(),
                ddl.to_string(),
            ))
            .await
            .expect("create object-index generation fixture");
        }
        for trigger in OBJECT_INDEX_CATALOG_TRIGGERS {
            conn.execute_raw(Statement::from_string(
                conn.get_database_backend(),
                trigger.to_string(),
            ))
            .await
            .expect("install object-index generation trigger");
        }
        for mutation in [
            "INSERT INTO object_index VALUES (1, 'b', 'blob', 1, 'repo', 1, 1)",
            "UPDATE object_index SET o_size = 2 WHERE repo_id = 'repo' AND o_id = 'b'",
            "INSERT INTO object_index VALUES (2, 'a', 'blob', 1, 'repo', 1, 1)",
            "DELETE FROM object_index WHERE repo_id = 'repo' AND o_id = 'b'",
        ] {
            conn.execute_raw(Statement::from_string(
                conn.get_database_backend(),
                mutation.to_string(),
            ))
            .await
            .expect("mutate object-index generation fixture");
        }
        let generation = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT generation FROM object_index_catalog_generation WHERE repo_id = 'repo'"
                    .to_string(),
            ))
            .await
            .expect("query object-index generation")
            .expect("object-index generation row")
            .try_get_by::<i64, _>("generation")
            .expect("object-index generation value");
        assert_eq!(generation, 4);
    }

    #[tokio::test]
    async fn generation_start_excludes_active_writer_and_recovers_expired_writer() {
        use sea_orm::{ConnectionTrait, Database, Statement};

        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open generation lease fixture");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "CREATE TABLE agent_capture_generation (
                repo_id TEXT PRIMARY KEY, generation INTEGER NOT NULL,
                state TEXT NOT NULL, writer_token TEXT,
                object_index_digest TEXT, object_index_count INTEGER,
                object_index_scope TEXT, object_index_generation INTEGER,
                traces_head TEXT, started_at INTEGER NOT NULL, completed_at INTEGER
             )"
            .to_string(),
        ))
        .await
        .expect("create generation lease fixture");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "INSERT INTO agent_capture_generation VALUES (
                'repo', 7, 'publishing', 'active', 'old', 1,
                'checkpoint_projection', 1, NULL,
                CAST(strftime('%s', 'now') AS INTEGER), NULL
             )"
            .to_string(),
        ))
        .await
        .expect("seed active generation");
        let params = || {
            vec![
                "repo".into(),
                "next".into(),
                "digest".into(),
                2_i64.into(),
                "checkpoint_projection".into(),
                2_i64.into(),
                Option::<String>::None.into(),
                7_i64.into(),
                AGENT_CAPTURE_GENERATION_LEASE_SECONDS.into(),
            ]
        };
        let active = conn
            .query_all_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                AGENT_CAPTURE_BEGIN_GENERATION_FROM_SQL,
                params(),
            ))
            .await
            .expect("probe active generation lease");
        assert!(
            active.is_empty(),
            "an active publisher must retain its fence"
        );

        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "UPDATE agent_capture_generation SET started_at = 0 WHERE repo_id = 'repo'".to_string(),
        ))
        .await
        .expect("expire generation lease");
        let recovered = conn
            .query_all_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                AGENT_CAPTURE_BEGIN_GENERATION_FROM_SQL,
                params(),
            ))
            .await
            .expect("recover expired generation");
        assert_eq!(recovered.len(), 1);
        assert_eq!(
            recovered[0]
                .try_get_by::<i64, _>("generation")
                .expect("recovered generation"),
            8
        );
        assert_eq!(
            recovered[0]
                .try_get_by::<String, _>("writer_token")
                .expect("recovered writer"),
            "next"
        );
    }

    #[tokio::test]
    async fn capture_completion_atomically_fences_object_index_generation() {
        use sea_orm::{ConnectionTrait, Database, Statement};

        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open capture completion fixture");
        for ddl in [
            "CREATE TABLE object_index_catalog_generation (
                repo_id TEXT PRIMARY KEY, generation INTEGER NOT NULL
             )",
            "CREATE TABLE agent_capture_generation (
                repo_id TEXT PRIMARY KEY, generation INTEGER NOT NULL,
                state TEXT NOT NULL, writer_token TEXT,
                object_index_digest TEXT, object_index_count INTEGER,
                object_index_scope TEXT, object_index_generation INTEGER,
                traces_head TEXT, started_at INTEGER NOT NULL, completed_at INTEGER
             )",
            "INSERT INTO object_index_catalog_generation VALUES ('repo', 2)",
            "INSERT INTO agent_capture_generation VALUES (
                'repo', 1, 'publishing', 'writer', 'digest', 0,
                'checkpoint_projection', 1, NULL, 1, NULL
             )",
        ] {
            conn.execute_raw(Statement::from_string(
                conn.get_database_backend(),
                ddl.to_string(),
            ))
            .await
            .expect("prepare capture completion fixture");
        }

        let stale = conn
            .query_all_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                AGENT_CAPTURE_COMPLETE_GENERATION_SQL,
                ["repo".into(), "writer".into(), 1_i64.into()],
            ))
            .await
            .expect("run stale completion predicate");
        assert!(
            stale.is_empty(),
            "an object-index mutation after manifest read must fence completion"
        );

        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "UPDATE object_index_catalog_generation SET generation = 1 WHERE repo_id = 'repo'"
                .to_string(),
        ))
        .await
        .expect("restore matching object generation");
        let complete = conn
            .query_all_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                AGENT_CAPTURE_COMPLETE_GENERATION_SQL,
                ["repo".into(), "writer".into(), 1_i64.into()],
            ))
            .await
            .expect("run stable completion predicate");
        assert_eq!(complete.len(), 1);
    }

    #[tokio::test]
    async fn agent_capture_v2_barriers_reject_legacy_mixed_version_writers() {
        use sea_orm::{ConnectionTrait, Database, Statement};

        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open SQLite compatibility fixture");
        for ddl in [
            "CREATE TABLE agent_session (session_id TEXT PRIMARY KEY)",
            "CREATE TABLE agent_checkpoint (checkpoint_id TEXT PRIMARY KEY)",
        ] {
            conn.execute_raw(Statement::from_string(
                conn.get_database_backend(),
                ddl.to_string(),
            ))
            .await
            .expect("create legacy capture table");
        }
        for ddl in AGENT_CAPTURE_LEGACY_WRITE_BARRIERS {
            conn.execute_raw(Statement::from_string(
                conn.get_database_backend(),
                ddl.to_string(),
            ))
            .await
            .expect("install legacy writer barrier");
        }

        for sql in [
            "INSERT INTO agent_session (session_id) VALUES ('legacy-session')",
            "INSERT INTO agent_checkpoint (checkpoint_id) VALUES ('legacy-checkpoint')",
        ] {
            let error = conn
                .execute_raw(Statement::from_string(
                    conn.get_database_backend(),
                    sql.to_string(),
                ))
                .await
                .expect_err("legacy writer must be rejected after v2 activation");
            assert!(
                error
                    .to_string()
                    .contains("legacy agent capture writer is fenced"),
                "unexpected barrier error: {error}"
            );
        }
    }

    #[test]
    fn unfenced_single_row_agent_capture_writes_fail_closed() {
        for label in [
            "agent session",
            "agent checkpoint",
            "subagent content claim",
            "subagent content revision",
            "subagent link",
        ] {
            let error = reject_unfenced_agent_capture_write(label)
                .expect_err("single-row v2 writes require a publication generation");
            assert_eq!(error.code, 3003);
            assert!(error.message.contains("active agent-capture generation"));
        }
    }

    #[test]
    fn legacy_agent_session_projection_keeps_its_original_struct_literal_shape() {
        let row = AgentSessionRow {
            session_id: "session".to_string(),
            agent_kind: "claude_code".to_string(),
            provider_session_id: "provider-session".to_string(),
            state: "active".to_string(),
            working_dir: "/repo".to_string(),
            worktree_id: None,
            parent_commit: None,
            parent_session_id: None,
            metadata_json: "{}".to_string(),
            redaction_report: "{}".to_string(),
            started_at: 1,
            last_event_at: 2,
            stopped_at: None,
            schema_version: 1,
        };
        assert_eq!(row.session_id, "session");
    }

    #[test]
    fn legacy_agent_checkpoint_projection_keeps_its_original_struct_literal_shape() {
        let row = AgentCheckpointRow {
            checkpoint_id: "checkpoint".to_string(),
            session_id: "session".to_string(),
            parent_checkpoint_id: None,
            scope: "committed".to_string(),
            parent_commit: None,
            tree_oid: "tree".to_string(),
            metadata_blob_oid: "metadata".to_string(),
            traces_commit: "traces".to_string(),
            tool_use_id: None,
            subagent_session_id: None,
            description: None,
            created_at: 1,
        };
        assert_eq!(row.checkpoint_id, "checkpoint");
    }

    #[test]
    fn compatibility_reads_wait_for_completed_v2_adoption() {
        assert_eq!(agent_session_compatibility_table(false), "agent_session");
        assert_eq!(
            agent_checkpoint_compatibility_table(false),
            "agent_checkpoint"
        );
        assert_eq!(
            agent_session_compatibility_table(true),
            "agent_capture_session_v2"
        );
        assert_eq!(
            agent_checkpoint_compatibility_table(true),
            "agent_capture_checkpoint_v2"
        );
    }

    #[tokio::test]
    async fn agent_capture_v2_adopts_legacy_sessions_once_at_generation_zero() {
        use sea_orm::{ConnectionTrait, Database, Statement};

        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open SQLite adoption fixture");
        for ddl in [
            "CREATE TABLE agent_session (
                session_id TEXT, repo_id TEXT, agent_kind TEXT, provider_session_id TEXT,
                state TEXT, working_dir TEXT, worktree_id TEXT, parent_commit TEXT,
                parent_session_id TEXT, metadata_json TEXT, redaction_report TEXT,
                started_at INTEGER, last_event_at INTEGER, stopped_at INTEGER,
                schema_version INTEGER, synced_at INTEGER
             )",
            "CREATE TABLE agent_capture_session_v2 (
                session_id TEXT, repo_id TEXT, agent_kind TEXT, provider_session_id TEXT,
                state TEXT, working_dir TEXT, worktree_id TEXT, parent_commit TEXT,
                parent_session_id TEXT, metadata_json TEXT, redaction_report TEXT,
                started_at INTEGER, last_event_at INTEGER, stopped_at INTEGER,
                schema_version INTEGER, sync_revision INTEGER, synced_at INTEGER,
                PRIMARY KEY (repo_id, session_id)
             )",
            "CREATE TABLE agent_capture_schema_migration (
                version INTEGER PRIMARY KEY, state TEXT, completed_at INTEGER
             )",
            "INSERT INTO agent_capture_schema_migration VALUES (2, 'copying', NULL)",
            "INSERT INTO agent_session VALUES (
                'legacy-1', 'repo', 'claude_code', 'provider-1', 'active', '/repo',
                NULL, NULL, NULL, '{}', '{}', 1, 2, NULL, 1, 3
             )",
        ] {
            conn.execute_raw(Statement::from_string(
                conn.get_database_backend(),
                ddl.to_string(),
            ))
            .await
            .expect("seed adoption fixture");
        }
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            AGENT_CAPTURE_LEGACY_SESSION_ADOPTION_SQL.to_string(),
        ))
        .await
        .expect("adopt legacy session");
        let adopted = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT sync_revision FROM agent_capture_session_v2 WHERE session_id = 'legacy-1'"
                    .to_string(),
            ))
            .await
            .expect("query adopted session")
            .expect("adopted row");
        assert_eq!(
            adopted
                .try_get_by::<i64, _>("sync_revision")
                .expect("adopted generation"),
            0,
            "a generation-1 current client must be able to supersede legacy state"
        );

        for sql in [
            "UPDATE agent_capture_schema_migration SET state = 'complete' WHERE version = 2",
            "INSERT INTO agent_session VALUES (
                'legacy-2', 'repo', 'claude_code', 'provider-2', 'active', '/repo',
                NULL, NULL, NULL, '{}', '{}', 1, 2, NULL, 1, 3
             )",
        ] {
            conn.execute_raw(Statement::from_string(
                conn.get_database_backend(),
                sql.to_string(),
            ))
            .await
            .expect("finish one-time adoption fixture");
        }
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            AGENT_CAPTURE_LEGACY_SESSION_ADOPTION_SQL.to_string(),
        ))
        .await
        .expect("re-run adoption after completion");
        let count = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT COUNT(*) AS n FROM agent_capture_session_v2".to_string(),
            ))
            .await
            .expect("count adopted sessions")
            .expect("count row")
            .try_get_by::<i64, _>("n")
            .expect("count value");
        assert_eq!(
            count, 1,
            "later legacy writes must never enter the v2 snapshot"
        );
    }

    #[tokio::test]
    async fn agent_capture_v2_discards_unrestorable_legacy_checkpoint_orphans() {
        use sea_orm::{ConnectionTrait, Database, Statement};

        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open SQLite orphan adoption fixture");
        for ddl in [
            "CREATE TABLE agent_capture_schema_migration (
                version INTEGER PRIMARY KEY, state TEXT, completed_at INTEGER
             )",
            "INSERT INTO agent_capture_schema_migration VALUES (2, 'copying', NULL)",
            "CREATE TABLE agent_capture_session_v2 (
                session_id TEXT, repo_id TEXT, PRIMARY KEY (repo_id, session_id)
             )",
            "INSERT INTO agent_capture_session_v2 VALUES ('present-session', 'repo')",
            "CREATE TABLE agent_checkpoint (
                checkpoint_id TEXT, repo_id TEXT, session_id TEXT,
                parent_checkpoint_id TEXT, scope TEXT, parent_commit TEXT,
                tree_oid TEXT, metadata_blob_oid TEXT, traces_commit TEXT,
                tool_use_id TEXT, subagent_session_id TEXT, description TEXT,
                created_at INTEGER, synced_at INTEGER
             )",
            "INSERT INTO agent_checkpoint VALUES
                ('good', 'repo', 'present-session', NULL, 'committed', NULL,
                 'tree-good', 'meta-good', 'commit-good', NULL, NULL, NULL, 1, 1),
                ('orphan', 'repo', 'missing-session', NULL, 'committed', NULL,
                 'tree-orphan', 'meta-orphan', 'commit-orphan', NULL, NULL, NULL, 2, 2)",
            "CREATE TABLE agent_capture_checkpoint_v2 (
                checkpoint_id TEXT, repo_id TEXT, session_id TEXT,
                parent_checkpoint_id TEXT, scope TEXT, parent_commit TEXT,
                tree_oid TEXT, metadata_blob_oid TEXT, traces_commit TEXT,
                tool_use_id TEXT, subagent_session_id TEXT, description TEXT,
                created_at INTEGER, sync_revision INTEGER, synced_at INTEGER,
                PRIMARY KEY (repo_id, checkpoint_id)
             )",
        ] {
            conn.execute_raw(Statement::from_string(
                conn.get_database_backend(),
                ddl.to_string(),
            ))
            .await
            .expect("seed legacy orphan adoption fixture");
        }
        for sql in [
            AGENT_CAPTURE_LEGACY_CHECKPOINT_ADOPTION_SQL,
            AGENT_CAPTURE_LEGACY_ORPHAN_CLEANUP_SQL,
        ] {
            conn.execute_raw(Statement::from_string(
                conn.get_database_backend(),
                sql.to_string(),
            ))
            .await
            .expect("adopt and reconcile legacy checkpoints");
        }
        let rows = conn
            .query_all_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT checkpoint_id, sync_revision FROM agent_capture_checkpoint_v2".to_string(),
            ))
            .await
            .expect("query reconciled adopted checkpoints");
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0]
                .try_get_by::<String, _>("checkpoint_id")
                .expect("checkpoint id"),
            "good"
        );
        assert_eq!(
            rows[0]
                .try_get_by::<i64, _>("sync_revision")
                .expect("adopted checkpoint generation"),
            0
        );
    }

    /// Scenario: with all three D1 env vars unset and the local repo config holding
    /// the values, `from_env` should successfully build a client. This is the
    /// happy path users follow when storing credentials in `vault.env.*` rather
    /// than in their shell profile.
    #[test]
    #[serial]
    fn d1_client_from_env_reads_values_from_local_config() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let repo = tempdir().unwrap();
        rt.block_on(setup_with_new_libra_in(repo.path()));
        let _cwd = ChangeDirGuard::new(repo.path());
        let _account = ClearedEnvVarGuard::new("LIBRA_D1_ACCOUNT_ID");
        let _token = ClearedEnvVarGuard::new("LIBRA_D1_API_TOKEN");
        let _database = ClearedEnvVarGuard::new("LIBRA_D1_DATABASE_ID");

        rt.block_on(async {
            ConfigKv::set(
                "vault.env.LIBRA_D1_ACCOUNT_ID",
                "account-from-config",
                false,
            )
            .await
            .unwrap();
            ConfigKv::set("vault.env.LIBRA_D1_API_TOKEN", "token-from-config", false)
                .await
                .unwrap();
            ConfigKv::set("vault.env.LIBRA_D1_DATABASE_ID", "db-from-config", false)
                .await
                .unwrap();
        });

        let client = rt
            .block_on(D1Client::from_env())
            .expect("local config values should initialize D1 client");
        assert_eq!(client.account_id, "account-from-config");
        assert_eq!(client.api_token, "token-from-config");
        assert_eq!(client.database_id, "db-from-config");
    }

    /// Scenario: D1 credentials follow the same priority chain as the rest of
    /// the secret surface (docs/development/commands/config.md 12-Factor rule):
    /// process env > local vault > global vault. Local vault is the fallback
    /// when env is unset. Mirrors v0.17.906's resolve_env_for_target_process_
    /// env_overrides_local_vault fix.
    #[test]
    #[serial]
    fn d1_client_from_env_process_env_overrides_local_config() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let repo = tempdir().unwrap();
        rt.block_on(setup_with_new_libra_in(repo.path()));
        let _cwd = ChangeDirGuard::new(repo.path());
        let _account = ScopedEnvVar::set("LIBRA_D1_ACCOUNT_ID", "account-from-env");
        let _token = ScopedEnvVar::set("LIBRA_D1_API_TOKEN", "token-from-env");
        let _database = ScopedEnvVar::set("LIBRA_D1_DATABASE_ID", "db-from-env");

        rt.block_on(async {
            ConfigKv::set(
                "vault.env.LIBRA_D1_ACCOUNT_ID",
                "account-from-config",
                false,
            )
            .await
            .unwrap();
            ConfigKv::set("vault.env.LIBRA_D1_API_TOKEN", "token-from-config", false)
                .await
                .unwrap();
            ConfigKv::set("vault.env.LIBRA_D1_DATABASE_ID", "db-from-config", false)
                .await
                .unwrap();
        });

        // env wins.
        let client = rt
            .block_on(D1Client::from_env())
            .expect("D1Client::from_env should pick up env values when present");
        assert_eq!(client.account_id, "account-from-env");
        assert_eq!(client.api_token, "token-from-env");
        assert_eq!(client.database_id, "db-from-env");

        // …and vault is the fallback when env is unset.
        drop(_account);
        drop(_token);
        drop(_database);
        let _account = ClearedEnvVarGuard::new("LIBRA_D1_ACCOUNT_ID");
        let _token = ClearedEnvVarGuard::new("LIBRA_D1_API_TOKEN");
        let _database = ClearedEnvVarGuard::new("LIBRA_D1_DATABASE_ID");
        let client = rt
            .block_on(D1Client::from_env())
            .expect("D1Client::from_env should fall back to vault");
        assert_eq!(client.account_id, "account-from-config");
        assert_eq!(client.api_token, "token-from-config");
        assert_eq!(client.database_id, "db-from-config");
    }

    /// Scenario: when `LIBRA_CONFIG_GLOBAL_DB` points at a corrupt file, the
    /// resolver should emit a `1101`-coded error rather than silently degrading to
    /// "missing variable". This pins the contract that lets the cloud-backup
    /// command surface actionable errors.
    #[test]
    #[serial]
    fn d1_client_from_env_surfaces_global_config_connection_errors() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let repo = tempdir().unwrap();
        rt.block_on(setup_with_new_libra_in(repo.path()));
        let _cwd = ChangeDirGuard::new(repo.path());
        let _account = ClearedEnvVarGuard::new("LIBRA_D1_ACCOUNT_ID");
        let _token = ClearedEnvVarGuard::new("LIBRA_D1_API_TOKEN");
        let _database = ClearedEnvVarGuard::new("LIBRA_D1_DATABASE_ID");

        let bad_global_dir = tempdir().unwrap();
        let bad_global_db = bad_global_dir.path().join("bad-global.db");
        std::fs::write(&bad_global_db, "not sqlite").unwrap();
        let _global_db =
            crate::utils::test::ScopedEnvVar::set("LIBRA_CONFIG_GLOBAL_DB", &bad_global_db);

        let err = match rt.block_on(D1Client::from_env()) {
            Ok(_) => panic!("global config resolution failure should surface"),
            Err(err) => err,
        };
        assert_eq!(err.code, 1101);
        assert!(
            err.message.contains("failed to open config database")
                || err.message.contains("failed to connect to global config"),
            "unexpected error: {}",
            err.message
        );
    }

    /// Codex Phase 2 P3 + pass-1 P1 (closed): pin the SQL splitter
    /// behaviour so the publish migrations apply one statement at a
    /// time without breaking on `BEGIN…END` blocks (used by the
    /// trigger migrations), `--` line comments, or `/* */` block
    /// comments. The previous draft of the splitter incorrectly
    /// processed `;` before flushing the running keyword, so `END;`
    /// collapsed an entire trigger block into a single multi-
    /// statement payload.
    #[test]
    fn split_sql_statements_handles_triggers_and_comments() {
        let sql = r#"
            -- Header comment, ignored.
            /* Block comment with ; and BEGIN inside, also ignored. */
            CREATE TABLE IF NOT EXISTS foo (id INTEGER);
            CREATE TRIGGER IF NOT EXISTS foo_guard
                BEFORE INSERT ON foo
                FOR EACH ROW
                WHEN NEW.id < 0
            BEGIN
                SELECT RAISE(ABORT, 'id must be >= 0');
            END;
            CREATE TRIGGER IF NOT EXISTS foo_guard_update
                BEFORE UPDATE ON foo
                FOR EACH ROW
                WHEN NEW.id < 0
            BEGIN
                SELECT RAISE(ABORT, 'id must be >= 0');
            END;
            CREATE INDEX IF NOT EXISTS foo_id ON foo (id);
        "#;
        let stmts = split_sql_statements(sql);
        assert_eq!(stmts.len(), 4, "got: {stmts:?}");
        assert!(stmts[0].starts_with("CREATE TABLE IF NOT EXISTS foo"));
        assert!(stmts[1].contains("CREATE TRIGGER IF NOT EXISTS foo_guard"));
        assert!(stmts[1].contains("END"));
        assert!(stmts[2].contains("CREATE TRIGGER IF NOT EXISTS foo_guard_update"));
        assert!(stmts[2].contains("END"));
        assert!(stmts[3].starts_with("CREATE INDEX IF NOT EXISTS foo_id"));
    }

    /// Pin the splitter against single-quoted literals containing
    /// semicolons (e.g. `RAISE(ABORT, 'must be > 0; restart')`).
    /// A naive splitter would chop the literal in two.
    #[test]
    fn split_sql_statements_preserves_quoted_semicolons() {
        let sql = r#"
            SELECT 'one; two; three' AS phrase;
            SELECT 1;
        "#;
        let stmts = split_sql_statements(sql);
        assert_eq!(stmts.len(), 2, "got: {stmts:?}");
        assert!(stmts[0].contains("'one; two; three'"));
        assert_eq!(stmts[1], "SELECT 1");
    }

    /// The publish migrations under `sql/publish/` must split into a
    /// non-empty statement list and the BEGIN/END trigger blocks must
    /// not be chopped. Acts as a smoke test for `ensure_publish_schema`.
    #[test]
    fn publish_migrations_split_cleanly() {
        let sql_0001 = include_str!("../../sql/publish/0001_publish.sql");
        let sql_0002 = include_str!("../../sql/publish/0002_publish_digest_check.sql");
        let sql_0003 =
            include_str!("../../sql/publish/0003_publish_max_preview_trigger_replace.sql");
        let sql_0004 = include_str!("../../sql/publish/0004_publish_refs_index.sql");
        for (label, sql) in [
            ("0001", sql_0001),
            ("0002", sql_0002),
            ("0003", sql_0003),
            ("0004", sql_0004),
        ] {
            let stmts = split_sql_statements(sql);
            assert!(!stmts.is_empty(), "{label} produced no statements");
            for (idx, stmt) in stmts.iter().enumerate() {
                let begin_count = stmt
                    .split_whitespace()
                    .filter(|w| w.eq_ignore_ascii_case("BEGIN"))
                    .count();
                let end_count = stmt
                    .split_whitespace()
                    .filter(|w| w.eq_ignore_ascii_case("END"))
                    .count();
                assert_eq!(
                    begin_count, end_count,
                    "{label} statement #{idx} has unbalanced BEGIN/END:\n{stmt}",
                );
            }
        }
    }

    #[test]
    fn cloud_clone_domain_resolve_test_uses_unique_slug_and_repo_id_lookups() {
        let slug_sql = publish_site_select_sql("clone_domain = ?1 AND slug = ?2");
        assert!(
            slug_sql.contains("FROM publish_sites WHERE clone_domain = ?1 AND slug = ?2 LIMIT 1"),
            "slug lookup must use the clone-domain unique key: {slug_sql}"
        );
        assert!(
            slug_sql.contains("site_id, repo_id, clone_domain, slug"),
            "slug lookup must return the restore identity columns: {slug_sql}"
        );
        assert!(
            slug_sql.contains("default_ref"),
            "slug lookup must return default ref metadata: {slug_sql}"
        );
        assert!(
            slug_sql.contains("latest_revision_oid"),
            "slug lookup must return latest revision metadata: {slug_sql}"
        );
        assert!(
            slug_sql.contains("refs_generation"),
            "slug lookup must return refs generation metadata: {slug_sql}"
        );

        let repo_sql = publish_site_select_sql("clone_domain = ?1 AND repo_id = ?2");
        assert!(
            repo_sql
                .contains("FROM publish_sites WHERE clone_domain = ?1 AND repo_id = ?2 LIMIT 1"),
            "repo lookup must use the slug-rename-proof unique key: {repo_sql}"
        );
        assert!(
            !repo_sql.contains("slug = ?2"),
            "repo lookup must not depend on the current slug: {repo_sql}"
        );
    }

    #[test]
    fn publish_latest_cas_update_requires_generation_match_unless_forced() {
        let guarded = PublishSiteLatestUpdate {
            site_id: "site-1",
            default_ref: Some("refs/heads/main"),
            latest_revision_oid: Some("abcdef0123456789abcdef0123456789abcdef01"),
            next_refs_generation: 12,
            expected_refs_generation: 11,
            updated_at: "2026-05-13T12:00:00Z",
            force: false,
        };
        let guarded_statement = publish_site_latest_update_statement(&guarded);
        assert!(
            guarded_statement
                .sql
                .contains("WHERE site_id = ?1 AND refs_generation = ?6"),
            "non-force update must use refs_generation as the CAS guard: {}",
            guarded_statement.sql
        );
        assert_eq!(guarded_statement.params.as_ref().expect("params").len(), 6);
        assert_eq!(
            guarded_statement.params.as_ref().expect("params").last(),
            Some(&serde_json::json!(11))
        );

        let forced = PublishSiteLatestUpdate {
            force: true,
            ..guarded
        };
        let forced_statement = publish_site_latest_update_statement(&forced);
        assert!(
            !forced_statement.sql.contains("refs_generation = ?6"),
            "force update must bypass the stale-generation guard: {}",
            forced_statement.sql
        );
        assert_eq!(forced_statement.params.as_ref().expect("params").len(), 5);
    }

    #[test]
    fn publish_latest_cas_update_reports_conflict_on_zero_changes() {
        assert_eq!(
            publish_site_latest_update_result_from_changes(1),
            PublishSiteLatestUpdateResult::Updated
        );
        assert_eq!(
            publish_site_latest_update_result_from_changes(0),
            PublishSiteLatestUpdateResult::Conflict
        );
    }

    #[test]
    fn publish_ref_stale_delete_scopes_to_site_and_current_sync_run() {
        let statement = delete_publish_refs_for_other_sync_runs_statement("site-1", "sync-current");
        assert_eq!(
            statement.sql,
            "DELETE FROM publish_refs WHERE site_id = ?1 AND sync_run_id != ?2"
        );
        let params = statement.params.expect("delete statement must bind params");
        assert_eq!(
            params,
            vec![
                serde_json::json!("site-1"),
                serde_json::json!("sync-current")
            ]
        );
    }

    /// Codex Phase 2 P2 (closed): the `ensure_publish_schema`
    /// migration list is hardcoded via `include_str!` because Rust
    /// has no built-in directory glob at compile time. This test
    /// reads the on-disk `sql/publish/` directory and asserts every
    /// `*.sql` file is present in the hardcoded list, so a future
    /// `0005_*.sql` cannot ship without an explicit code change.
    #[test]
    fn publish_migration_list_matches_disk() {
        use std::path::PathBuf;
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dir = manifest_dir.join("sql/publish");
        let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
            .expect("read sql/publish/")
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let path = e.path();
                if path.extension().and_then(|s| s.to_str()) == Some("sql") {
                    path.file_name()?.to_str().map(|s| s.to_string())
                } else {
                    None
                }
            })
            .collect();
        on_disk.sort();
        let expected: Vec<&str> = vec![
            "0001_publish.sql",
            "0002_publish_digest_check.sql",
            "0003_publish_max_preview_trigger_replace.sql",
            "0004_publish_refs_index.sql",
        ];
        assert_eq!(
            on_disk.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            expected,
            "sql/publish/ contents drifted from `ensure_publish_schema` include_str! list; \
             update both lists together",
        );
    }

    /// Codex Phase 2 P1 (regression): the 0002 trigger migration
    /// MUST split into one statement per trigger, not a single
    /// multi-trigger payload. The earlier splitter collapsed every
    /// `END;` because the `;` cleared the keyword buffer before the
    /// `END` was processed at a word boundary.
    #[test]
    fn publish_0002_splits_one_statement_per_trigger() {
        let sql = include_str!("../../sql/publish/0002_publish_digest_check.sql");
        let stmts = split_sql_statements(sql);
        // 0002 ships eight triggers (max_preview INSERT/UPDATE +
        // three sha256 columns × INSERT/UPDATE). Pin the count so a
        // future drift surfaces here instead of in CI under D1.
        assert_eq!(stmts.len(), 8, "0002 stmts: {stmts:?}");
        for (idx, stmt) in stmts.iter().enumerate() {
            assert!(
                stmt.contains("CREATE TRIGGER IF NOT EXISTS"),
                "0002 statement #{idx} is not a trigger:\n{stmt}",
            );
            assert!(
                stmt.ends_with("END") || stmt.contains("END"),
                "0002 statement #{idx} should close with END:\n{stmt}",
            );
        }
    }
}
