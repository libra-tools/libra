//! Reflog persistence layer that writes formatted entries, queries by ref name, and enforces transaction-safe patterns for updates.

use std::{
    collections::HashSet,
    fmt::{Debug, Display, Formatter},
    future::Future,
    pin::Pin,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseTransaction, DbBackend, DbErr,
    EntityTrait, QueryFilter, QueryOrder, Set, Statement, TransactionError, TransactionTrait,
};
use serde::Serialize;
use tokio::time::sleep;

use crate::internal::{
    config,
    db::get_db_conn_instance,
    head::Head,
    model::{
        reflog,
        reflog::{ActiveModel, Model},
    },
};

pub const HEAD: &str = "HEAD";
const SQLITE_BUSY_MAX_RETRIES: usize = 15;
const SQLITE_BUSY_RETRY_BASE_MS: u64 = 100;

fn is_sqlite_busy(err: &DbErr) -> bool {
    let message = err.to_string();
    message.contains("database is locked") || message.contains("database schema is locked")
}

#[derive(Debug)]
pub struct ReflogContext {
    pub old_oid: String,
    pub new_oid: String,
    pub action: ReflogAction,
}

#[derive(Debug)]
pub enum ReflogError {
    DatabaseError(DbErr),
    TransactionError(TransactionError<DbErr>),
    /// A `gc.reflog*` config value could not be read or parsed.
    Config(String),
}

impl Display for ReflogError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DatabaseError(_) => write!(f, "failed to access reflog storage"),
            Self::TransactionError(_) => write!(f, "failed to update reflog"),
            Self::Config(detail) => write!(f, "invalid reflog expire config: {detail}"),
        }
    }
}

impl From<DbErr> for ReflogError {
    fn from(err: DbErr) -> Self {
        ReflogError::DatabaseError(err)
    }
}

impl From<TransactionError<DbErr>> for ReflogError {
    fn from(err: TransactionError<DbErr>) -> Self {
        ReflogError::TransactionError(err)
    }
}

impl std::error::Error for ReflogError {}
impl Display for ReflogContext {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match &self.action {
            ReflogAction::Commit { message } => write!(
                f,
                "{}",
                message.lines().next().unwrap_or("(no commit message)")
            ),
            ReflogAction::Switch { from, to } => write!(f, "moving from {from} to {to}"),
            ReflogAction::Checkout { from, to } => write!(f, "moving from {from} to {to}"),
            ReflogAction::Reset { target } => write!(f, "moving to {target}"),
            ReflogAction::Merge { branch, policy } => write!(f, "merge {branch}:{policy}"),
            ReflogAction::CherryPick { source_message } => write!(
                f,
                "{}",
                source_message
                    .trim()
                    .lines()
                    .next()
                    .unwrap_or("(no commit message)")
            ),
            ReflogAction::Fetch => write!(f, "fast-forward"),
            ReflogAction::Pull => write!(f, "fast-forward"),
            ReflogAction::Push => write!(f, "push"),
            ReflogAction::Rebase { state, details } => write!(f, "({state}) {details}"),
            ReflogAction::Clone { from } => write!(f, "from {from}"),
            ReflogAction::UpdateRef { message } => write!(f, "{message}"),
        }
    }
}

#[derive(Debug)]
pub enum ReflogAction {
    Commit {
        message: String,
    },
    Reset {
        target: String,
    },
    Checkout {
        from: String,
        to: String,
    },
    Switch {
        from: String,
        to: String,
    },
    Merge {
        branch: String,
        policy: String,
    },
    CherryPick {
        source_message: String,
    },
    Rebase {
        state: String,
        details: String,
    },
    Fetch,
    Pull,
    Push,
    Clone {
        from: String,
    },
    /// A direct ref update via `update-ref` (carries the optional `-m` reason).
    UpdateRef {
        message: String,
    },
}

#[derive(Copy, Clone)]
pub enum ReflogActionKind {
    Commit,
    Reset,
    // we don't need `checkout` because we have `switch`,
    Checkout,
    Switch,
    Merge,
    CherryPick,
    Rebase,
    Fetch,
    // pull is a combination of `fetch` and `merge`, maybe we don't need to do anything...
    Pull,
    Push,
    Clone,
    UpdateRef,
}

impl Display for ReflogActionKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Commit => write!(f, "commit"),
            Self::Reset => write!(f, "reset"),
            Self::Checkout => write!(f, "checkout"),
            Self::Switch => write!(f, "switch"),
            Self::Merge => write!(f, "merge"),
            Self::CherryPick => write!(f, "cherry-pick"),
            Self::Rebase => write!(f, "rebase"),
            Self::Fetch => write!(f, "fetch"),
            Self::Pull => write!(f, "pull"),
            Self::Push => write!(f, "push"),
            Self::Clone => write!(f, "clone"),
            Self::UpdateRef => write!(f, "update-ref"),
        }
    }
}

impl ReflogAction {
    fn kind(&self) -> ReflogActionKind {
        match self {
            Self::Commit { .. } => ReflogActionKind::Commit,
            Self::Reset { .. } => ReflogActionKind::Reset,
            Self::Switch { .. } => ReflogActionKind::Switch,
            Self::Merge { .. } => ReflogActionKind::Merge,
            Self::Pull => ReflogActionKind::Pull,
            Self::Clone { .. } => ReflogActionKind::Clone,
            Self::CherryPick { .. } => ReflogActionKind::CherryPick,
            Self::Rebase { .. } => ReflogActionKind::Rebase,
            Self::Checkout { .. } => ReflogActionKind::Checkout,
            Self::Fetch => ReflogActionKind::Fetch,
            Self::Push => ReflogActionKind::Push,
            Self::UpdateRef { .. } => ReflogActionKind::UpdateRef,
        }
    }
}

pub struct Reflog;

impl Reflog {
    pub async fn insert_single_entry<C: ConnectionTrait>(
        db: &C,
        context: &ReflogContext,
        ref_to_log: &str,
    ) -> Result<(), ReflogError> {
        // considering that there are many commands that have not yet used user configs,
        // we just set default user info.
        let name = config::ConfigKv::get_with_conn(db, "user.name")
            .await
            .ok()
            .flatten()
            .map(|e| e.value)
            .unwrap_or("mega".to_string());
        let email = config::ConfigKv::get_with_conn(db, "user.email")
            .await
            .ok()
            .flatten()
            .map(|e| e.value)
            .unwrap_or("admin@mega.org".to_string());
        let message = context.to_string();

        // lore.md 2.1: the HEAD reflog is PER-WORKTREE; branch reflogs
        // (refs/heads/*) stay shared (worktree_id NULL).
        let worktree_id = if ref_to_log == HEAD {
            crate::utils::util::current_worktree_id()
        } else {
            None
        };
        let model = ActiveModel {
            ref_name: Set(ref_to_log.to_string()),
            old_oid: Set(context.old_oid.clone()),
            new_oid: Set(context.new_oid.clone()),
            action: Set(context.action.kind().to_string()),
            committer_name: Set(name),
            committer_email: Set(email),
            timestamp: Set(timestamp_seconds()),
            message: Set(message),
            worktree_id: Set(worktree_id),
            ..Default::default()
        };

        for attempt in 0..=SQLITE_BUSY_MAX_RETRIES {
            match model.clone().save(db).await {
                Ok(_) => return Ok(()),
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                }
                Err(err) => return Err(err.into()),
            }
        }
        Ok(())
    }

    /// insert a reflog record.
    /// see `ReflogContext`
    pub async fn insert(
        db: &DatabaseTransaction,
        context: ReflogContext,
        insert_ref: bool,
    ) -> Result<(), ReflogError> {
        ensure_reflog_table_exists(db).await?;
        let head = Head::current_with_conn(db).await;

        Self::insert_single_entry(db, &context, HEAD).await?;

        if let Head::Branch(branch_name) = head
            && insert_ref
        {
            let full_branch_ref = format!("refs/heads/{branch_name}");
            Self::insert_single_entry(db, &context, &full_branch_ref).await?;
        }
        Ok(())
    }

    pub async fn find_all<C: ConnectionTrait>(
        db: &C,
        ref_name: &str,
    ) -> Result<Vec<Model>, ReflogError> {
        Ok(Self::scope_head(
            reflog::Entity::find().filter(reflog::Column::RefName.eq(ref_name)),
            ref_name,
        )
        .order_by_desc(reflog::Column::Timestamp)
        .order_by_desc(reflog::Column::Id)
        .all(db)
        .await?)
    }

    pub async fn find_one<C: ConnectionTrait>(
        db: &C,
        ref_name: &str,
    ) -> Result<Option<Model>, ReflogError> {
        Ok(Self::scope_head(
            reflog::Entity::find().filter(reflog::Column::RefName.eq(ref_name)),
            ref_name,
        )
        .order_by_desc(reflog::Column::Timestamp)
        .order_by_desc(reflog::Column::Id)
        .one(db)
        .await?)
    }

    /// lore.md 2.1: scope a HEAD-reflog query to the current worktree (branch
    /// reflogs are shared, unscoped).
    fn scope_head(
        query: sea_orm::Select<reflog::Entity>,
        ref_name: &str,
    ) -> sea_orm::Select<reflog::Entity> {
        if ref_name != HEAD {
            return query;
        }
        match crate::utils::util::current_worktree_id() {
            Some(id) => query.filter(reflog::Column::WorktreeId.eq(id)),
            None => query.filter(reflog::Column::WorktreeId.is_null()),
        }
    }
}

fn timestamp_seconds() -> i64 {
    let now = SystemTime::now();
    let since_the_epoch = now.duration_since(UNIX_EPOCH).expect("Time went backwards");
    since_the_epoch.as_secs() as i64
}

/// Executes a database operation within a transaction and records a reflog entry upon success.
///
/// This function acts as a safe, atomic wrapper for any operation that needs to be
/// recorded in the reflog. It ensures that the core operation and the creation of its
/// corresponding reflog entry either both succeed and are committed, or both fail and
/// are rolled back. This prevents inconsistent states where an action is performed
/// but not logged.
///
/// # Example
///
/// Here is how you would use `with_reflog` to wrap a `commit` operation.
///
/// ```rust,ignore
/// // 1. First, prepare the context for the reflog entry.
/// let reflog_context = ReflogContext {
///     old_oid: "previous_commit_hash".to_string(),
///     new_oid: "new_commit_hash".to_string(),
///     action: ReflogAction::Commit {
///         message: message.to_string(),
///     }
/// };
///
/// // 2. Define the core database operation as an async closure.
/// //    Note that all DB calls inside MUST use the provided `txn` handle.
/// let core_operation = |txn: &DatabaseTransaction| Box::pin(async move {
///     // This is where you move the branch pointer, update HEAD, etc.
///     // IMPORTANT: Use `_with_conn` variants of your helper functions.
///     Branch::update_branch_with_conn(txn, "main", "new_commit_hash", None).await;
///     Head::update_with_conn(txn, Head::Branch("main".to_string()), None).await;
///
///     // The closure must return a Result compatible with DbErr.
///     // You can use `ReflogError`.
///     Ok(())
/// });
///
/// // 3. Execute the wrapper.
/// match with_reflog(reflog_context, core_operation, true).await {
///     Ok(_) => println!("Commit and reflog recorded successfully."),
///     Err(e) => eprintln!("Operation failed: {:?}", e),
/// }
/// ```
/// # Parameters
///
/// * `context`: A `ReflogContext` struct...
/// * `operation`: An asynchronous closure that performs the core database work...
/// * `insert_ref`: A boolean flag. If `true`, a reflog entry will be created for the
///   current branch in addition to HEAD. If `false`, only HEAD will be logged. This should
///   be `false` for operations like `checkout` that only move HEAD.
pub async fn with_reflog<F>(
    context: ReflogContext,
    operation: F,
    insert_ref: bool,
) -> Result<(), ReflogError>
where
    for<'b> F: FnOnce(
        &'b DatabaseTransaction,
    ) -> Pin<Box<dyn Future<Output = Result<(), DbErr>> + Send + 'b>>,
    F: Send + 'static,
{
    let db = get_db_conn_instance().await;
    db.transaction(|txn| {
        Box::pin(async move {
            operation(txn).await.map_err(ReflogError::from)?;
            Reflog::insert(txn, context, insert_ref).await?;
            Ok::<_, ReflogError>(())
        })
    })
    .await
    .map_err(|err| match err {
        TransactionError::Connection(err) => ReflogError::from(err),
        TransactionError::Transaction(err) => err,
    })
}

/// Check whether the current libra repo have a `reflog` table
async fn reflog_table_exists<C: ConnectionTrait>(db_conn: &C) -> Result<bool, ReflogError> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Sqlite,
        r#"
            SELECT COUNT(*)
            FROM sqlite_master
            WHERE type='table' AND name=?;
        "#,
        ["reflog".into()],
    );

    if let Some(result) = db_conn.query_one(stmt).await? {
        let count = result.try_get_by_index(0).unwrap_or(0);
        if count == 0 {
            return Ok(false);
        }
    }

    Ok(true)
}

/// Ensures that the 'reflog' table and its associated indexes exist in the database.
/// If they do not exist, they will be created.
async fn ensure_reflog_table_exists<C: ConnectionTrait>(db: &C) -> Result<(), ReflogError> {
    if reflog_table_exists(db).await? {
        return Ok(());
    }

    println!("Warning: The current libra repo does not have a `reflog` table, creating one...");
    let create_table_stmt = Statement::from_string(
        DbBackend::Sqlite,
        r#"
            CREATE TABLE IF NOT EXISTS `reflog` (
                `id`              INTEGER PRIMARY KEY AUTOINCREMENT,
                `ref_name`        TEXT NOT NULL,
                `old_oid`         TEXT NOT NULL,
                `new_oid`         TEXT NOT NULL,
                `committer_name`  TEXT NOT NULL,
                `committer_email` TEXT NOT NULL,
                `timestamp`       INTEGER NOT NULL,
                `action`          TEXT NOT NULL,
                `message`         TEXT NOT NULL
            );
        "#
        .to_string(),
    );

    for attempt in 0..=SQLITE_BUSY_MAX_RETRIES {
        match db.execute(create_table_stmt.clone()).await {
            Ok(_) => break,
            Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                sleep(Duration::from_millis(
                    SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                ))
                .await;
            }
            Err(err) => return Err(err.into()),
        }
    }

    let create_index_stmt = Statement::from_string(
        DbBackend::Sqlite,
        r#"
            CREATE INDEX IF NOT EXISTS idx_ref_name_timestamp ON `reflog`(`ref_name`, `timestamp`);
        "#
        .to_string(),
    );

    for attempt in 0..=SQLITE_BUSY_MAX_RETRIES {
        match db.execute(create_index_stmt.clone()).await {
            Ok(_) => break,
            Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                sleep(Duration::from_millis(
                    SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                ))
                .await;
            }
            Err(err) => return Err(err.into()),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// `reflog expire`
// ---------------------------------------------------------------------------

/// Absolute cutoff used to decide whether a reflog entry is expired.
///
/// All CLI / config values are normalised to this type at the parse layer (a
/// single `now` subtraction), so the cleanup layer never re-derives durations:
/// `Never` disables the dimension, `All` matches every entry (Git's `all`
/// token, time-independent), and `Before(secs)` matches entries strictly older
/// than the absolute Unix second `secs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpireCutoff {
    Never,
    All,
    Before(i64),
}

/// Whether `timestamp` (absolute Unix seconds) is expired under `cutoff`.
pub fn is_expired(cutoff: ExpireCutoff, timestamp: i64) -> bool {
    match cutoff {
        ExpireCutoff::Never => false,
        ExpireCutoff::All => true,
        ExpireCutoff::Before(c) => timestamp < c,
    }
}

/// Options for [`expire_reflog`]. Cutoffs are already absolute (see
/// [`ExpireCutoff`]); booleans mirror the `git reflog expire` flags.
#[derive(Debug, Clone)]
pub struct ExpireOptions {
    pub expire: ExpireCutoff,
    pub expire_unreachable: ExpireCutoff,
    pub rewrite: bool,
    pub updateref: bool,
    pub stale_fix: bool,
    pub dry_run: bool,
}

impl Default for ExpireOptions {
    fn default() -> Self {
        // Default to `Never` so a bare construction (no config, no flags) never
        // prunes anything; the CLI injects the 90/30-day defaults explicitly.
        Self {
            expire: ExpireCutoff::Never,
            expire_unreachable: ExpireCutoff::Never,
            rewrite: false,
            updateref: false,
            stale_fix: false,
            dry_run: false,
        }
    }
}

/// Why a single reflog entry was pruned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PruneReason {
    Expired,
    Unreachable,
    Stale,
}

/// A pruned reflog entry, surfaced in `--verbose` / JSON output.
#[derive(Debug, Clone, Serialize)]
pub struct PrunedEntry {
    pub index: usize,
    pub old_oid: String,
    pub new_oid: String,
    pub reason: PruneReason,
}

/// Per-ref summary of an expire run.
#[derive(Debug, Clone, Serialize)]
pub struct ExpireResult {
    pub ref_name: String,
    pub scanned: usize,
    pub pruned: usize,
    pub rewritten: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pruned_entries: Vec<PrunedEntry>,
}

impl ExpireResult {
    pub fn empty(ref_name: String) -> Self {
        Self {
            ref_name,
            scanned: 0,
            pruned: 0,
            rewritten: 0,
            pruned_entries: Vec::new(),
        }
    }
}

/// Read `gc.reflogExpire` / `gc.reflogExpireUnreachable`, falling back to Git's
/// 90-day / 30-day defaults. Returns absolute [`ExpireCutoff`]s (the only `now`
/// subtraction happens here). Invalid values yield [`ReflogError::Config`].
pub async fn expire_defaults_with_conn<C: ConnectionTrait>(
    conn: &C,
) -> Result<(ExpireCutoff, ExpireCutoff), ReflogError> {
    let expire = read_expire_config(conn, "gc.reflogExpire", 90).await?;
    let unreachable = read_expire_config(conn, "gc.reflogExpireUnreachable", 30).await?;
    Ok((expire, unreachable))
}

async fn read_expire_config<C: ConnectionTrait>(
    conn: &C,
    key: &str,
    default_days: i64,
) -> Result<ExpireCutoff, ReflogError> {
    let value = config::ConfigKv::get_with_conn(conn, key)
        .await
        .map_err(|e| ReflogError::Config(e.to_string()))?
        .map(|entry| entry.value);
    match value {
        None => Ok(ExpireCutoff::Before(now_seconds() - default_days * 86_400)),
        Some(raw) => parse_expire_cutoff(&raw).ok_or_else(|| {
            ReflogError::Config(format!("{key}='{raw}' is not a valid expire value"))
        }),
    }
}

/// Parse a `gc.reflog*` config string into an absolute cutoff. Handles the
/// special tokens, a bare number of days, and the relative/absolute forms that
/// [`crate::internal::log::date_parser::parse_date`] understands. Returns `None`
/// when the value cannot be parsed (CLI parsing is handled separately).
pub fn parse_expire_cutoff(raw: &str) -> Option<ExpireCutoff> {
    let trimmed = raw.trim();
    match trimmed.to_ascii_lowercase().as_str() {
        "never" => return Some(ExpireCutoff::Never),
        "all" => return Some(ExpireCutoff::All),
        "now" => return Some(ExpireCutoff::Before(now_seconds())),
        _ => {}
    }
    // A bare integer is a number of days (Git's `gc.reflogExpire` default form).
    if let Ok(days) = trimmed.parse::<i64>() {
        return Some(ExpireCutoff::Before(now_seconds() - days * 86_400));
    }
    crate::internal::log::date_parser::parse_date(trimmed)
        .ok()
        .map(ExpireCutoff::Before)
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        // INVARIANT: the system clock is after the Unix epoch on any supported host.
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Collect the set of commit OIDs reachable from `tips`, using the injected
/// `load_parents` (OID → its parent OIDs). Iterative (explicit stack + visited
/// set) so arbitrarily long chains never overflow the stack and synthetic
/// cycles terminate.
pub fn collect_reachable<F>(tips: &[String], mut load_parents: F) -> HashSet<String>
where
    F: FnMut(&str) -> Option<Vec<String>>,
{
    let mut visited = HashSet::new();
    let mut stack: Vec<String> = tips.iter().filter(|t| !t.is_empty()).cloned().collect();
    while let Some(oid) = stack.pop() {
        if !visited.insert(oid.clone()) {
            continue;
        }
        if let Some(parents) = load_parents(&oid) {
            for parent in parents {
                if !visited.contains(&parent) {
                    stack.push(parent);
                }
            }
        }
    }
    visited
}

/// Expire a single ref's reflog inside the caller's connection/transaction.
///
/// `load_parents` resolves a commit OID to its parent OIDs (for reachability)
/// and `is_commit` reports whether an OID loads as a commit (for `--stale-fix`);
/// both are injected so this layer stays free of the object store and is
/// unit-testable with synthetic graphs. Honors `dry_run` (computes the plan but
/// performs no writes).
pub async fn expire_reflog_with_conn<C, LP, LC>(
    conn: &C,
    ref_name: &str,
    options: &ExpireOptions,
    mut load_parents: LP,
    is_commit: LC,
) -> Result<ExpireResult, ReflogError>
where
    C: ConnectionTrait,
    LP: FnMut(&str) -> Option<Vec<String>>,
    LC: Fn(&str) -> bool,
{
    // Entries are returned newest-first (timestamp DESC).
    let entries = Reflog::find_all(conn, ref_name).await?;
    let mut result = ExpireResult::empty(ref_name.to_string());
    result.scanned = entries.len();
    if entries.is_empty() {
        return Ok(result);
    }

    // Reachability is computed from the ref's current tip (newest new_oid).
    let reachable = if matches!(options.expire_unreachable, ExpireCutoff::Never) {
        HashSet::new()
    } else {
        let tip = entries[0].new_oid.clone();
        collect_reachable(&[tip], &mut load_parents)
    };

    // Decide which entries to prune. Precedence: stale > expired > unreachable.
    let mut doomed: Vec<(usize, &Model, PruneReason)> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let reason = if options.stale_fix && !is_commit(&entry.new_oid) {
            Some(PruneReason::Stale)
        } else if is_expired(options.expire, entry.timestamp) {
            Some(PruneReason::Expired)
        } else if is_expired(options.expire_unreachable, entry.timestamp)
            && !reachable.contains(&entry.new_oid)
        {
            Some(PruneReason::Unreachable)
        } else {
            None
        };
        if let Some(reason) = reason {
            doomed.push((index, entry, reason));
        }
    }

    result.pruned = doomed.len();
    result.pruned_entries = doomed
        .iter()
        .map(|(index, entry, reason)| PrunedEntry {
            index: *index,
            old_oid: entry.old_oid.clone(),
            new_oid: entry.new_oid.clone(),
            reason: *reason,
        })
        .collect();

    if options.dry_run || doomed.is_empty() {
        return Ok(result);
    }

    let doomed_ids: HashSet<i64> = doomed.iter().map(|(_, entry, _)| entry.id).collect();

    // `--rewrite`: keep the surviving chain continuous. Each surviving entry's
    // `old_oid` becomes the `new_oid` of the next-older surviving entry (entries
    // are newest-first, so "next older" is the following surviving index).
    if options.rewrite {
        let survivors: Vec<&Model> = entries
            .iter()
            .filter(|entry| !doomed_ids.contains(&entry.id))
            .collect();
        for window in survivors.windows(2) {
            let (newer, older) = (window[0], window[1]);
            if newer.old_oid != older.new_oid {
                conn.execute(Statement::from_sql_and_values(
                    DbBackend::Sqlite,
                    "UPDATE reflog SET old_oid = ? WHERE id = ?;",
                    [older.new_oid.clone().into(), newer.id.into()],
                ))
                .await?;
                result.rewritten += 1;
            }
        }
    }

    for entry in &entries {
        if doomed_ids.contains(&entry.id) {
            conn.execute(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "DELETE FROM reflog WHERE id = ?;",
                [entry.id.into()],
            ))
            .await?;
        }
    }

    // `--updateref`: move a local branch tip to the newest surviving entry.
    // Symbolic HEAD and remote-tracking refs are intentionally skipped (Git
    // ignores `--updateref` for symbolic references).
    if options.updateref
        && let Some(branch) = ref_name.strip_prefix("refs/heads/")
        && let Some(newest_survivor) = entries.iter().find(|entry| !doomed_ids.contains(&entry.id))
    {
        crate::internal::branch::Branch::update_branch_with_conn(
            conn,
            branch,
            &newest_survivor.new_oid,
            None,
        )
        .await
        .map_err(|e| ReflogError::Config(format!("failed to update branch '{branch}': {e}")))?;
    }

    Ok(result)
}

/// Transaction-opening wrapper around [`expire_reflog_with_conn`]: the whole
/// per-ref prune/rewrite/updateref runs in one transaction and rolls back on
/// any failure.
pub async fn expire_reflog<C, LP, LC>(
    db: &C,
    ref_name: &str,
    options: &ExpireOptions,
    load_parents: LP,
    is_commit: LC,
) -> Result<ExpireResult, ReflogError>
where
    C: TransactionTrait,
    LP: FnMut(&str) -> Option<Vec<String>> + Send + 'static,
    LC: Fn(&str) -> bool + Send + 'static,
{
    let ref_name = ref_name.to_string();
    let options = options.clone();
    let result = db
        .transaction(move |txn| {
            Box::pin(async move {
                expire_reflog_with_conn(txn, &ref_name, &options, load_parents, is_commit)
                    .await
                    .map_err(|e| DbErr::Custom(e.to_string()))
            })
        })
        .await?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use sea_orm::{DbErr, TransactionError};

    use super::ReflogError;

    #[test]
    fn reflog_error_display_pins_static_messages() {
        assert_eq!(
            ReflogError::DatabaseError(DbErr::Custom("ignored".to_string())).to_string(),
            "failed to access reflog storage",
        );
        assert_eq!(
            ReflogError::TransactionError(TransactionError::Transaction(DbErr::Custom(
                "ignored".to_string()
            )))
            .to_string(),
            "failed to update reflog",
        );
    }

    #[test]
    fn reflog_error_display_config_variant() {
        assert_eq!(
            ReflogError::Config("gc.reflogExpire='x' is not a valid expire value".to_string())
                .to_string(),
            "invalid reflog expire config: gc.reflogExpire='x' is not a valid expire value",
        );
    }

    #[test]
    fn is_expired_matches_cutoff_semantics() {
        use super::{ExpireCutoff, is_expired};
        assert!(!is_expired(ExpireCutoff::Never, 0));
        assert!(!is_expired(ExpireCutoff::Never, i64::MAX));
        assert!(is_expired(ExpireCutoff::All, 0));
        assert!(is_expired(ExpireCutoff::All, i64::MAX));
        assert!(is_expired(ExpireCutoff::Before(100), 99));
        assert!(!is_expired(ExpireCutoff::Before(100), 100)); // boundary: == cutoff is kept
        assert!(!is_expired(ExpireCutoff::Before(100), 101));
    }

    #[test]
    fn parse_expire_cutoff_special_tokens_and_days() {
        use super::{ExpireCutoff, parse_expire_cutoff};
        assert_eq!(parse_expire_cutoff("never"), Some(ExpireCutoff::Never));
        assert_eq!(parse_expire_cutoff("NEVER"), Some(ExpireCutoff::Never));
        assert_eq!(parse_expire_cutoff("all"), Some(ExpireCutoff::All));
        assert!(matches!(
            parse_expire_cutoff("now"),
            Some(ExpireCutoff::Before(_))
        ));
        // A bare number is a count of days back from now.
        assert!(matches!(
            parse_expire_cutoff("90"),
            Some(ExpireCutoff::Before(_))
        ));
        assert!(matches!(
            parse_expire_cutoff("10 days ago"),
            Some(ExpireCutoff::Before(_))
        ));
        assert_eq!(parse_expire_cutoff("not-a-date"), None);
    }

    #[test]
    fn collect_reachable_includes_all_parents_and_terminates_on_cycle() {
        use std::collections::HashMap;

        use super::collect_reachable;

        // Merge graph: m -> {a, b}, a -> base, b -> base, base -> {}.
        let mut graph: HashMap<&str, Vec<String>> = HashMap::new();
        graph.insert("m", vec!["a".to_string(), "b".to_string()]);
        graph.insert("a", vec!["base".to_string()]);
        graph.insert("b", vec!["base".to_string()]);
        graph.insert("base", vec![]);
        let reachable = collect_reachable(&["m".to_string()], |oid| graph.get(oid).cloned());
        for expected in ["m", "a", "b", "base"] {
            assert!(reachable.contains(expected), "missing {expected}");
        }

        // A synthetic cycle x -> y -> x must terminate (only true via injected loader).
        let mut cyclic: HashMap<&str, Vec<String>> = HashMap::new();
        cyclic.insert("x", vec!["y".to_string()]);
        cyclic.insert("y", vec!["x".to_string()]);
        let visited = collect_reachable(&["x".to_string()], |oid| cyclic.get(oid).cloned());
        assert_eq!(visited.len(), 2);
    }
}
