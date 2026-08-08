//! Thread projection types for the Libra runtime layer.
//!
//! These records do not replace immutable `Intent` history. They materialize
//! the current conversational view over an Intent DAG so Libra can resume the
//! active branch, render participants, and track thread-local metadata without
//! rewriting snapshot objects.
//!
//! SQLite persistence stores projection timestamps at second granularity.
//! Sub-second precision is intentionally discarded because these rows are
//! rebuildable current-state views rather than immutable audit facts.
//!
//! Projection rows are optional runtime caches. Callers that receive `Ok(None)`
//! for a thread lookup should treat that as "projection missing" and attempt a
//! rebuild from immutable history rather than assuming the thread does not
//! exist.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use git_internal::internal::object::types::{ActorKind, ActorRef};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseConnection,
    EntityTrait, QueryFilter, QueryOrder, QuerySelect, TransactionTrait, sea_query::Expr,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::internal::model::{ai_thread, ai_thread_intent, ai_thread_participant};

/// Libra-side identifier for a projected conversation thread.
pub type ThreadId = Uuid;

/// Current conversational projection over a related Intent DAG.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThreadProjection {
    /// Stable projection key for the thread inside Libra storage.
    pub thread_id: ThreadId,
    /// Human-readable thread title shown in UI surfaces when available.
    pub title: Option<String>,
    /// Actor that owns or initiated the thread.
    pub owner: ActorRef,
    /// Human and agent members currently attached to the thread projection.
    #[serde(default)]
    pub participants: Vec<ThreadParticipant>,
    /// Intent currently focused by the UI / scheduler within the thread.
    pub current_intent_id: Option<Uuid>,
    /// Most recently linked Intent revision in the thread, used as the default
    /// resume fallback when no explicit current intent is selected.
    pub latest_intent_id: Option<Uuid>,
    /// Ordered Intent membership view, including branch-head markers.
    #[serde(default)]
    pub intents: Vec<ThreadIntentRef>,
    /// Optional projection-only routing or presentation hints.
    pub metadata: Option<Value>,
    /// Whether the thread is closed for normal mutation in the runtime/UI.
    pub archived: bool,
    /// Projection creation time.
    pub created_at: DateTime<Utc>,
    /// Last time Libra updated this projection row.
    pub updated_at: DateTime<Utc>,
    /// Projection revision maintained by Libra for updates and rebuilds.
    pub version: i64,
}

/// Actor membership in a thread projection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThreadParticipant {
    /// Actor included in the thread membership list.
    pub actor: ActorRef,
    /// Thread-local role used for routing, permissions, or presentation.
    pub role: ThreadParticipantRole,
    /// Time at which the actor joined the projected thread, stored at second
    /// granularity in SQLite.
    pub joined_at: DateTime<Utc>,
}

/// Intent membership state within a thread projection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThreadIntentRef {
    /// Intent revision that belongs to this thread's DAG view.
    pub intent_id: Uuid,
    /// Stable display / traversal order for Intents within the thread.
    pub ordinal: i64,
    /// Whether this Intent is currently a branch head in the projected DAG.
    pub is_head: bool,
    /// Time at which the Intent was attached to this thread, stored at second
    /// granularity in SQLite.
    pub linked_at: DateTime<Utc>,
    /// Why Libra linked this Intent into the thread projection.
    pub link_reason: ThreadIntentLinkReason,
}

/// Role of an actor inside a projected thread.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThreadParticipantRole {
    /// Primary owner or initiator of the thread.
    Owner,
    /// Regular participant who can contribute to the thread.
    Member,
    /// Reviewer focused on validation or approval work.
    Reviewer,
    /// Read-mostly observer included for visibility.
    Observer,
}

/// Reason an Intent revision became part of a projected thread.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThreadIntentLinkReason {
    /// Initial Intent that seeded the thread.
    Seed,
    /// A new Intent revision linked from an existing thread branch.
    Revision,
    /// A branch split created an additional line of work in the thread.
    Split,
    /// Multiple branches were joined back into one conversational view.
    Merge,
    /// A follow-up request was attached to continue the thread.
    Followup,
    /// Existing history was imported into Libra from another source.
    Imported,
}

impl ThreadProjection {
    /// Persist a new thread projection and all child rows to the database.
    ///
    /// Call this when Libra materializes a thread for the first time, such as
    /// after projecting a newly discovered Intent DAG into runtime state.
    /// This method owns its transaction, so it is appropriate for one-shot
    /// writes from command handlers, rebuild jobs, or projectors that are not
    /// already inside a broader database transaction.
    ///
    /// When the caller already has an open transaction that should also cover
    /// other projection writes, prefer [`ThreadProjection::create_with_conn`]
    /// rather than opening a second transaction at this boundary.
    ///
    /// ```no_run
    /// use chrono::Utc;
    /// use git_internal::internal::object::types::ActorRef;
    /// use libra::internal::ai::projection::{
    ///     ThreadIntentLinkReason, ThreadIntentRef, ThreadParticipant,
    ///     ThreadParticipantRole, ThreadProjection,
    /// };
    /// use sea_orm::Database;
    /// use uuid::Uuid;
    ///
    /// # fn actor(id: &str, display_name: Option<&str>) -> ActorRef {
    /// #     let mut actor = ActorRef::human(id).unwrap();
    /// #     actor.set_display_name(display_name.map(str::to_owned));
    /// #     actor
    /// # }
    /// # async fn docs() -> anyhow::Result<()> {
    /// let db = Database::connect("sqlite::memory:").await?;
    /// let now = Utc::now();
    ///
    /// let projection = ThreadProjection {
    ///     thread_id: Uuid::new_v4(),
    ///     title: Some("Planner thread".to_string()),
    ///     owner: actor("user-1", Some("Alice")),
    ///     participants: vec![ThreadParticipant {
    ///         actor: actor("user-1", Some("Alice")),
    ///         role: ThreadParticipantRole::Owner,
    ///         joined_at: now,
    ///     }],
    ///     current_intent_id: None,
    ///     latest_intent_id: None,
    ///     intents: vec![ThreadIntentRef {
    ///         intent_id: Uuid::new_v4(),
    ///         ordinal: 0,
    ///         is_head: true,
    ///         linked_at: now,
    ///         link_reason: ThreadIntentLinkReason::Seed,
    ///     }],
    ///     metadata: None,
    ///     archived: false,
    ///     created_at: now,
    ///     updated_at: now,
    ///     version: 1,
    /// };
    ///
    /// projection.create(&db).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn create(&self, db: &DatabaseConnection) -> Result<()> {
        let txn = db
            .begin()
            .await
            .context("Failed to start transaction for thread projection create")?;

        if let Err(err) = self.create_with_conn(&txn).await {
            if let Err(rollback_err) = txn.rollback().await {
                return Err(anyhow::Error::new(rollback_err).context(format!(
                    "Failed to rollback thread projection create for {} after: {err:#}",
                    self.thread_id
                )));
            }
            return Err(err);
        }

        txn.commit().await.with_context(|| {
            format!(
                "Failed to commit thread projection create for {}",
                self.thread_id
            )
        })?;
        Ok(())
    }

    /// Persist a new thread projection using an existing connection or
    /// transaction.
    ///
    /// Use this variant when the caller already owns a broader transaction that
    /// should also cover scheduler rows or query-index updates.
    ///
    /// ```no_run
    /// use chrono::Utc;
    /// use git_internal::internal::object::types::ActorRef;
    /// use libra::internal::ai::projection::{
    ///     ThreadIntentLinkReason, ThreadIntentRef, ThreadParticipant,
    ///     ThreadParticipantRole, ThreadProjection,
    /// };
    /// use sea_orm::{Database, TransactionTrait};
    /// use uuid::Uuid;
    ///
    /// # fn actor(id: &str, display_name: Option<&str>) -> ActorRef {
    /// #     let mut actor = ActorRef::human(id).unwrap();
    /// #     actor.set_display_name(display_name.map(str::to_owned));
    /// #     actor
    /// # }
    /// # async fn docs() -> anyhow::Result<()> {
    /// let db = Database::connect("sqlite::memory:").await?;
    /// let now = Utc::now();
    /// let txn = db.begin().await?;
    ///
    /// let projection = ThreadProjection {
    ///     thread_id: Uuid::new_v4(),
    ///     title: Some("Planner thread".to_string()),
    ///     owner: actor("user-1", Some("Alice")),
    ///     participants: vec![ThreadParticipant {
    ///         actor: actor("user-1", Some("Alice")),
    ///         role: ThreadParticipantRole::Owner,
    ///         joined_at: now,
    ///     }],
    ///     current_intent_id: None,
    ///     latest_intent_id: None,
    ///     intents: vec![ThreadIntentRef {
    ///         intent_id: Uuid::new_v4(),
    ///         ordinal: 0,
    ///         is_head: true,
    ///         linked_at: now,
    ///         link_reason: ThreadIntentLinkReason::Seed,
    ///     }],
    ///     metadata: None,
    ///     archived: false,
    ///     created_at: now,
    ///     updated_at: now,
    ///     version: 1,
    /// };
    ///
    /// projection.create_with_conn(&txn).await?;
    /// txn.commit().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn create_with_conn<C: ConnectionTrait>(&self, db: &C) -> Result<()> {
        self.insert_projection(db).await
    }

    /// Update an existing thread projection and replace its child rows.
    ///
    /// Call this after Libra advances mutable thread state, such as switching
    /// the active Intent, updating thread-local metadata, or replacing the
    /// projected participant / intent membership lists.
    ///
    /// The caller must preserve immutable fields (`owner` identity and
    /// `created_at`) and increment `version` by exactly one. The method performs
    /// an optimistic compare-and-swap update so concurrent writers fail instead
    /// of silently overwriting each other. Once a thread is already archived in
    /// storage, further updates are rejected; use a lower-level rebuild / repair
    /// path if a future migration needs to rewrite archived rows.
    ///
    /// ```no_run
    /// use chrono::Utc;
    /// use libra::internal::ai::projection::ThreadProjection;
    /// use sea_orm::Database;
    /// use uuid::Uuid;
    ///
    /// # async fn docs() -> anyhow::Result<()> {
    /// let db = Database::connect("sqlite::memory:").await?;
    /// let thread_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111")?;
    ///
    /// if let Some(mut projection) = ThreadProjection::find_by_id(&db, thread_id).await? {
    ///     projection.title = Some("Release review".to_string());
    ///     projection.updated_at = Utc::now();
    ///     projection.version += 1;
    ///     projection.update(&db).await?;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn update(&self, db: &DatabaseConnection) -> Result<()> {
        let txn = crate::internal::db::begin_write_transaction(db)
            .await
            .context("Failed to start transaction for thread projection update")?;

        if let Err(err) = self.update_with_conn(&txn).await {
            if let Err(rollback_err) = txn.rollback().await {
                return Err(anyhow::Error::new(rollback_err).context(format!(
                    "Failed to rollback thread projection update for {} after: {err:#}",
                    self.thread_id
                )));
            }
            return Err(err);
        }

        txn.commit().await.with_context(|| {
            format!(
                "Failed to commit thread projection update for {}",
                self.thread_id
            )
        })?;
        Ok(())
    }

    /// Update a thread projection inside an existing connection or transaction.
    ///
    /// Use this variant when Thread, Scheduler, and index projections must be
    /// advanced as one atomic unit.
    ///
    /// ```no_run
    /// use chrono::Utc;
    /// use libra::internal::ai::projection::ThreadProjection;
    /// use sea_orm::{Database, TransactionTrait};
    /// use uuid::Uuid;
    ///
    /// # async fn docs() -> anyhow::Result<()> {
    /// let db = Database::connect("sqlite::memory:").await?;
    /// let thread_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111")?;
    /// let txn = db.begin().await?;
    ///
    /// if let Some(mut projection) = ThreadProjection::find_by_id_with_conn(&txn, thread_id).await? {
    ///     projection.title = Some("Release review".to_string());
    ///     projection.updated_at = Utc::now();
    ///     projection.version += 1;
    ///     projection.update_with_conn(&txn).await?;
    /// }
    ///
    /// txn.commit().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn update_with_conn<C: ConnectionTrait>(&self, db: &C) -> Result<()> {
        self.update_projection(db).await
    }

    /// Load a thread projection and its child rows from the database.
    ///
    /// This method opens a read transaction so the parent row and child rows
    /// are reconstructed from one consistent snapshot.
    ///
    /// Use this entry point when a command, UI surface, or API handler needs to
    /// read a thread by id and is not already operating inside a database
    /// transaction.
    ///
    /// ```no_run
    /// use libra::internal::ai::projection::ThreadProjection;
    /// use sea_orm::Database;
    /// use uuid::Uuid;
    ///
    /// # async fn docs() -> anyhow::Result<()> {
    /// let db = Database::connect("sqlite::memory:").await?;
    /// let thread_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111")?;
    ///
    /// if let Some(projection) = ThreadProjection::find_by_id(&db, thread_id).await? {
    ///     assert_eq!(projection.thread_id, thread_id);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn find_by_id(db: &DatabaseConnection, thread_id: ThreadId) -> Result<Option<Self>> {
        let txn = db
            .begin()
            .await
            .context("Failed to start transaction for thread projection read")?;

        let projection = Self::find_by_id_with_conn(&txn, thread_id).await;
        match projection {
            Ok(projection) => {
                txn.commit().await.with_context(|| {
                    format!("Failed to commit thread projection read for {}", thread_id)
                })?;
                Ok(projection)
            }
            Err(err) => {
                if let Err(rollback_err) = txn.rollback().await {
                    return Err(anyhow::Error::new(rollback_err).context(format!(
                        "Failed to rollback thread projection read for {} after: {err:#}",
                        thread_id
                    )));
                }
                Err(err)
            }
        }
    }

    /// List active, non-archived threads ordered by most recent update time.
    ///
    /// Use this for UI/TUI entry points that need to render a thread picker or
    /// resume list.
    ///
    /// ```no_run
    /// use libra::internal::ai::projection::ThreadProjection;
    /// use sea_orm::Database;
    ///
    /// # async fn docs() -> anyhow::Result<()> {
    /// let db = Database::connect("sqlite::memory:").await?;
    /// let threads = ThreadProjection::list_active(&db, 20, 0).await?;
    /// let _ = threads;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn list_active(
        db: &DatabaseConnection,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<Self>> {
        let txn = db
            .begin()
            .await
            .context("Failed to start transaction for thread projection list_active")?;

        let projections = Self::list_active_with_conn(&txn, limit, offset).await;
        match projections {
            Ok(projections) => {
                txn.commit()
                    .await
                    .context("Failed to commit thread projection list_active transaction")?;
                Ok(projections)
            }
            Err(err) => {
                if let Err(rollback_err) = txn.rollback().await {
                    return Err(anyhow::Error::new(rollback_err).context(format!(
                        "Failed to rollback thread projection list_active transaction after: {err:#}"
                    )));
                }
                Err(err)
            }
        }
    }

    /// List active, non-archived threads using an existing connection or
    /// transaction.
    pub async fn list_active_with_conn<C: ConnectionTrait>(
        db: &C,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<Self>> {
        let rows = ai_thread::Entity::find()
            .filter(ai_thread::Column::Archived.eq(false))
            .order_by_desc(ai_thread::Column::UpdatedAt)
            .order_by_desc(ai_thread::Column::ThreadId)
            .limit(limit)
            .offset(offset)
            .all(db)
            .await
            .context("Failed to query active thread projections")?;

        let mut projections = Vec::with_capacity(rows.len());
        // This is intentionally an N+1 read path today: one query for the
        // active thread ids, then one full reconstruction per thread. That is
        // acceptable for the current small UI/TUI working set, but should move
        // to a batched loader if active thread counts grow materially.
        // TODO(perf): switch to `find_by_ids_with_conn` batching before active
        // thread lists regularly exceed low double digits (roughly 20-50 rows).
        for row in rows {
            let thread_id = Uuid::parse_str(&row.thread_id).with_context(|| {
                format!(
                    "Invalid thread_id in active thread projection row: {}",
                    row.thread_id
                )
            })?;
            if let Some(projection) = Self::find_by_id_with_conn(db, thread_id).await? {
                projections.push(projection);
            }
        }

        Ok(projections)
    }

    /// Resolve the thread projection that owns a specific Intent.
    ///
    /// Use this when the caller only has an immutable Intent id and needs to
    /// recover the current Libra-side thread view.
    ///
    /// ```no_run
    /// use libra::internal::ai::projection::ThreadProjection;
    /// use sea_orm::Database;
    /// use uuid::Uuid;
    ///
    /// # async fn docs() -> anyhow::Result<()> {
    /// let db = Database::connect("sqlite::memory:").await?;
    /// let intent_id = Uuid::parse_str("22222222-2222-2222-2222-222222222222")?;
    /// let thread = ThreadProjection::find_by_intent_id(&db, intent_id).await?;
    /// let _ = thread;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn find_by_intent_id(
        db: &DatabaseConnection,
        intent_id: Uuid,
    ) -> Result<Option<Self>> {
        let txn = db
            .begin()
            .await
            .context("Failed to start transaction for thread projection intent lookup")?;

        let projection = Self::find_by_intent_id_with_conn(&txn, intent_id).await;
        match projection {
            Ok(projection) => {
                txn.commit()
                    .await
                    .context("Failed to commit thread projection intent lookup transaction")?;
                Ok(projection)
            }
            Err(err) => {
                if let Err(rollback_err) = txn.rollback().await {
                    return Err(anyhow::Error::new(rollback_err).context(format!(
                        "Failed to rollback thread projection intent lookup transaction after: {err:#}"
                    )));
                }
                Err(err)
            }
        }
    }

    /// Resolve the thread projection for an Intent using an existing
    /// connection or transaction.
    pub async fn find_by_intent_id_with_conn<C: ConnectionTrait>(
        db: &C,
        intent_id: Uuid,
    ) -> Result<Option<Self>> {
        let rows = ai_thread_intent::Entity::find()
            .filter(ai_thread_intent::Column::IntentId.eq(intent_id.to_string()))
            .all(db)
            .await
            .with_context(|| format!("Failed to query thread projection for intent {intent_id}"))?;

        match rows.as_slice() {
            [] => Ok(None),
            [row] => {
                let thread_id = Uuid::parse_str(&row.thread_id).with_context(|| {
                    format!(
                        "Invalid thread_id in thread intent row for intent {}: {}",
                        intent_id, row.thread_id
                    )
                })?;
                Self::find_by_id_with_conn(db, thread_id).await
            }
            _ => bail!(
                "Intent {} is linked to multiple thread projections; mapping is ambiguous",
                intent_id
            ),
        }
    }

    /// Load a thread projection from an existing connection or transaction.
    ///
    /// Use this variant when the caller already owns a transaction and wants
    /// the thread read to participate in that broader unit of work. Typical
    /// call sites include projection rebuilders and workflows that read a
    /// thread, then update scheduler rows or reverse indexes before committing.
    ///
    /// This avoids opening a second connection and keeps the read on the same
    /// snapshot as the surrounding transaction.
    ///
    /// ```no_run
    /// use libra::internal::ai::projection::ThreadProjection;
    /// use sea_orm::{Database, TransactionTrait};
    /// use uuid::Uuid;
    ///
    /// # async fn docs() -> anyhow::Result<()> {
    /// let db = Database::connect("sqlite::memory:").await?;
    /// let thread_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111")?;
    ///
    /// let txn = db.begin().await?;
    /// let projection = ThreadProjection::find_by_id_with_conn(&txn, thread_id).await?;
    /// // ... update related scheduler / index rows inside the same transaction ...
    /// txn.commit().await?;
    ///
    /// let _ = projection;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn find_by_id_with_conn<C: ConnectionTrait>(
        db: &C,
        thread_id: ThreadId,
    ) -> Result<Option<Self>> {
        let thread_id_text = thread_id.to_string();
        let model = ai_thread::Entity::find_by_id(thread_id_text.clone())
            .one(db)
            .await
            .with_context(|| format!("Failed to query thread projection {thread_id}"))?;

        let Some(model) = model else {
            return Ok(None);
        };

        let participants = ai_thread_participant::Entity::find()
            .filter(ai_thread_participant::Column::ThreadId.eq(thread_id_text.clone()))
            .order_by_asc(ai_thread_participant::Column::JoinedAt)
            .order_by_asc(ai_thread_participant::Column::ActorKind)
            .order_by_asc(ai_thread_participant::Column::ActorId)
            .all(db)
            .await
            .with_context(|| {
                format!("Failed to query participants for thread projection {thread_id}")
            })?
            .into_iter()
            .map(|row| thread_participant_from_model(thread_id, row))
            .collect::<Result<Vec<_>>>()?;

        let intents = ai_thread_intent::Entity::find()
            .filter(ai_thread_intent::Column::ThreadId.eq(thread_id_text))
            .order_by_asc(ai_thread_intent::Column::Ordinal)
            .order_by_asc(ai_thread_intent::Column::LinkedAt)
            .order_by_asc(ai_thread_intent::Column::IntentId)
            .all(db)
            .await
            .with_context(|| format!("Failed to query intents for thread projection {thread_id}"))?
            .into_iter()
            .map(|row| thread_intent_from_model(thread_id, row))
            .collect::<Result<Vec<_>>>()?;

        Ok(Some(ThreadProjection {
            thread_id,
            title: model.title,
            owner: actor_from_row(
                &model.owner_kind,
                &model.owner_id,
                model.owner_display_name,
                thread_id,
                "owner",
            )?,
            participants,
            current_intent_id: optional_uuid_from_row(
                model.current_intent_id.as_deref(),
                thread_id,
                "current_intent_id",
            )?,
            latest_intent_id: optional_uuid_from_row(
                model.latest_intent_id.as_deref(),
                thread_id,
                "latest_intent_id",
            )?,
            intents,
            metadata: metadata_from_row(model.metadata_json.as_deref(), thread_id)?,
            archived: model.archived,
            created_at: datetime_from_row(model.created_at, thread_id, "created_at")?,
            updated_at: datetime_from_row(model.updated_at, thread_id, "updated_at")?,
            version: model.version,
        }))
    }

    async fn insert_projection<C: ConnectionTrait>(&self, db: &C) -> Result<()> {
        let active_model = ai_thread::ActiveModel {
            thread_id: Set(self.thread_id.to_string()),
            title: Set(self.title.clone()),
            owner_kind: Set(self.owner.kind().to_string()),
            owner_id: Set(self.owner.id().to_string()),
            owner_display_name: Set(self.owner.display_name().map(str::to_owned)),
            current_intent_id: Set(self.current_intent_id.as_ref().map(Uuid::to_string)),
            latest_intent_id: Set(self.latest_intent_id.as_ref().map(Uuid::to_string)),
            metadata_json: Set(metadata_to_row(self.metadata.as_ref(), self.thread_id)?),
            archived: Set(self.archived),
            version: Set(self.version),
            created_at: Set(self.created_at.timestamp()),
            updated_at: Set(self.updated_at.timestamp()),
        };

        active_model.insert(db).await.with_context(|| {
            format!(
                "Failed to insert thread projection row for {}",
                self.thread_id
            )
        })?;

        self.replace_related_rows(db).await
    }

    async fn update_projection<C: ConnectionTrait>(&self, db: &C) -> Result<()> {
        let Some(model) = ai_thread::Entity::find_by_id(self.thread_id.to_string())
            .one(db)
            .await
            .with_context(|| {
                format!(
                    "Failed to query thread projection {} for update",
                    self.thread_id
                )
            })?
        else {
            bail!("Thread projection {} does not exist", self.thread_id);
        };

        if model.archived {
            bail!(
                "Thread projection {} is archived and cannot be updated",
                self.thread_id
            );
        }

        if model.owner_kind != self.owner.kind().to_string() || model.owner_id != self.owner.id() {
            bail!(
                "Thread projection {} owner identity is immutable",
                self.thread_id
            );
        }

        if self.created_at.timestamp() != model.created_at {
            bail!(
                "Thread projection {} created_at is immutable",
                self.thread_id
            );
        }

        let expected_version = model.version + 1;
        if self.version != expected_version {
            bail!(
                "Thread projection {} update expects version {}, got {}",
                self.thread_id,
                expected_version,
                self.version
            );
        }

        let result = ai_thread::Entity::update_many()
            .filter(ai_thread::Column::ThreadId.eq(self.thread_id.to_string()))
            .filter(ai_thread::Column::Version.eq(model.version))
            .col_expr(ai_thread::Column::Title, Expr::value(self.title.clone()))
            .col_expr(
                ai_thread::Column::OwnerDisplayName,
                Expr::value(self.owner.display_name().map(str::to_owned)),
            )
            .col_expr(
                ai_thread::Column::CurrentIntentId,
                Expr::value(self.current_intent_id.as_ref().map(Uuid::to_string)),
            )
            .col_expr(
                ai_thread::Column::LatestIntentId,
                Expr::value(self.latest_intent_id.as_ref().map(Uuid::to_string)),
            )
            .col_expr(
                ai_thread::Column::MetadataJson,
                Expr::value(metadata_to_row(self.metadata.as_ref(), self.thread_id)?),
            )
            .col_expr(ai_thread::Column::Archived, Expr::value(self.archived))
            .col_expr(
                ai_thread::Column::UpdatedAt,
                Expr::value(self.updated_at.timestamp()),
            )
            .col_expr(ai_thread::Column::Version, Expr::value(self.version))
            .exec(db)
            .await
            .with_context(|| {
                format!(
                    "Failed to update thread projection row for {}",
                    self.thread_id
                )
            })?;

        if result.rows_affected != 1 {
            let latest_version = ai_thread::Entity::find_by_id(self.thread_id.to_string())
                .one(db)
                .await
                .with_context(|| {
                    format!(
                        "Failed to reload thread projection {} after optimistic-lock conflict",
                        self.thread_id
                    )
                })?
                .map(|row| row.version);
            bail!(
                "Thread projection {} was modified concurrently; stored version changed from {} to {:?} while caller attempted version {}",
                self.thread_id,
                model.version,
                latest_version,
                self.version
            );
        }

        self.replace_related_rows(db).await
    }

    async fn replace_related_rows<C: ConnectionTrait>(&self, db: &C) -> Result<()> {
        let thread_id_text = self.thread_id.to_string();

        // Thread projections are expected to have small participant / intent
        // sets, so full replacement keeps write logic simple and deterministic
        // under optimistic locking. If thread fan-out grows materially, replace
        // this with a diff-based upsert path.
        ai_thread_participant::Entity::delete_many()
            .filter(ai_thread_participant::Column::ThreadId.eq(thread_id_text.clone()))
            .exec(db)
            .await
            .with_context(|| {
                format!(
                    "Failed to clear participants for thread projection {}",
                    self.thread_id
                )
            })?;

        ai_thread_intent::Entity::delete_many()
            .filter(ai_thread_intent::Column::ThreadId.eq(thread_id_text.clone()))
            .exec(db)
            .await
            .with_context(|| {
                format!(
                    "Failed to clear intents for thread projection {}",
                    self.thread_id
                )
            })?;

        for participant in &self.participants {
            let active_model = ai_thread_participant::ActiveModel {
                thread_id: Set(thread_id_text.clone()),
                actor_kind: Set(participant.actor.kind().to_string()),
                actor_id: Set(participant.actor.id().to_string()),
                actor_display_name: Set(participant.actor.display_name().map(str::to_owned)),
                role: Set(thread_participant_role_to_row(&participant.role).to_string()),
                joined_at: Set(participant.joined_at.timestamp()),
            };

            active_model.insert(db).await.with_context(|| {
                format!(
                    "Failed to insert participant {} for thread projection {}",
                    participant.actor.id(),
                    self.thread_id
                )
            })?;
        }

        for intent in &self.intents {
            let active_model = ai_thread_intent::ActiveModel {
                thread_id: Set(thread_id_text.clone()),
                intent_id: Set(intent.intent_id.to_string()),
                ordinal: Set(intent.ordinal),
                is_head: Set(intent.is_head),
                linked_at: Set(intent.linked_at.timestamp()),
                link_reason: Set(thread_intent_link_reason_to_row(&intent.link_reason).to_string()),
            };

            active_model.insert(db).await.with_context(|| {
                format!(
                    "Failed to insert intent {} for thread projection {}",
                    intent.intent_id, self.thread_id
                )
            })?;
        }

        Ok(())
    }
}

fn actor_from_row(
    kind: &str,
    id: &str,
    display_name: Option<String>,
    thread_id: ThreadId,
    field_name: &str,
) -> Result<ActorRef> {
    let mut actor = ActorRef::new(ActorKind::from(kind.to_string()), id.to_string())
        .map_err(|err| anyhow::anyhow!(err))
        .with_context(|| {
            format!(
                "Invalid actor in {field_name} for thread projection {thread_id}: kind={kind}, id={id}"
            )
        })?;
    actor.set_display_name(display_name);
    Ok(actor)
}

fn optional_uuid_from_row(
    raw: Option<&str>,
    thread_id: ThreadId,
    field_name: &str,
) -> Result<Option<Uuid>> {
    raw.map(|value| {
        Uuid::parse_str(value).with_context(|| {
            format!("Invalid {field_name} value in thread projection {thread_id}: {value}")
        })
    })
    .transpose()
}

fn datetime_from_row(raw: i64, thread_id: ThreadId, field_name: &str) -> Result<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp(raw, 0).with_context(|| {
        format!("Invalid {field_name} timestamp in thread projection {thread_id}: {raw}")
    })
}

fn metadata_to_row(metadata: Option<&Value>, thread_id: ThreadId) -> Result<Option<String>> {
    metadata
        .map(|value| {
            serde_json::to_string(value).with_context(|| {
                format!("Failed to serialize metadata for thread projection {thread_id}")
            })
        })
        .transpose()
}

fn metadata_from_row(raw: Option<&str>, thread_id: ThreadId) -> Result<Option<Value>> {
    raw.map(|value| {
        serde_json::from_str(value)
            .with_context(|| format!("Failed to parse metadata for thread projection {thread_id}"))
    })
    .transpose()
}

fn thread_participant_from_model(
    thread_id: ThreadId,
    model: ai_thread_participant::Model,
) -> Result<ThreadParticipant> {
    Ok(ThreadParticipant {
        actor: actor_from_row(
            &model.actor_kind,
            &model.actor_id,
            model.actor_display_name,
            thread_id,
            "participant",
        )?,
        role: thread_participant_role_from_row(&model.role, thread_id)?,
        joined_at: datetime_from_row(model.joined_at, thread_id, "participant.joined_at")?,
    })
}

fn thread_intent_from_model(
    thread_id: ThreadId,
    model: ai_thread_intent::Model,
) -> Result<ThreadIntentRef> {
    Ok(ThreadIntentRef {
        intent_id: Uuid::parse_str(&model.intent_id).with_context(|| {
            format!(
                "Invalid intent_id in thread projection {}: {}",
                thread_id, model.intent_id
            )
        })?,
        ordinal: model.ordinal,
        is_head: model.is_head,
        linked_at: datetime_from_row(model.linked_at, thread_id, "intent.linked_at")?,
        link_reason: thread_intent_link_reason_from_row(&model.link_reason, thread_id)?,
    })
}

fn thread_participant_role_to_row(role: &ThreadParticipantRole) -> &'static str {
    // Keep the SQLite TEXT encoding aligned with serde's snake_case JSON form.
    match role {
        ThreadParticipantRole::Owner => "owner",
        ThreadParticipantRole::Member => "member",
        ThreadParticipantRole::Reviewer => "reviewer",
        ThreadParticipantRole::Observer => "observer",
    }
}

fn thread_participant_role_from_row(
    raw: &str,
    thread_id: ThreadId,
) -> Result<ThreadParticipantRole> {
    match raw {
        "owner" => Ok(ThreadParticipantRole::Owner),
        "member" => Ok(ThreadParticipantRole::Member),
        "reviewer" => Ok(ThreadParticipantRole::Reviewer),
        "observer" => Ok(ThreadParticipantRole::Observer),
        _ => bail!(
            "Invalid participant role in thread projection {}: {}",
            thread_id,
            raw
        ),
    }
}

fn thread_intent_link_reason_to_row(reason: &ThreadIntentLinkReason) -> &'static str {
    // Keep the SQLite TEXT encoding aligned with serde's snake_case JSON form.
    match reason {
        ThreadIntentLinkReason::Seed => "seed",
        ThreadIntentLinkReason::Revision => "revision",
        ThreadIntentLinkReason::Split => "split",
        ThreadIntentLinkReason::Merge => "merge",
        ThreadIntentLinkReason::Followup => "followup",
        ThreadIntentLinkReason::Imported => "imported",
    }
}

fn thread_intent_link_reason_from_row(
    raw: &str,
    thread_id: ThreadId,
) -> Result<ThreadIntentLinkReason> {
    match raw {
        "seed" => Ok(ThreadIntentLinkReason::Seed),
        "revision" => Ok(ThreadIntentLinkReason::Revision),
        "split" => Ok(ThreadIntentLinkReason::Split),
        "merge" => Ok(ThreadIntentLinkReason::Merge),
        "followup" => Ok(ThreadIntentLinkReason::Followup),
        "imported" => Ok(ThreadIntentLinkReason::Imported),
        _ => bail!(
            "Invalid intent link reason in thread projection {}: {}",
            thread_id,
            raw
        ),
    }
}

impl crate::internal::ai::runtime::snapshot::Snapshot for ThreadProjection {
    fn snapshot_kind(&self) -> &'static str {
        "thread_projection"
    }

    fn snapshot_id(&self) -> Uuid {
        // Projection identity is the thread; multiple projection versions for
        // the same thread share the same snapshot id and are distinguished
        // by `version` on the concrete type (CEX-00.5).
        self.thread_id
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::{Database, Statement, TransactionTrait};
    use serde_json::json;

    use super::*;

    const BOOTSTRAP_SQL: &str = include_str!("../../../../sql/sqlite_20260309_init.sql");

    async fn setup_test_db() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let backend = db.get_database_backend();
        db.execute_raw(Statement::from_string(backend, BOOTSTRAP_SQL))
            .await
            .unwrap();
        db
    }

    fn ts(seconds: i64) -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(seconds, 0).unwrap()
    }

    fn actor(kind: &str, id: &str, display_name: Option<&str>) -> ActorRef {
        let mut actor = ActorRef::new(kind, id.to_string()).unwrap();
        actor.set_display_name(display_name.map(str::to_owned));
        actor
    }

    fn sample_projection() -> ThreadProjection {
        ThreadProjection {
            thread_id: Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
            title: Some("Planner thread".to_string()),
            owner: actor("human", "user-1", Some("Alice")),
            participants: vec![
                ThreadParticipant {
                    actor: actor("human", "user-1", Some("Alice")),
                    role: ThreadParticipantRole::Owner,
                    joined_at: ts(1_700_000_000),
                },
                ThreadParticipant {
                    actor: actor("agent", "planner", Some("Planner")),
                    role: ThreadParticipantRole::Member,
                    joined_at: ts(1_700_000_010),
                },
            ],
            current_intent_id: Some(
                Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
            ),
            latest_intent_id: Some(
                Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
            ),
            intents: vec![
                ThreadIntentRef {
                    intent_id: Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
                    ordinal: 0,
                    is_head: false,
                    linked_at: ts(1_700_000_020),
                    link_reason: ThreadIntentLinkReason::Seed,
                },
                ThreadIntentRef {
                    intent_id: Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
                    ordinal: 1,
                    is_head: true,
                    linked_at: ts(1_700_000_030),
                    link_reason: ThreadIntentLinkReason::Revision,
                },
            ],
            metadata: Some(json!({
                "workspace": "repo-a",
                "resume": {
                    "mode": "auto"
                }
            })),
            archived: false,
            created_at: ts(1_700_000_040),
            updated_at: ts(1_700_000_050),
            version: 1,
        }
    }

    #[test]
    fn thread_projection_implements_snapshot_trait_with_stable_kind() {
        use crate::internal::ai::runtime::snapshot::Snapshot;
        let projection = sample_projection();
        let snapshot: &dyn Snapshot = &projection;
        assert_eq!(snapshot.snapshot_kind(), "thread_projection");
        assert_eq!(snapshot.snapshot_id(), projection.thread_id);
    }

    #[tokio::test]
    async fn thread_projection_create_persists_thread_and_children() {
        let db = setup_test_db().await;
        let projection = sample_projection();

        projection.create(&db).await.unwrap();

        let thread_row = ai_thread::Entity::find_by_id(projection.thread_id.to_string())
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(thread_row.title.as_deref(), Some("Planner thread"));
        assert_eq!(thread_row.owner_kind, "human");
        assert_eq!(thread_row.owner_display_name.as_deref(), Some("Alice"));

        let participant_rows = ai_thread_participant::Entity::find()
            .filter(ai_thread_participant::Column::ThreadId.eq(projection.thread_id.to_string()))
            .all(&db)
            .await
            .unwrap();
        assert_eq!(participant_rows.len(), 2);

        let intent_rows = ai_thread_intent::Entity::find()
            .filter(ai_thread_intent::Column::ThreadId.eq(projection.thread_id.to_string()))
            .all(&db)
            .await
            .unwrap();
        assert_eq!(intent_rows.len(), 2);
    }

    #[tokio::test]
    async fn thread_projection_create_with_conn_persists_with_existing_transaction() {
        let db = setup_test_db().await;
        let projection = sample_projection();

        let txn = db.begin().await.unwrap();
        projection.create_with_conn(&txn).await.unwrap();
        txn.commit().await.unwrap();

        let stored = ThreadProjection::find_by_id(&db, projection.thread_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored, projection);
    }

    #[tokio::test]
    async fn thread_projection_find_by_id_reconstructs_full_projection() {
        let db = setup_test_db().await;
        let projection = sample_projection();
        projection.create(&db).await.unwrap();

        let stored = ThreadProjection::find_by_id(&db, projection.thread_id)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(stored, projection);
    }

    #[tokio::test]
    async fn thread_projection_find_by_intent_id_returns_projection() {
        let db = setup_test_db().await;
        let projection = sample_projection();
        projection.create(&db).await.unwrap();

        let stored = ThreadProjection::find_by_intent_id(
            &db,
            Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(stored, projection);
    }

    #[tokio::test]
    async fn thread_projection_create_rejects_duplicate_intent_membership() {
        let db = setup_test_db().await;
        let first = sample_projection();
        first.create(&db).await.unwrap();

        let second = ThreadProjection {
            thread_id: Uuid::parse_str("66666666-6666-6666-6666-666666666666").unwrap(),
            current_intent_id: Some(
                Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
            ),
            latest_intent_id: Some(
                Uuid::parse_str("77777777-7777-7777-7777-777777777777").unwrap(),
            ),
            intents: vec![
                ThreadIntentRef {
                    intent_id: Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
                    ordinal: 0,
                    is_head: false,
                    linked_at: ts(1_700_000_220),
                    link_reason: ThreadIntentLinkReason::Seed,
                },
                ThreadIntentRef {
                    intent_id: Uuid::parse_str("77777777-7777-7777-7777-777777777777").unwrap(),
                    ordinal: 1,
                    is_head: true,
                    linked_at: ts(1_700_000_230),
                    link_reason: ThreadIntentLinkReason::Revision,
                },
            ],
            updated_at: ts(1_700_000_250),
            ..sample_projection()
        };

        let err = second.create(&db).await.unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("Failed to insert intent"));
        assert!(message.contains(&second.thread_id.to_string()));

        let stored = ThreadProjection::find_by_intent_id(
            &db,
            Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(stored.thread_id, first.thread_id);

        let second_row = ai_thread::Entity::find_by_id(second.thread_id.to_string())
            .one(&db)
            .await
            .unwrap();
        assert!(second_row.is_none());
    }

    #[tokio::test]
    async fn thread_projection_find_by_intent_id_rejects_ambiguous_mapping() {
        let db = setup_test_db().await;
        let first = sample_projection();
        first.create(&db).await.unwrap();

        let backend = db.get_database_backend();
        db.execute_raw(Statement::from_string(
            backend,
            "DROP INDEX uq_ai_thread_intent_intent".to_string(),
        ))
        .await
        .unwrap();

        let second = ThreadProjection {
            thread_id: Uuid::parse_str("66666666-6666-6666-6666-666666666666").unwrap(),
            current_intent_id: Some(
                Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
            ),
            latest_intent_id: Some(
                Uuid::parse_str("88888888-8888-8888-8888-888888888888").unwrap(),
            ),
            intents: vec![
                ThreadIntentRef {
                    intent_id: Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
                    ordinal: 0,
                    is_head: false,
                    linked_at: ts(1_700_000_220),
                    link_reason: ThreadIntentLinkReason::Imported,
                },
                ThreadIntentRef {
                    intent_id: Uuid::parse_str("88888888-8888-8888-8888-888888888888").unwrap(),
                    ordinal: 1,
                    is_head: true,
                    linked_at: ts(1_700_000_230),
                    link_reason: ThreadIntentLinkReason::Revision,
                },
            ],
            updated_at: ts(1_700_000_250),
            ..sample_projection()
        };
        second.create(&db).await.unwrap();

        let err = ThreadProjection::find_by_intent_id(
            &db,
            Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
        )
        .await
        .unwrap_err();
        assert!(format!("{err:#}").contains("mapping is ambiguous"));
    }

    #[tokio::test]
    async fn thread_projection_list_active_excludes_archived_and_sorts_by_updated_at() {
        let db = setup_test_db().await;

        let older = sample_projection();
        older.create(&db).await.unwrap();

        let newer = ThreadProjection {
            thread_id: Uuid::parse_str("66666666-6666-6666-6666-666666666666").unwrap(),
            current_intent_id: Some(
                Uuid::parse_str("77777777-7777-7777-7777-777777777777").unwrap(),
            ),
            latest_intent_id: Some(
                Uuid::parse_str("88888888-8888-8888-8888-888888888888").unwrap(),
            ),
            intents: vec![
                ThreadIntentRef {
                    intent_id: Uuid::parse_str("77777777-7777-7777-7777-777777777777").unwrap(),
                    ordinal: 0,
                    is_head: false,
                    linked_at: ts(1_700_000_220),
                    link_reason: ThreadIntentLinkReason::Seed,
                },
                ThreadIntentRef {
                    intent_id: Uuid::parse_str("88888888-8888-8888-8888-888888888888").unwrap(),
                    ordinal: 1,
                    is_head: true,
                    linked_at: ts(1_700_000_230),
                    link_reason: ThreadIntentLinkReason::Revision,
                },
            ],
            updated_at: ts(1_700_000_250),
            ..sample_projection()
        };
        newer.create(&db).await.unwrap();

        let archived = ThreadProjection {
            thread_id: Uuid::parse_str("99999999-9999-9999-9999-999999999999").unwrap(),
            current_intent_id: Some(
                Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap(),
            ),
            latest_intent_id: Some(
                Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap(),
            ),
            intents: vec![
                ThreadIntentRef {
                    intent_id: Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap(),
                    ordinal: 0,
                    is_head: false,
                    linked_at: ts(1_700_000_320),
                    link_reason: ThreadIntentLinkReason::Seed,
                },
                ThreadIntentRef {
                    intent_id: Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap(),
                    ordinal: 1,
                    is_head: true,
                    linked_at: ts(1_700_000_330),
                    link_reason: ThreadIntentLinkReason::Revision,
                },
            ],
            archived: true,
            updated_at: ts(1_700_000_350),
            ..sample_projection()
        };
        archived.create(&db).await.unwrap();

        let listed = ThreadProjection::list_active(&db, 10, 0).await.unwrap();
        let listed_ids = listed
            .into_iter()
            .map(|projection| projection.thread_id)
            .collect::<Vec<_>>();
        assert_eq!(listed_ids, vec![newer.thread_id, older.thread_id]);

        let paged = ThreadProjection::list_active(&db, 1, 1).await.unwrap();
        assert_eq!(paged.len(), 1);
        assert_eq!(paged[0].thread_id, older.thread_id);
    }

    #[tokio::test]
    async fn thread_projection_update_replaces_existing_rows() {
        let db = setup_test_db().await;
        let projection = sample_projection();
        projection.create(&db).await.unwrap();

        let updated = ThreadProjection {
            thread_id: projection.thread_id,
            title: Some("Release review".to_string()),
            owner: actor("human", "user-1", Some("Alice Updated")),
            participants: vec![ThreadParticipant {
                actor: actor("human", "reviewer-1", Some("Bob")),
                role: ThreadParticipantRole::Reviewer,
                joined_at: ts(1_700_000_100),
            }],
            current_intent_id: Some(
                Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap(),
            ),
            latest_intent_id: Some(
                Uuid::parse_str("55555555-5555-5555-5555-555555555555").unwrap(),
            ),
            intents: vec![ThreadIntentRef {
                intent_id: Uuid::parse_str("55555555-5555-5555-5555-555555555555").unwrap(),
                ordinal: 0,
                is_head: true,
                linked_at: ts(1_700_000_110),
                link_reason: ThreadIntentLinkReason::Followup,
            }],
            metadata: Some(json!({
                "workspace": "repo-b",
                "archived_by": "system"
            })),
            archived: true,
            created_at: projection.created_at,
            updated_at: ts(1_700_000_120),
            version: 2,
        };

        updated.update(&db).await.unwrap();

        let stored = ThreadProjection::find_by_id(&db, updated.thread_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored, updated);

        let participant_rows = ai_thread_participant::Entity::find()
            .filter(ai_thread_participant::Column::ThreadId.eq(updated.thread_id.to_string()))
            .all(&db)
            .await
            .unwrap();
        assert_eq!(participant_rows.len(), 1);
        assert_eq!(participant_rows[0].actor_id, "reviewer-1");

        let intent_rows = ai_thread_intent::Entity::find()
            .filter(ai_thread_intent::Column::ThreadId.eq(updated.thread_id.to_string()))
            .all(&db)
            .await
            .unwrap();
        assert_eq!(intent_rows.len(), 1);
        assert_eq!(
            intent_rows[0].intent_id,
            "55555555-5555-5555-5555-555555555555"
        );
    }

    #[tokio::test]
    async fn thread_projection_update_with_conn_uses_existing_transaction() {
        let db = setup_test_db().await;
        let projection = sample_projection();
        projection.create(&db).await.unwrap();

        let updated = ThreadProjection {
            title: Some("Transactional update".to_string()),
            updated_at: ts(1_700_000_180),
            version: 2,
            ..projection.clone()
        };

        let txn = db.begin().await.unwrap();
        updated.update_with_conn(&txn).await.unwrap();
        txn.commit().await.unwrap();

        let stored = ThreadProjection::find_by_id(&db, updated.thread_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored, updated);
    }

    #[tokio::test]
    async fn thread_projection_update_returns_error_for_missing_thread() {
        let db = setup_test_db().await;
        let projection = sample_projection();

        let err = projection.update(&db).await.unwrap_err();
        assert!(err.to_string().contains("does not exist"));
    }

    #[tokio::test]
    async fn thread_projection_update_rejects_stale_version() {
        let db = setup_test_db().await;
        let projection = sample_projection();
        projection.create(&db).await.unwrap();

        let mut stale = projection.clone();
        stale.title = Some("Stale update".to_string());
        stale.updated_at = ts(1_700_000_200);

        let err = stale.update(&db).await.unwrap_err();
        assert!(err.to_string().contains("expects version 2, got 1"));

        let stored = ThreadProjection::find_by_id(&db, projection.thread_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored, projection);
    }

    #[tokio::test]
    async fn thread_projection_update_rejects_archived_thread() {
        let db = setup_test_db().await;
        let projection = sample_projection();
        projection.create(&db).await.unwrap();

        let archived = ThreadProjection {
            archived: true,
            updated_at: ts(1_700_000_180),
            version: 2,
            ..projection.clone()
        };
        archived.update(&db).await.unwrap();

        let rejected = ThreadProjection {
            title: Some("Should fail".to_string()),
            updated_at: ts(1_700_000_200),
            version: 3,
            ..archived.clone()
        };

        let err = rejected.update(&db).await.unwrap_err();
        assert!(
            err.to_string()
                .contains("is archived and cannot be updated")
        );

        let stored = ThreadProjection::find_by_id(&db, projection.thread_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored, archived);
    }

    #[tokio::test]
    async fn thread_projection_update_rejects_owner_identity_change() {
        let db = setup_test_db().await;
        let projection = sample_projection();
        projection.create(&db).await.unwrap();

        let mut invalid = projection.clone();
        invalid.owner = actor("agent", "coordinator", Some("Coordinator"));
        invalid.updated_at = ts(1_700_000_200);
        invalid.version = 2;

        let err = invalid.update(&db).await.unwrap_err();
        assert!(err.to_string().contains("owner identity is immutable"));

        let stored = ThreadProjection::find_by_id(&db, projection.thread_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored, projection);
    }

    #[tokio::test]
    async fn thread_projection_update_rejects_created_at_change() {
        let db = setup_test_db().await;
        let projection = sample_projection();
        projection.create(&db).await.unwrap();

        let mut invalid = projection.clone();
        invalid.created_at = ts(1_700_000_999);
        invalid.updated_at = ts(1_700_000_200);
        invalid.version = 2;

        let err = invalid.update(&db).await.unwrap_err();
        assert!(err.to_string().contains("created_at is immutable"));

        let stored = ThreadProjection::find_by_id(&db, projection.thread_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored, projection);
    }

    #[tokio::test]
    async fn thread_projection_find_by_id_returns_none_when_missing() {
        let db = setup_test_db().await;

        let stored = ThreadProjection::find_by_id(
            &db,
            Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap(),
        )
        .await
        .unwrap();

        assert!(stored.is_none());
    }

    #[test]
    fn thread_participant_role_row_encoding_matches_serde() {
        let roles = [
            ThreadParticipantRole::Owner,
            ThreadParticipantRole::Member,
            ThreadParticipantRole::Reviewer,
            ThreadParticipantRole::Observer,
        ];

        for role in roles {
            let row_value = thread_participant_role_to_row(&role);
            let serde_value = serde_json::to_string(&role).unwrap();
            assert_eq!(serde_value, format!("\"{row_value}\""));
            assert_eq!(
                thread_participant_role_from_row(row_value, Uuid::nil()).unwrap(),
                role
            );
        }
    }

    #[test]
    fn thread_intent_link_reason_row_encoding_matches_serde() {
        let reasons = [
            ThreadIntentLinkReason::Seed,
            ThreadIntentLinkReason::Revision,
            ThreadIntentLinkReason::Split,
            ThreadIntentLinkReason::Merge,
            ThreadIntentLinkReason::Followup,
            ThreadIntentLinkReason::Imported,
        ];

        for reason in reasons {
            let row_value = thread_intent_link_reason_to_row(&reason);
            let serde_value = serde_json::to_string(&reason).unwrap();
            assert_eq!(serde_value, format!("\"{row_value}\""));
            assert_eq!(
                thread_intent_link_reason_from_row(row_value, Uuid::nil()).unwrap(),
                reason
            );
        }
    }
}
