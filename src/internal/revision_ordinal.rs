//! Revision ordinal index (lore.md §1.16) — the SINGLE owner API for the
//! `revision_ordinal` / `revision_ordinal_meta` tables.
//!
//! ORDINAL SEMANTICS: per-ref ordinal spaces over the FIRST-PARENT chain —
//! `ordinal(c)` is the 1-based position of `c` on the root→tip first-parent
//! walk of the ref (1 = root, N = tip). Every commit has exactly one first
//! parent, so the numbering is a pure function of the tip OID: rebuilding at
//! any time reproduces identical `(ref, ordinal, oid)` rows (test-pinned).
//! Commits reachable only through non-first parents have NO ordinal — the
//! reverse lookup says so explicitly rather than inventing a number.
//!
//! FRESHNESS (the 1.1 cache-never-lies principle): every read validates the
//! per-ref fingerprint — the tip OID **plus a digest of the `refs/replace`
//! set** (a replace mutation changes the EFFECTIVE chain that `load_object`
//! resolves without moving the tip). Tip moved forward → the new suffix is
//! APPENDED (existing ordinals never change — Lore's monotone property);
//! any other mismatch (rewrite, reset to an ancestor, replace change) → a
//! full deterministic rebuild. Both run inside the caller's transaction, so
//! concurrent readers never observe partial numbering. A stale index never
//! answers.

use anyhow::{Context, Result, anyhow};
use git_internal::hash::ObjectHash;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait,
    QueryFilter, QueryOrder,
};

use crate::internal::model::{revision_ordinal, revision_ordinal_meta};

/// Insert chunk size — comfortably under SQLite's bind-variable limits.
const INSERT_CHUNK: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrdinalMeta {
    pub ref_name: String,
    pub tip_oid: String,
    pub replace_sig: String,
    pub max_ordinal: i64,
    pub built_at: String,
}

/// The replace-set signature of the AMBIENT snapshot (see
/// `command::replace`). The builder paths below do NOT use this directly for
/// stamping: they take ONE pinned snapshot and derive both the signature and
/// the walk from it, so a concurrent replace mutation can never make the
/// stamped signature describe a different map than the chain indexed.
pub fn current_replace_signature() -> String {
    crate::command::replace::effective_replace_signature()
}

/// Walk first parents from `tip` down, collecting OIDs tip-first, stopping
/// EARLY when `stop_at` is reached (exclusive). Returns `(chain_tip_first,
/// reached_stop)`.
fn walk_first_parents(tip: &ObjectHash, stop_at: Option<&str>) -> Result<(Vec<String>, bool)> {
    let mut chain: Vec<String> = Vec::new();
    let mut cursor = *tip;
    loop {
        let oid_text = cursor.to_string();
        if stop_at == Some(oid_text.as_str()) {
            return Ok((chain, true));
        }
        chain.push(oid_text);
        let commit: git_internal::internal::object::commit::Commit =
            crate::command::load_object(&cursor)
                .map_err(|error| anyhow!("failed to load commit {cursor}: {error}"))?;
        match commit.parent_commit_ids.first() {
            Some(parent) => cursor = *parent,
            None => return Ok((chain, false)),
        }
    }
}

/// The single owner API for the revision ordinal tables.
pub struct RevisionOrdinalIndex;

impl RevisionOrdinalIndex {
    pub async fn meta_with_conn<C: ConnectionTrait>(
        db: &C,
        ref_name: &str,
    ) -> Result<Option<OrdinalMeta>> {
        let row = revision_ordinal_meta::Entity::find_by_id(ref_name)
            .one(db)
            .await
            .context("failed to read revision_ordinal_meta")?;
        Ok(row.map(|row| OrdinalMeta {
            ref_name: row.ref_name,
            tip_oid: row.tip_oid,
            replace_sig: row.replace_sig,
            max_ordinal: row.max_ordinal,
            built_at: row.built_at,
        }))
    }

    /// Validate / extend / rebuild so the index answers for exactly
    /// `current_tip` under the current replace set. Call INSIDE the same
    /// transaction as the subsequent read.
    pub async fn ensure_fresh_with_conn<C: ConnectionTrait>(
        db: &C,
        ref_name: &str,
        current_tip: &ObjectHash,
    ) -> Result<OrdinalMeta> {
        // ONE pinned snapshot for the signature AND the walk (§C.4.1.1):
        // with fresh per-call replace reads, a concurrent retarget between
        // the two would stamp rows under a signature the walk never used.
        let replace_snapshot = crate::command::replace::snapshot();
        let replace_sig = crate::command::replace::signature_of(&replace_snapshot);
        let tip_text = current_tip.to_string();
        let existing = Self::meta_with_conn(db, ref_name).await?;
        if let Some(meta) = &existing
            && meta.tip_oid == tip_text
            && meta.replace_sig == replace_sig
        {
            return Ok(meta.clone());
        }
        // A replace-set change invalidates the WHOLE chain (any commit may
        // resolve differently) → full rebuild. A pure tip move may be a
        // fast-forward → append the new suffix.
        if let Some(meta) = &existing
            && meta.replace_sig == replace_sig
        {
            let (suffix_tip_first, reached_old_tip) =
                crate::command::replace::with_pinned_snapshot_sync(
                    replace_snapshot.clone(),
                    || walk_first_parents(current_tip, Some(meta.tip_oid.as_str())),
                )?;
            if reached_old_tip {
                // Fast-forward: append suffix (root-most first).
                let mut ordinal = meta.max_ordinal;
                let rows: Vec<revision_ordinal::ActiveModel> = suffix_tip_first
                    .iter()
                    .rev()
                    .map(|oid| {
                        ordinal += 1;
                        revision_ordinal::ActiveModel {
                            ref_name: Set(ref_name.to_string()),
                            ordinal: Set(ordinal),
                            oid: Set(oid.clone()),
                            ..Default::default()
                        }
                    })
                    .collect();
                for chunk in rows.chunks(INSERT_CHUNK) {
                    revision_ordinal::Entity::insert_many(chunk.to_vec())
                        .exec(db)
                        .await
                        .context("failed to append ordinal rows")?;
                }
                return Self::stamp_meta_with_conn(db, ref_name, &tip_text, &replace_sig, ordinal)
                    .await;
            }
        }
        Self::rebuild_with_conn(db, ref_name, current_tip).await
    }

    /// Full deterministic rebuild: delete the ref's rows, walk root→tip,
    /// insert, stamp. One transaction (the caller's).
    pub async fn rebuild_with_conn<C: ConnectionTrait>(
        db: &C,
        ref_name: &str,
        current_tip: &ObjectHash,
    ) -> Result<OrdinalMeta> {
        // Same pinning discipline as `ensure_fresh_with_conn`: signature and
        // walk both come from ONE immutable snapshot.
        let replace_snapshot = crate::command::replace::snapshot();
        let replace_sig = crate::command::replace::signature_of(&replace_snapshot);
        let tip_text = current_tip.to_string();
        let (chain_tip_first, _) =
            crate::command::replace::with_pinned_snapshot_sync(replace_snapshot, || {
                walk_first_parents(current_tip, None)
            })?;
        revision_ordinal::Entity::delete_many()
            .filter(revision_ordinal::Column::RefName.eq(ref_name))
            .exec(db)
            .await
            .context("failed to clear ordinal rows for rebuild")?;
        let mut ordinal = 0i64;
        let rows: Vec<revision_ordinal::ActiveModel> = chain_tip_first
            .iter()
            .rev()
            .map(|oid| {
                ordinal += 1;
                revision_ordinal::ActiveModel {
                    ref_name: Set(ref_name.to_string()),
                    ordinal: Set(ordinal),
                    oid: Set(oid.clone()),
                    ..Default::default()
                }
            })
            .collect();
        for chunk in rows.chunks(INSERT_CHUNK) {
            revision_ordinal::Entity::insert_many(chunk.to_vec())
                .exec(db)
                .await
                .context("failed to insert ordinal rows")?;
        }
        Self::stamp_meta_with_conn(db, ref_name, &tip_text, &replace_sig, ordinal).await
    }

    async fn stamp_meta_with_conn<C: ConnectionTrait>(
        db: &C,
        ref_name: &str,
        tip_oid: &str,
        replace_sig: &str,
        max_ordinal: i64,
    ) -> Result<OrdinalMeta> {
        let built_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
        let existing = revision_ordinal_meta::Entity::find_by_id(ref_name)
            .one(db)
            .await
            .context("failed to read revision_ordinal_meta")?;
        match existing {
            Some(row) => {
                let mut active: revision_ordinal_meta::ActiveModel = row.into();
                active.tip_oid = Set(tip_oid.to_string());
                active.replace_sig = Set(replace_sig.to_string());
                active.max_ordinal = Set(max_ordinal);
                active.built_at = Set(built_at.clone());
                active
                    .update(db)
                    .await
                    .context("failed to update revision_ordinal_meta")?;
            }
            None => {
                revision_ordinal_meta::ActiveModel {
                    ref_name: Set(ref_name.to_string()),
                    tip_oid: Set(tip_oid.to_string()),
                    replace_sig: Set(replace_sig.to_string()),
                    max_ordinal: Set(max_ordinal),
                    built_at: Set(built_at.clone()),
                }
                .insert(db)
                .await
                .context("failed to insert revision_ordinal_meta")?;
            }
        }
        Ok(OrdinalMeta {
            ref_name: ref_name.to_string(),
            tip_oid: tip_oid.to_string(),
            replace_sig: replace_sig.to_string(),
            max_ordinal,
            built_at,
        })
    }

    pub async fn find_by_ordinal_with_conn<C: ConnectionTrait>(
        db: &C,
        ref_name: &str,
        ordinal: i64,
    ) -> Result<Option<String>> {
        let row = revision_ordinal::Entity::find()
            .filter(revision_ordinal::Column::RefName.eq(ref_name))
            .filter(revision_ordinal::Column::Ordinal.eq(ordinal))
            .one(db)
            .await
            .context("failed to query revision_ordinal")?;
        Ok(row.map(|row| row.oid))
    }

    pub async fn ordinal_of_with_conn<C: ConnectionTrait>(
        db: &C,
        ref_name: &str,
        oid: &str,
    ) -> Result<Option<i64>> {
        let row = revision_ordinal::Entity::find()
            .filter(revision_ordinal::Column::RefName.eq(ref_name))
            .filter(revision_ordinal::Column::Oid.eq(oid))
            .one(db)
            .await
            .context("failed to query revision_ordinal")?;
        Ok(row.map(|row| row.ordinal))
    }

    /// Sweep rows/meta for refs that no longer exist (invoked from
    /// `revision index`, giving prune a real trigger).
    pub async fn prune_missing_refs_with_conn<C: ConnectionTrait>(
        db: &C,
        live_refs: &[String],
    ) -> Result<usize> {
        let known: Vec<String> = revision_ordinal_meta::Entity::find()
            .all(db)
            .await
            .context("failed to list revision_ordinal_meta")?
            .into_iter()
            .map(|row| row.ref_name)
            .collect();
        let mut pruned = 0usize;
        for ref_name in known {
            if !live_refs.contains(&ref_name) {
                revision_ordinal::Entity::delete_many()
                    .filter(revision_ordinal::Column::RefName.eq(ref_name.as_str()))
                    .exec(db)
                    .await
                    .context("failed to prune ordinal rows")?;
                revision_ordinal_meta::Entity::delete_by_id(ref_name.as_str())
                    .exec(db)
                    .await
                    .context("failed to prune ordinal meta")?;
                pruned += 1;
            }
        }
        Ok(pruned)
    }

    /// Row count for a ref (diagnostics).
    pub async fn count_with_conn<C: ConnectionTrait>(db: &C, ref_name: &str) -> Result<u64> {
        revision_ordinal::Entity::find()
            .filter(revision_ordinal::Column::RefName.eq(ref_name))
            .count(db)
            .await
            .context("failed to count ordinal rows")
    }

    /// The `(ordinal, oid)` projection in ordinal order (determinism tests —
    /// the AUTOINCREMENT id column is deliberately excluded).
    pub async fn projection_with_conn<C: ConnectionTrait>(
        db: &C,
        ref_name: &str,
    ) -> Result<Vec<(i64, String)>> {
        let rows = revision_ordinal::Entity::find()
            .filter(revision_ordinal::Column::RefName.eq(ref_name))
            .order_by_asc(revision_ordinal::Column::Ordinal)
            .all(db)
            .await
            .context("failed to list ordinal rows")?;
        Ok(rows.into_iter().map(|row| (row.ordinal, row.oid)).collect())
    }
}
