//! Unified scoped metadata store (lore.md §1.5/§1.10) — the SINGLE owner API
//! for all metadata scopes. Branch metadata lives in the `metadata_kv` table;
//! repo-scope metadata intentionally lives in `config_kv` under the
//! `metadata.*` namespace (see [`REPO_METADATA_PREFIX`]) so `libra config`
//! tooling keeps working on it; revision metadata merges the commit's
//! immutable trailer block with a mutable notes layer (see the revision
//! section below); file-scope metadata awaits its own design round.
//!
//! `protect` / `archive` / `lineage.*` are KEYS in this store, never separate
//! tables. Nothing enforces them yet: enforcement lands once, in the future
//! branch-policy layer (lore.md 1.13), which reads through
//! [`MetadataKv::is_protected_with_conn`] / [`MetadataKv::is_archived_with_conn`]
//! inside its authoritative transaction. The truthy parse is FAIL-CLOSED — a
//! garbage value counts as protected — so a corrupted value can never silently
//! disable protection when enforcement arrives.
//!
//! Every core operation ships a `_with_conn` variant (transaction-safe,
//! matching the `ConfigKv` convention) plus a pool-acquiring wrapper.

use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter,
    QueryOrder, sea_query::OnConflict,
};

use crate::internal::{db::get_db_conn_instance, model::metadata_kv};

/// Repo-scope metadata namespace inside `config_kv` (lore.md §1.5: repo =
/// config_kv). `libra metadata --repo <key>` reads/writes `metadata.<key>`
/// through `ConfigKv`; `libra config` operating on the same keys is an
/// intended dual surface.
pub const REPO_METADATA_PREFIX: &str = "metadata.";

/// Well-known branch-metadata key: branch protection (recorded now, enforced
/// by the future branch-policy layer, lore.md 1.13).
pub const KEY_PROTECT: &str = "protect";
/// Well-known branch-metadata key: branch archival.
pub const KEY_ARCHIVE: &str = "archive";
/// Well-known branch-metadata key prefix: branch lineage records.
pub const LINEAGE_PREFIX: &str = "lineage.";

/// Maximum metadata key length in bytes.
pub const MAX_KEY_LEN: usize = 256;
/// Maximum metadata value length in bytes (text values in v1).
pub const MAX_VALUE_LEN: usize = 1024 * 1024;

/// A reasoning identity session accumulates one slot per committed logical
/// turn, so its bounds must not reuse a single-checkpoint artifact limit.
pub(crate) const REASONING_IDENTITY_SESSION_MAP_MAX_ENTRIES: usize = 4096;
pub(crate) const REASONING_IDENTITY_SESSION_MAP_MAX_BYTES: usize = MAX_VALUE_LEN;

/// The metadata scope. v1 supports `Branch`; the `scope` column is TEXT so
/// future scopes (worktree, …; revision/file metadata use trailers/side-trees
/// per lore.md 1.10, not this table) need no table rebuild.
///
/// `AgentTracesInflight` (AG-20) holds the external-agent checkpoint
/// writer's in-progress markers: `target` = the Libra agent session id,
/// `key` = the write attempt's checkpoint UUID, `value` = a JSON
/// [`crate::internal::ai::history::TracesInflightMarker`]. The markers close
/// the prune windows A/B described in
/// `docs/development/tracing/agent.md` (write-sequence matrix): prune must
/// treat OIDs/commits named by a live (non-expired) marker as reachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataScope {
    Branch,
    AgentTracesInflight,
    /// Durable import-side marker requiring foreground object-index repair
    /// before an otherwise idempotent replay may become a no-op.
    AgentImportIndexRepair,
    /// Repository-private authenticated SessionEnd recovery headers.
    AgentCapturePending,
    /// Retained headers requiring explicit repair; still count towards capacity.
    AgentCaptureQuarantine,
    /// Bounded base64 chunks; never enumerate through generic metadata/GC walkers.
    AgentCapturePendingChunk,
    /// Local sensitive catalog association; never hydrate through GC/export.
    AgentCaptureSessionAlias,
    /// Local authenticated per-turn reasoning commitments; never a generic
    /// OID root, cloud projection, or public checkpoint metadata surface.
    AgentReasoningIdentity,
    /// Per-session `kept` / `dropped` counts for coalesced display batches
    /// (Claude MessageDisplay). Counts only; never content.
    AgentDisplayBatchCount,
}

impl MetadataScope {
    pub fn as_str(self) -> &'static str {
        match self {
            MetadataScope::Branch => "branch",
            MetadataScope::AgentTracesInflight => "agent_traces_inflight",
            MetadataScope::AgentImportIndexRepair => "agent_import_index_repair",
            MetadataScope::AgentCapturePending => "agent_capture_pending",
            MetadataScope::AgentCaptureQuarantine => "agent_capture_quarantine",
            MetadataScope::AgentCapturePendingChunk => "agent_capture_pending_chunk",
            MetadataScope::AgentCaptureSessionAlias => "agent_capture_session_alias",
            MetadataScope::AgentReasoningIdentity => "agent_reasoning_identity",
            MetadataScope::AgentDisplayBatchCount => "agent_display_batch_count",
        }
    }
}

/// Take one slot of a per-session display-batch budget.
///
/// One conditional UPSERT bumps `kept` while it is below `limit` and returns
/// `true`. Once the budget is spent only `dropped` grows and the call returns
/// `false`. Rows live under [`MetadataScope::AgentDisplayBatchCount`] with
/// `target` = the Libra session id; they hold counts only and session erase
/// deletes them.
pub(crate) async fn take_display_batch_slot<C: ConnectionTrait>(
    conn: &C,
    session_id: &str,
    limit: u32,
) -> Result<bool> {
    let now = Utc::now().to_rfc3339();
    let bump = |key: &'static str, cap: Option<u32>| {
        let mut sql = "INSERT INTO metadata_kv \
             (scope, target, key, value, value_type, created_at, updated_at) \
             VALUES (?, ?, ?, '1', 'integer', ?, ?) \
             ON CONFLICT(scope, target, key) DO UPDATE SET \
             value = CAST(CAST(value AS INTEGER) + 1 AS TEXT), \
             updated_at = excluded.updated_at"
            .to_string();
        let mut values: Vec<sea_orm::Value> = vec![
            MetadataScope::AgentDisplayBatchCount.as_str().into(),
            session_id.into(),
            key.into(),
            now.clone().into(),
            now.clone().into(),
        ];
        if let Some(cap) = cap {
            sql.push_str(" WHERE CAST(value AS INTEGER) < ?");
            values.push(i64::from(cap).into());
        }
        sea_orm::Statement::from_sql_and_values(conn.get_database_backend(), sql, values)
    };
    let kept = conn
        .execute_raw(bump("kept", Some(limit)))
        .await
        .context("count a coalesced display batch in the repository database")?;
    if kept.rows_affected() > 0 {
        return Ok(true);
    }
    conn.execute_raw(bump("dropped", None))
        .await
        .context("count a dropped display batch in the repository database")?;
    Ok(false)
}

/// This codec is local comparison metadata, never native-origin evidence.
/// Only the coverage/store owner may treat a decoded record as comparable,
/// after authenticating its MAC and current claim/workspace fences.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReasoningIdentityKind {
    Empty,
    Nonempty,
}

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReasoningIdentityRecordContext {
    pub(crate) repository_id: String,
    pub(crate) session_id: String,
    pub(crate) logical_turn_key: String,
    pub(crate) coverage_schema_version: i64,
    pub(crate) claim_revision: i64,
    pub(crate) claim_fence_token: i64,
    pub(crate) checkpoint_id: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReasoningIdentityRecordBody {
    pub(crate) context: ReasoningIdentityRecordContext,
    pub(crate) kind: ReasoningIdentityKind,
    pub(crate) descriptor_mac: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReasoningIdentityRecord {
    version: u8,
    body: ReasoningIdentityRecordBody,
    mac: String,
}

impl std::fmt::Debug for ReasoningIdentityRecord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReasoningIdentityRecord")
            .field("version", &self.version)
            .field("kind", &self.body.kind)
            .finish_non_exhaustive()
    }
}

impl ReasoningIdentityRecord {
    pub(crate) const VERSION: u8 = 1;

    /// Canonical metadata bytes to authenticate with the key owner's Record
    /// purpose. The outer version is inside the MAC; the MAC itself is not.
    pub(crate) fn authentication_body(body: &ReasoningIdentityRecordBody) -> Result<Vec<u8>> {
        Self::validate_body(body)?;
        let bytes =
            serde_json::to_vec(&(Self::VERSION, body)).map_err(|_| Self::invalid_record())?;
        if bytes.len() > crate::internal::ai::traces::REASONING_ARTIFACT_MANIFEST_MAX_BYTES {
            return Err(Self::invalid_record());
        }
        Ok(bytes)
    }

    /// Attach a computed local Record MAC. This only validates the closed
    /// syntax; authentication remains the key/coverage owner's responsibility.
    pub(crate) fn from_body_and_mac(
        body: ReasoningIdentityRecordBody,
        mac: String,
    ) -> Result<Self> {
        let record = Self {
            version: Self::VERSION,
            body,
            mac,
        };
        record.validate_syntax()?;
        Ok(record)
    }

    #[cfg(test)]
    pub(crate) fn decode(text: &str) -> Result<Self> {
        if text.len() > crate::internal::ai::traces::REASONING_ARTIFACT_MANIFEST_MAX_BYTES {
            return Err(Self::invalid_record());
        }
        let record: Self = serde_json::from_str(text).map_err(|_| Self::invalid_record())?;
        record.validate_syntax()?;
        Ok(record)
    }

    #[cfg(test)]
    pub(crate) fn encode(&self) -> Result<String> {
        self.validate_syntax()?;
        let text = serde_json::to_string(self).map_err(|_| Self::invalid_record())?;
        if text.len() > crate::internal::ai::traces::REASONING_ARTIFACT_MANIFEST_MAX_BYTES {
            return Err(Self::invalid_record());
        }
        Ok(text)
    }

    pub(crate) fn body(&self) -> &ReasoningIdentityRecordBody {
        &self.body
    }

    pub(crate) fn mac(&self) -> &str {
        &self.mac
    }

    pub(crate) fn validate_context(&self, expected: &ReasoningIdentityRecordContext) -> Result<()> {
        if &self.body.context != expected {
            return Err(anyhow::anyhow!(
                "private reasoning identity does not match the current claim; start a new capture session"
            ));
        }
        Ok(())
    }

    fn validate_syntax(&self) -> Result<()> {
        use crate::internal::ai::capture::key::ReasoningIdentityMacPurpose;
        if self.version != Self::VERSION
            || !Self::valid_mac_syntax(ReasoningIdentityMacPurpose::Record, &self.mac)
        {
            return Err(Self::invalid_record());
        }
        Self::validate_body(&self.body)
    }

    fn validate_body(body: &ReasoningIdentityRecordBody) -> Result<()> {
        use crate::internal::ai::capture::key::ReasoningIdentityMacPurpose;
        let context = &body.context;
        if [
            context.repository_id.as_str(),
            context.session_id.as_str(),
            context.logical_turn_key.as_str(),
            context.checkpoint_id.as_str(),
        ]
        .iter()
        .any(|value| {
            value.is_empty() || value.len() > MAX_KEY_LEN || value.chars().any(char::is_control)
        }) || context.coverage_schema_version != 1
            || context.claim_revision <= 0
            || context.claim_fence_token <= 0
            || !Self::valid_mac_syntax(
                ReasoningIdentityMacPurpose::Descriptor,
                &body.descriptor_mac,
            )
        {
            return Err(Self::invalid_record());
        }
        Ok(())
    }

    fn valid_mac_syntax(
        purpose: crate::internal::ai::capture::key::ReasoningIdentityMacPurpose,
        tag: &str,
    ) -> bool {
        tag.strip_prefix(purpose.prefix()).is_some_and(|suffix| {
            suffix.len() == 64
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    }

    fn invalid_record() -> anyhow::Error {
        anyhow::anyhow!("private reasoning identity is invalid; start a new capture session")
    }
}

/// The v2 identity is a separate private codec. Its byte domain and MAC
/// purpose do not alias the frozen identity-v1 envelope.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReasoningIdentityRecordV2 {
    version: u8,
    body: ReasoningIdentityRecordBody,
    mac: String,
}

impl std::fmt::Debug for ReasoningIdentityRecordV2 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReasoningIdentityRecordV2")
            .field("version", &self.version)
            .field("kind", &self.body.kind)
            .finish_non_exhaustive()
    }
}

impl ReasoningIdentityRecordV2 {
    pub(crate) const VERSION: u8 = 2;

    pub(crate) fn authentication_body(body: &ReasoningIdentityRecordBody) -> Result<Vec<u8>> {
        Self::validate_body(body)?;
        let bytes = serde_json::to_vec(&("reasoning-identity-record-v2", Self::VERSION, body))
            .map_err(|_| ReasoningIdentityRecord::invalid_record())?;
        if bytes.len() > crate::internal::ai::traces::REASONING_ARTIFACT_MANIFEST_MAX_BYTES {
            return Err(ReasoningIdentityRecord::invalid_record());
        }
        Ok(bytes)
    }

    // COV2-14 validates the v2 carrier; COV2-03 will connect its writer.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn from_body_and_mac(
        body: ReasoningIdentityRecordBody,
        mac: String,
    ) -> Result<Self> {
        let record = Self {
            version: Self::VERSION,
            body,
            mac,
        };
        record.validate_syntax()?;
        Ok(record)
    }

    #[cfg(test)]
    pub(crate) fn decode(text: &str) -> Result<Self> {
        if text.len() > crate::internal::ai::traces::REASONING_ARTIFACT_MANIFEST_MAX_BYTES {
            return Err(ReasoningIdentityRecord::invalid_record());
        }
        let record: Self =
            serde_json::from_str(text).map_err(|_| ReasoningIdentityRecord::invalid_record())?;
        record.validate_syntax()?;
        Ok(record)
    }

    #[cfg(test)]
    pub(crate) fn encode(&self) -> Result<String> {
        self.validate_syntax()?;
        let text =
            serde_json::to_string(self).map_err(|_| ReasoningIdentityRecord::invalid_record())?;
        if text.len() > crate::internal::ai::traces::REASONING_ARTIFACT_MANIFEST_MAX_BYTES {
            return Err(ReasoningIdentityRecord::invalid_record());
        }
        Ok(text)
    }

    pub(crate) fn body(&self) -> &ReasoningIdentityRecordBody {
        &self.body
    }

    pub(crate) fn mac(&self) -> &str {
        &self.mac
    }

    pub(crate) fn validate_context(&self, expected: &ReasoningIdentityRecordContext) -> Result<()> {
        if &self.body.context != expected {
            return Err(anyhow::anyhow!(
                "private reasoning identity does not match the current claim; start a new capture session"
            ));
        }
        Ok(())
    }

    fn validate_syntax(&self) -> Result<()> {
        use crate::internal::ai::capture::key::ReasoningIdentityMacPurpose;
        if self.version != Self::VERSION
            || !ReasoningIdentityRecord::valid_mac_syntax(
                ReasoningIdentityMacPurpose::RecordV2,
                &self.mac,
            )
        {
            return Err(ReasoningIdentityRecord::invalid_record());
        }
        Self::validate_body(&self.body)
    }

    fn validate_body(body: &ReasoningIdentityRecordBody) -> Result<()> {
        use crate::internal::ai::capture::key::ReasoningIdentityMacPurpose;
        let context = &body.context;
        if [
            context.repository_id.as_str(),
            context.session_id.as_str(),
            context.logical_turn_key.as_str(),
            context.checkpoint_id.as_str(),
        ]
        .iter()
        .any(|value| {
            value.is_empty() || value.len() > MAX_KEY_LEN || value.chars().any(char::is_control)
        }) || context.coverage_schema_version != 2
            || context.claim_revision <= 0
            || context.claim_fence_token <= 0
            || !ReasoningIdentityRecord::valid_mac_syntax(
                ReasoningIdentityMacPurpose::DescriptorV2,
                &body.descriptor_mac,
            )
        {
            return Err(ReasoningIdentityRecord::invalid_record());
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
#[error(
    "private reasoning identity session map reached its safe capacity; start a new capture session"
)]
pub(crate) struct ReasoningIdentityMapCapacity;

/// One authenticated, bounded session map. A missing entry is legacy only
/// after the outer map has been authenticated; a damaged map is never empty.
/// Keys are repository-keyed turn commitments, not provider coordinates.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReasoningIdentitySessionMap {
    version: u8,
    repository_id: String,
    session_id: String,
    #[serde(deserialize_with = "deserialize_reasoning_identity_records")]
    records: std::collections::BTreeMap<String, ReasoningIdentityRecord>,
    mac: String,
}

fn deserialize_reasoning_identity_records<'de, D>(
    deserializer: D,
) -> std::result::Result<std::collections::BTreeMap<String, ReasoningIdentityRecord>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct UniqueRecords;
    impl<'de> serde::de::Visitor<'de> for UniqueRecords {
        type Value = std::collections::BTreeMap<String, ReasoningIdentityRecord>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a bounded private reasoning identity map")
        }

        fn visit_map<A>(self, mut access: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: serde::de::MapAccess<'de>,
        {
            let mut records = std::collections::BTreeMap::new();
            while let Some((slot, record)) =
                access.next_entry::<String, ReasoningIdentityRecord>()?
            {
                if records.len() >= REASONING_IDENTITY_SESSION_MAP_MAX_ENTRIES
                    || records.insert(slot, record).is_some()
                {
                    return Err(serde::de::Error::custom(
                        "invalid private reasoning identity map",
                    ));
                }
            }
            Ok(records)
        }
    }
    deserializer.deserialize_map(UniqueRecords)
}

impl ReasoningIdentitySessionMap {
    pub(crate) const KEY: &str = "identity-v1";

    pub(crate) fn authentication_body(
        repository_id: &str,
        session_id: &str,
        records: &std::collections::BTreeMap<String, ReasoningIdentityRecord>,
    ) -> Result<Vec<u8>> {
        Self::validate_records(repository_id, session_id, records)?;
        let bytes = serde_json::to_vec(&(
            "reasoning-identity-session-map",
            ReasoningIdentityRecord::VERSION,
            repository_id,
            session_id,
            records,
        ))
        .map_err(|_| ReasoningIdentityRecord::invalid_record())?;
        Self::validate_size(bytes.len())?;
        Ok(bytes)
    }

    pub(crate) fn from_records_and_mac(
        repository_id: String,
        session_id: String,
        records: std::collections::BTreeMap<String, ReasoningIdentityRecord>,
        mac: String,
    ) -> Result<Self> {
        let map = Self {
            version: ReasoningIdentityRecord::VERSION,
            repository_id,
            session_id,
            records,
            mac,
        };
        map.validate_syntax()?;
        // Check the persisted envelope as well as its authentication body.
        map.encode()?;
        Ok(map)
    }

    pub(crate) fn decode(text: &str) -> Result<Self> {
        Self::validate_size(text.len())?;
        let map: Self =
            serde_json::from_str(text).map_err(|_| ReasoningIdentityRecord::invalid_record())?;
        map.validate_syntax()?;
        Ok(map)
    }

    pub(crate) fn encode(&self) -> Result<String> {
        self.validate_syntax()?;
        let text =
            serde_json::to_string(self).map_err(|_| ReasoningIdentityRecord::invalid_record())?;
        Self::validate_size(text.len())?;
        Ok(text)
    }

    pub(crate) fn records(&self) -> &std::collections::BTreeMap<String, ReasoningIdentityRecord> {
        &self.records
    }

    pub(crate) fn mac(&self) -> &str {
        &self.mac
    }

    pub(crate) fn validate_owner(&self, repository_id: &str, session_id: &str) -> Result<()> {
        if self.repository_id != repository_id || self.session_id != session_id {
            return Err(ReasoningIdentityRecord::invalid_record());
        }
        Ok(())
    }

    fn validate_syntax(&self) -> Result<()> {
        if self.version != ReasoningIdentityRecord::VERSION
            || !ReasoningIdentityRecord::valid_mac_syntax(
                crate::internal::ai::capture::key::ReasoningIdentityMacPurpose::Record,
                &self.mac,
            )
        {
            return Err(ReasoningIdentityRecord::invalid_record());
        }
        Self::validate_records(&self.repository_id, &self.session_id, &self.records)
    }

    fn validate_records(
        repository_id: &str,
        session_id: &str,
        records: &std::collections::BTreeMap<String, ReasoningIdentityRecord>,
    ) -> Result<()> {
        if [repository_id, session_id].iter().any(|value| {
            value.is_empty() || value.len() > MAX_KEY_LEN || value.chars().any(char::is_control)
        }) {
            return Err(ReasoningIdentityRecord::invalid_record());
        }
        if records.len() > REASONING_IDENTITY_SESSION_MAP_MAX_ENTRIES {
            return Err(ReasoningIdentityMapCapacity.into());
        }
        for (slot, record) in records {
            record.validate_syntax()?;
            let context = &record.body.context;
            if !ReasoningIdentityRecord::valid_mac_syntax(
                crate::internal::ai::capture::key::ReasoningIdentityMacPurpose::Descriptor,
                slot,
            ) || context.repository_id != repository_id
                || context.session_id != session_id
                || context.logical_turn_key != *slot
            {
                return Err(ReasoningIdentityRecord::invalid_record());
            }
        }
        Ok(())
    }

    fn validate_size(size: usize) -> Result<()> {
        if size > REASONING_IDENTITY_SESSION_MAP_MAX_BYTES {
            return Err(ReasoningIdentityMapCapacity.into());
        }
        Ok(())
    }
}

/// The private namespace admits only these two map keys. Callers must check
/// the complete bounded key set before interpreting a missing map as legacy.
pub(crate) fn validate_reasoning_identity_private_map_keys<'a>(
    keys: impl IntoIterator<Item = &'a str>,
) -> Result<()> {
    let mut seen_v1 = false;
    let mut seen_v2 = false;
    for key in keys {
        let seen = match key {
            ReasoningIdentitySessionMap::KEY => &mut seen_v1,
            ReasoningIdentitySessionMapV2::KEY => &mut seen_v2,
            _ => return Err(ReasoningIdentityRecord::invalid_record()),
        };
        if *seen {
            return Err(ReasoningIdentityRecord::invalid_record());
        }
        *seen = true;
    }
    Ok(())
}

/// A v2 map is stored beside, never inside or over, the frozen v1 map.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReasoningIdentitySessionMapV2 {
    version: u8,
    repository_id: String,
    session_id: String,
    #[serde(deserialize_with = "deserialize_reasoning_identity_records_v2")]
    records: std::collections::BTreeMap<String, ReasoningIdentityRecordV2>,
    mac: String,
}

fn deserialize_reasoning_identity_records_v2<'de, D>(
    deserializer: D,
) -> std::result::Result<std::collections::BTreeMap<String, ReasoningIdentityRecordV2>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct UniqueRecords;
    impl<'de> serde::de::Visitor<'de> for UniqueRecords {
        type Value = std::collections::BTreeMap<String, ReasoningIdentityRecordV2>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a bounded private reasoning identity v2 map")
        }

        fn visit_map<A>(self, mut access: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: serde::de::MapAccess<'de>,
        {
            let mut records = std::collections::BTreeMap::new();
            while let Some((slot, record)) =
                access.next_entry::<String, ReasoningIdentityRecordV2>()?
            {
                if records.len() >= REASONING_IDENTITY_SESSION_MAP_MAX_ENTRIES
                    || records.insert(slot, record).is_some()
                {
                    return Err(serde::de::Error::custom(
                        "invalid private reasoning identity map",
                    ));
                }
            }
            Ok(records)
        }
    }
    deserializer.deserialize_map(UniqueRecords)
}

impl ReasoningIdentitySessionMapV2 {
    pub(crate) const KEY: &str = "identity-v2";

    pub(crate) fn authentication_body(
        repository_id: &str,
        session_id: &str,
        records: &std::collections::BTreeMap<String, ReasoningIdentityRecordV2>,
    ) -> Result<Vec<u8>> {
        Self::validate_records(repository_id, session_id, records)?;
        let bytes = serde_json::to_vec(&(
            "reasoning-identity-session-map-v2",
            ReasoningIdentityRecordV2::VERSION,
            repository_id,
            session_id,
            records,
        ))
        .map_err(|_| ReasoningIdentityRecord::invalid_record())?;
        ReasoningIdentitySessionMap::validate_size(bytes.len())?;
        Ok(bytes)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn from_records_and_mac(
        repository_id: String,
        session_id: String,
        records: std::collections::BTreeMap<String, ReasoningIdentityRecordV2>,
        mac: String,
    ) -> Result<Self> {
        let map = Self {
            version: ReasoningIdentityRecordV2::VERSION,
            repository_id,
            session_id,
            records,
            mac,
        };
        map.validate_syntax()?;
        map.encode()?;
        Ok(map)
    }

    pub(crate) fn decode(text: &str) -> Result<Self> {
        ReasoningIdentitySessionMap::validate_size(text.len())?;
        let map: Self =
            serde_json::from_str(text).map_err(|_| ReasoningIdentityRecord::invalid_record())?;
        map.validate_syntax()?;
        Ok(map)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn encode(&self) -> Result<String> {
        self.validate_syntax()?;
        let text =
            serde_json::to_string(self).map_err(|_| ReasoningIdentityRecord::invalid_record())?;
        ReasoningIdentitySessionMap::validate_size(text.len())?;
        Ok(text)
    }

    pub(crate) fn records(&self) -> &std::collections::BTreeMap<String, ReasoningIdentityRecordV2> {
        &self.records
    }

    pub(crate) fn mac(&self) -> &str {
        &self.mac
    }

    pub(crate) fn validate_owner(&self, repository_id: &str, session_id: &str) -> Result<()> {
        if self.repository_id != repository_id || self.session_id != session_id {
            return Err(ReasoningIdentityRecord::invalid_record());
        }
        Ok(())
    }

    fn validate_syntax(&self) -> Result<()> {
        if self.version != ReasoningIdentityRecordV2::VERSION
            || !ReasoningIdentityRecord::valid_mac_syntax(
                crate::internal::ai::capture::key::ReasoningIdentityMacPurpose::SessionMapV2,
                &self.mac,
            )
        {
            return Err(ReasoningIdentityRecord::invalid_record());
        }
        Self::validate_records(&self.repository_id, &self.session_id, &self.records)
    }

    fn validate_records(
        repository_id: &str,
        session_id: &str,
        records: &std::collections::BTreeMap<String, ReasoningIdentityRecordV2>,
    ) -> Result<()> {
        use crate::internal::ai::capture::key::ReasoningIdentityMacPurpose;
        if [repository_id, session_id].iter().any(|value| {
            value.is_empty() || value.len() > MAX_KEY_LEN || value.chars().any(char::is_control)
        }) {
            return Err(ReasoningIdentityRecord::invalid_record());
        }
        if records.len() > REASONING_IDENTITY_SESSION_MAP_MAX_ENTRIES {
            return Err(ReasoningIdentityMapCapacity.into());
        }
        for (slot, record) in records {
            record.validate_syntax()?;
            let context = &record.body.context;
            if !ReasoningIdentityRecord::valid_mac_syntax(
                ReasoningIdentityMacPurpose::DescriptorV2,
                slot,
            ) || context.repository_id != repository_id
                || context.session_id != session_id
                || context.logical_turn_key != *slot
            {
                return Err(ReasoningIdentityRecord::invalid_record());
            }
        }
        Ok(())
    }
}

/// A single metadata entry as read back from the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataEntry {
    pub scope: String,
    pub target: String,
    pub key: String,
    pub value: String,
    pub value_type: String,
}

impl MetadataEntry {
    fn from_model(model: &metadata_kv::Model) -> Self {
        Self {
            scope: model.scope.clone(),
            target: model.target.clone(),
            key: model.key.clone(),
            value: model.value.clone(),
            value_type: model.value_type.clone(),
        }
    }
}

/// Validate a metadata key: non-empty, ≤ [`MAX_KEY_LEN`] bytes, no whitespace
/// or control characters. Branch/repo keys are exact, case-sensitive
/// identifiers; the REVISION scope matches keys ASCII case-insensitively
/// (the trailer convention — documented divergence).
pub fn validate_key(key: &str) -> std::result::Result<(), String> {
    if key.is_empty() {
        return Err("metadata key must not be empty".to_string());
    }
    if key.len() > MAX_KEY_LEN {
        return Err(format!("metadata key exceeds {MAX_KEY_LEN} bytes: '{key}'"));
    }
    if key.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!(
            "metadata key must not contain whitespace or control characters: '{key}'"
        ));
    }
    Ok(())
}

/// Validate a metadata value: ≤ [`MAX_VALUE_LEN`] bytes. The empty string is
/// legal and distinct from an absent key.
pub fn validate_value(value: &str) -> std::result::Result<(), String> {
    if value.len() > MAX_VALUE_LEN {
        return Err(format!(
            "metadata value exceeds {} bytes ({} given)",
            MAX_VALUE_LEN,
            value.len()
        ));
    }
    Ok(())
}

/// A metadata value's declared type (the `value_type` column reserved by the
/// 1.5 migration). The stored `value` is always TEXT: the decimal string for
/// `numeric`, standard base64 for `binary` (per the 1.5 design — raw payloads
/// therefore cap at ~3/4 of [`MAX_VALUE_LEN`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MetadataValueType {
    #[default]
    Text,
    Numeric,
    Binary,
}

impl MetadataValueType {
    pub fn as_str(self) -> &'static str {
        match self {
            MetadataValueType::Text => "text",
            MetadataValueType::Numeric => "numeric",
            MetadataValueType::Binary => "binary",
        }
    }
}

/// Validate a value against its declared type (set-time only — reads return
/// whatever the store carries). `numeric` accepts an i64 or a FINITE f64
/// (rejects empty/NaN/inf/overflow and surrounding whitespace), stored
/// exactly as given; `binary` must be valid standard base64 (an empty payload
/// is legal). Both remain bounded by [`validate_value`] on the stored text.
pub fn validate_typed_value(
    value_type: MetadataValueType,
    value: &str,
) -> std::result::Result<(), String> {
    match value_type {
        MetadataValueType::Text => Ok(()),
        MetadataValueType::Numeric => {
            // STRICT: no surrounding whitespace — the exact string that
            // validates is the exact string stored (no trim-then-store skew).
            let ok = value.parse::<i64>().is_ok()
                || value.parse::<f64>().is_ok_and(|number| number.is_finite());
            if ok {
                Ok(())
            } else {
                Err(format!(
                    "invalid --numeric value '{value}' (expected an integer or finite decimal, no surrounding whitespace)"
                ))
            }
        }
        MetadataValueType::Binary => {
            use base64::Engine;
            if base64::engine::general_purpose::STANDARD
                .decode(value)
                .is_ok()
            {
                Ok(())
            } else {
                // Do NOT echo the value: it can be up to 1 MiB of arbitrary
                // (possibly secret) data — report only its length.
                Err(format!(
                    "invalid --binary value ({} bytes given; expected standard base64 — raw payloads cap at ~{}KiB)",
                    value.len(),
                    MAX_VALUE_LEN * 3 / 4 / 1024
                ))
            }
        }
    }
}

/// Whether a recorded flag value counts as SET, parsed FAIL-CLOSED: the
/// explicit falsy spellings (`false`/`0`/`no`/`off`, case-insensitive,
/// trimmed) count as off; EVERYTHING else — including garbage — counts as on,
/// so a corrupted value can never silently disable protection.
fn truthy_fail_closed(value: &str) -> bool {
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "false" | "0" | "no" | "off"
    )
}

/// The single owner API for the `metadata_kv` table.
/// `metadata_kv` stays in `libra.db`. Capture code paths may hold a
/// connection to `.libra/private/capture.db`, which has the catalog but no
/// `metadata_kv`. For such a connection this returns a fresh connection to
/// the paired repository database (same storage); otherwise `None`. A
/// fresh connection (not the cached one-connection pool) is used because a
/// caller may already hold a write transaction on the cached handle. Writes
/// through the peer commit separately: the two files are never attached.
pub(crate) async fn repository_metadata_peer<C: ConnectionTrait>(
    db: &C,
) -> Result<Option<sea_orm::DatabaseConnection>> {
    // Only the capture catalog qualifies: catalog tables and neither
    // `metadata_kv` nor `config_kv`. A damaged repository database that
    // lacks `metadata_kv` keeps failing on its own instead of pairing with
    // itself.
    if crate::internal::db::connection_has_table(db, "metadata_kv")
        .await
        .context("failed to inspect the metadata database")?
        || crate::internal::db::connection_has_table(db, "config_kv")
            .await
            .context("failed to inspect the metadata database")?
        || !crate::internal::db::connection_has_table(db, "agent_session")
            .await
            .context("failed to inspect the metadata database")?
    {
        return Ok(None);
    }
    let peer = crate::internal::db::open_paired_repository(db)
        .await
        .context("failed to open the repository database for capture metadata")?;
    Ok(Some(peer))
}

pub struct MetadataKv;

impl MetadataKv {
    /// Private bounded reader. SQLite checks byte length before returning the
    /// value, so corrupted rows cannot allocate unbounded buffers in callers.
    /// `key_range` is half-open and uses the existing scope/target/key index.
    pub(crate) async fn list_bounded_with_conn<C: ConnectionTrait>(
        db: &C,
        scopes: &[MetadataScope],
        target: Option<&str>,
        key_range: Option<(&str, &str)>,
        limit: u64,
        value_cap: usize,
    ) -> Result<Vec<MetadataEntry>> {
        if let Some(peer) = repository_metadata_peer(db).await? {
            return Box::pin(Self::list_bounded_with_conn(
                &peer, scopes, target, key_range, limit, value_cap,
            ))
            .await;
        }
        anyhow::ensure!(
            !scopes.is_empty()
                && scopes.len() <= 3
                && limit > 0
                && limit <= 129
                && value_cap <= MAX_VALUE_LEN,
            "invalid bounded metadata read; inspect the capture recovery configuration"
        );
        // Keys are capped like values: an out-of-band oversized key is never
        // hydrated into the caller, only reported through the safe-limit error.
        let mut values: Vec<sea_orm::Value> =
            vec![(MAX_KEY_LEN as i64).into(), (value_cap as i64).into()];
        let placeholders = scopes.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let mut sql = format!(
            "SELECT scope, target,
             CASE WHEN length(CAST(key AS BLOB)) <= ? THEN key ELSE NULL END AS bounded_key,
             value_type,
             CASE WHEN length(CAST(value AS BLOB)) <= ? THEN value ELSE NULL END AS bounded_value
             FROM metadata_kv WHERE scope IN ({placeholders})"
        );
        values.extend(scopes.iter().map(|scope| scope.as_str().into()));
        if let Some(target) = target {
            sql.push_str(" AND target = ?");
            values.push(target.into());
        }
        if let Some((lower, upper)) = key_range {
            sql.push_str(" AND key >= ? AND key < ?");
            values.extend([lower.into(), upper.into()]);
        }
        sql.push_str(" ORDER BY target, key, scope LIMIT ?");
        values.push((limit as i64).into());
        db.query_all_raw(sea_orm::Statement::from_sql_and_values(
            db.get_database_backend(),
            sql,
            values,
        ))
        .await
        .context("cannot read bounded capture metadata; run `libra agent doctor`")?
        .into_iter()
        .map(|row| {
            let key: Option<String> = row.try_get_by("bounded_key")?;
            let value: Option<String> = row.try_get_by("bounded_value")?;
            Ok(MetadataEntry {
                scope: row.try_get_by("scope")?,
                target: row.try_get_by("target")?,
                key: key.context(
                    "capture metadata key exceeds its safe size limit; run `libra agent doctor`",
                )?,
                value_type: row.try_get_by("value_type")?,
                value: value.context(
                    "capture metadata exceeds its safe size limit; run `libra agent doctor`",
                )?,
            })
        })
        .collect()
    }

    /// Get one entry, or `None` when absent.
    pub async fn get_with_conn<C: ConnectionTrait>(
        db: &C,
        scope: MetadataScope,
        target: &str,
        key: &str,
    ) -> Result<Option<MetadataEntry>> {
        if let Some(peer) = repository_metadata_peer(db).await? {
            return Box::pin(Self::get_with_conn(&peer, scope, target, key)).await;
        }
        let row = metadata_kv::Entity::find()
            .filter(metadata_kv::Column::Scope.eq(scope.as_str()))
            .filter(metadata_kv::Column::Target.eq(target))
            .filter(metadata_kv::Column::Key.eq(key))
            .one(db)
            .await
            .context("failed to query metadata_kv")?;
        Ok(row.as_ref().map(MetadataEntry::from_model))
    }

    /// Pool-acquiring counterpart of [`Self::get_with_conn`].
    pub async fn get(
        scope: MetadataScope,
        target: &str,
        key: &str,
    ) -> Result<Option<MetadataEntry>> {
        let db = get_db_conn_instance().await;
        Self::get_with_conn(&db, scope, target, key).await
    }

    /// Upsert one entry atomically (`INSERT … ON CONFLICT DO UPDATE` on the
    /// `(scope, target, key)` unique key — no find-then-insert race). Returns
    /// the PREVIOUS value when the key already existed.
    pub async fn set_with_conn<C: ConnectionTrait>(
        db: &C,
        scope: MetadataScope,
        target: &str,
        key: &str,
        value: &str,
        value_type: MetadataValueType,
    ) -> Result<Option<String>> {
        if let Some(peer) = repository_metadata_peer(db).await? {
            return Box::pin(Self::set_with_conn(
                &peer, scope, target, key, value, value_type,
            ))
            .await;
        }
        let previous = Self::get_with_conn(db, scope, target, key)
            .await?
            .map(|entry| entry.value);
        let now = Utc::now().to_rfc3339();
        let active = metadata_kv::ActiveModel {
            scope: Set(scope.as_str().to_string()),
            target: Set(target.to_string()),
            key: Set(key.to_string()),
            value: Set(value.to_string()),
            value_type: Set(value_type.as_str().to_string()),
            created_at: Set(now.clone()),
            updated_at: Set(now),
            ..Default::default()
        };
        let on_conflict = OnConflict::columns([
            metadata_kv::Column::Scope,
            metadata_kv::Column::Target,
            metadata_kv::Column::Key,
        ])
        .update_columns([
            metadata_kv::Column::Value,
            metadata_kv::Column::ValueType,
            metadata_kv::Column::UpdatedAt,
        ])
        .to_owned();
        metadata_kv::Entity::insert(active)
            .on_conflict(on_conflict)
            .exec(db)
            .await
            .context("failed to upsert metadata_kv entry")?;
        Ok(previous)
    }

    /// Pool-acquiring counterpart of [`Self::set_with_conn`].
    pub async fn set(
        scope: MetadataScope,
        target: &str,
        key: &str,
        value: &str,
        value_type: MetadataValueType,
    ) -> Result<Option<String>> {
        let db = get_db_conn_instance().await;
        Self::set_with_conn(&db, scope, target, key, value, value_type).await
    }

    /// Delete one entry; returns whether a row was removed.
    pub async fn unset_with_conn<C: ConnectionTrait>(
        db: &C,
        scope: MetadataScope,
        target: &str,
        key: &str,
    ) -> Result<bool> {
        if let Some(peer) = repository_metadata_peer(db).await? {
            return Box::pin(Self::unset_with_conn(&peer, scope, target, key)).await;
        }
        let result = metadata_kv::Entity::delete_many()
            .filter(metadata_kv::Column::Scope.eq(scope.as_str()))
            .filter(metadata_kv::Column::Target.eq(target))
            .filter(metadata_kv::Column::Key.eq(key))
            .exec(db)
            .await
            .context("failed to delete metadata_kv entry")?;
        Ok(result.rows_affected > 0)
    }

    /// Pool-acquiring counterpart of [`Self::unset_with_conn`].
    pub async fn unset(scope: MetadataScope, target: &str, key: &str) -> Result<bool> {
        let db = get_db_conn_instance().await;
        Self::unset_with_conn(&db, scope, target, key).await
    }

    /// List a target's entries, key-ordered, optionally filtered to a key
    /// prefix.
    pub async fn list_with_conn<C: ConnectionTrait>(
        db: &C,
        scope: MetadataScope,
        target: &str,
        key_prefix: Option<&str>,
    ) -> Result<Vec<MetadataEntry>> {
        if let Some(peer) = repository_metadata_peer(db).await? {
            return Box::pin(Self::list_with_conn(&peer, scope, target, key_prefix)).await;
        }
        let mut query = metadata_kv::Entity::find()
            .filter(metadata_kv::Column::Scope.eq(scope.as_str()))
            .filter(metadata_kv::Column::Target.eq(target));
        if let Some(prefix) = key_prefix {
            query = query.filter(metadata_kv::Column::Key.starts_with(prefix));
        }
        let rows = query
            .order_by_asc(metadata_kv::Column::Key)
            .all(db)
            .await
            .context("failed to list metadata_kv entries")?;
        Ok(rows.iter().map(MetadataEntry::from_model).collect())
    }

    /// Pool-acquiring counterpart of [`Self::list_with_conn`].
    pub async fn list(
        scope: MetadataScope,
        target: &str,
        key_prefix: Option<&str>,
    ) -> Result<Vec<MetadataEntry>> {
        let db = get_db_conn_instance().await;
        Self::list_with_conn(&db, scope, target, key_prefix).await
    }

    /// List EVERY entry in a scope across all targets, ordered by
    /// `(target, key)`. Needed by scopes whose consumers scan the whole
    /// namespace rather than one target (e.g. the prune side of
    /// [`MetadataScope::AgentTracesInflight`], which must see the in-flight
    /// markers of every session).
    pub async fn list_scope_with_conn<C: ConnectionTrait>(
        db: &C,
        scope: MetadataScope,
    ) -> Result<Vec<MetadataEntry>> {
        if let Some(peer) = repository_metadata_peer(db).await? {
            return Box::pin(Self::list_scope_with_conn(&peer, scope)).await;
        }
        let rows = metadata_kv::Entity::find()
            .filter(metadata_kv::Column::Scope.eq(scope.as_str()))
            .order_by_asc(metadata_kv::Column::Target)
            .order_by_asc(metadata_kv::Column::Key)
            .all(db)
            .await
            .context("failed to list metadata_kv scope entries")?;
        Ok(rows.iter().map(MetadataEntry::from_model).collect())
    }

    /// Delete every entry for a target (branch-delete cascade). Returns the
    /// number of rows removed.
    pub async fn delete_all_for_target_with_conn<C: ConnectionTrait>(
        db: &C,
        scope: MetadataScope,
        target: &str,
    ) -> Result<u64> {
        if let Some(peer) = repository_metadata_peer(db).await? {
            return Box::pin(Self::delete_all_for_target_with_conn(&peer, scope, target)).await;
        }
        let result = metadata_kv::Entity::delete_many()
            .filter(metadata_kv::Column::Scope.eq(scope.as_str()))
            .filter(metadata_kv::Column::Target.eq(target))
            .exec(db)
            .await
            .context("failed to cascade-delete metadata_kv entries")?;
        Ok(result.rows_affected)
    }

    /// Move a target's entries to a new target name (branch rename). Any
    /// pre-existing rows under the destination are removed first so the
    /// `(scope, target, key)` unique key cannot abort mid-move (defensive —
    /// the branch CLI refuses to rename onto an existing branch).
    pub async fn rename_target_with_conn<C: ConnectionTrait>(
        db: &C,
        scope: MetadataScope,
        old_target: &str,
        new_target: &str,
    ) -> Result<()> {
        if let Some(peer) = repository_metadata_peer(db).await? {
            return Box::pin(Self::rename_target_with_conn(
                &peer, scope, old_target, new_target,
            ))
            .await;
        }
        Self::delete_all_for_target_with_conn(db, scope, new_target).await?;
        metadata_kv::Entity::update_many()
            .col_expr(
                metadata_kv::Column::Target,
                sea_orm::sea_query::Expr::value(new_target),
            )
            .filter(metadata_kv::Column::Scope.eq(scope.as_str()))
            .filter(metadata_kv::Column::Target.eq(old_target))
            .exec(db)
            .await
            .context("failed to move metadata_kv entries to the renamed target")?;
        Ok(())
    }

    /// Copy a target's entries to another target (branch copy). Destination
    /// rows are replaced (matching `branch -C`'s overwrite semantics).
    pub async fn copy_target_with_conn<C: ConnectionTrait>(
        db: &C,
        scope: MetadataScope,
        from_target: &str,
        to_target: &str,
    ) -> Result<()> {
        if let Some(peer) = repository_metadata_peer(db).await? {
            return Box::pin(Self::copy_target_with_conn(
                &peer,
                scope,
                from_target,
                to_target,
            ))
            .await;
        }
        // Self-copy (`branch -C x x`) must be a no-op: clearing the
        // destination first would delete the source rows and then copy
        // nothing, silently losing the metadata.
        if from_target == to_target {
            return Ok(());
        }
        Self::delete_all_for_target_with_conn(db, scope, to_target).await?;
        let entries = Self::list_with_conn(db, scope, from_target, None).await?;
        let now = Utc::now().to_rfc3339();
        for entry in entries {
            let active = metadata_kv::ActiveModel {
                scope: Set(entry.scope),
                target: Set(to_target.to_string()),
                key: Set(entry.key),
                value: Set(entry.value),
                value_type: Set(entry.value_type),
                created_at: Set(now.clone()),
                updated_at: Set(now.clone()),
                ..Default::default()
            };
            active
                .insert(db)
                .await
                .context("failed to copy metadata_kv entry")?;
        }
        Ok(())
    }

    /// Whether the branch is recorded as protected — the read entry the future
    /// branch-policy layer (lore.md 1.13) calls inside its authoritative
    /// transaction. FAIL-CLOSED: any value other than the explicit falsy
    /// spellings counts as protected.
    pub async fn is_protected_with_conn<C: ConnectionTrait>(db: &C, branch: &str) -> Result<bool> {
        Ok(
            Self::get_with_conn(db, MetadataScope::Branch, branch, KEY_PROTECT)
                .await?
                .is_some_and(|entry| truthy_fail_closed(&entry.value)),
        )
    }

    /// Whether the branch is recorded as archived (fail-closed, like
    /// [`Self::is_protected_with_conn`]).
    pub async fn is_archived_with_conn<C: ConnectionTrait>(db: &C, branch: &str) -> Result<bool> {
        Ok(
            Self::get_with_conn(db, MetadataScope::Branch, branch, KEY_ARCHIVE)
                .await?
                .is_some_and(|entry| truthy_fail_closed(&entry.value)),
        )
    }
}

// ─── Revision-scope metadata (lore.md §1.10) ────────────────────────────────
//
// Commits are immutable, so revision metadata is TWO layers behind this one
// owner API: the commit message's qualifying TRAILER block (read-only, parsed
// by the 1.9 engine) and a mutable, metadata-owned NOTES document under
// [`REVISION_NOTES_REF`] — one note blob per commit holding a versioned JSON
// doc. No new table, no migration: the doc rides the existing `notes` store
// (lore.md 1.10 explicitly directs "revision 用 trailers/notes", which is the
// recorded resolution of the §3.6 unified-table red line for this scope).
// Reads merge both layers with NOTES WINNING (the only mutable layer must be
// able to override a baked-in trailer). Key matching is ASCII
// case-insensitive across BOTH layers (the trailer convention) — a documented
// divergence from the exact-match branch/repo scopes. Note-layer values are
// LOCAL-ONLY (notes are not pushed); trailer values travel with the commit.

/// The metadata-owned notes ref. `libra notes --ref metadata` operating on the
/// same doc is an intended dual surface (like `config` for `--repo`).
pub const REVISION_NOTES_REF: &str = "refs/notes/metadata";

const REVISION_DOC_VERSION: u32 = 1;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct RevisionDoc {
    version: u32,
    /// BTreeMap for deterministic serialization ordering.
    entries: std::collections::BTreeMap<String, RevisionDocEntry>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct RevisionDocEntry {
    value: String,
    #[serde(rename = "type")]
    value_type: String,
}

/// Where a revision metadata value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionSource {
    /// The mutable notes layer (local-only).
    Note,
    /// The commit message's immutable trailer block.
    Trailer,
}

impl RevisionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            RevisionSource::Note => "note",
            RevisionSource::Trailer => "trailer",
        }
    }
}

/// One merged revision-metadata entry.
#[derive(Debug, Clone)]
pub struct RevisionEntry {
    pub key: String,
    pub value: String,
    pub value_type: String,
    pub source: RevisionSource,
}

/// Outcome of [`MetadataKv::revision_unset`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionUnsetOutcome {
    /// A note entry was removed; no same-key trailer remains.
    Removed,
    /// A note entry was removed, but a same-key TRAILER value is now visible
    /// again (surface a notice).
    RemovedTrailerRemains,
    /// The key exists only as an immutable trailer — nothing to remove.
    OnlyTrailer,
    /// The key is absent from both layers.
    Absent,
}

fn corrupt_doc_error(oid: &str, detail: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "the revision metadata note for {oid} under {REVISION_NOTES_REF} is not a valid \
         metadata document ({detail}); repair or remove it with \
         'libra notes remove --ref metadata {oid}' (a hand-edited note via the dual \
         surface can cause this)"
    )
}

async fn load_revision_doc(oid: &str) -> Result<Option<RevisionDoc>> {
    Ok(load_revision_doc_with_blob(oid).await?.map(|(doc, _)| doc))
}

/// [`load_revision_doc`] plus the note BLOB hash, so a caller about to delete
/// the note can verify it still is the doc it loaded (see `revision_unset`).
async fn load_revision_doc_with_blob(oid: &str) -> Result<Option<(RevisionDoc, String)>> {
    match crate::internal::notes::show(REVISION_NOTES_REF, Some(oid)).await {
        Ok((_object, blob, text)) => {
            // Enforce the whole-doc bound on EXISTING docs too — a hand-edited
            // note via the dual surface must not bypass it (and must not feed
            // an unbounded blob to the JSON parser).
            if text.len() > MAX_VALUE_LEN {
                return Err(corrupt_doc_error(
                    oid,
                    &format!(
                        "document is {} bytes, over the {} byte bound",
                        text.len(),
                        MAX_VALUE_LEN
                    ),
                ));
            }
            let doc: RevisionDoc = serde_json::from_str(&text)
                .map_err(|error| corrupt_doc_error(oid, &error.to_string()))?;
            if doc.version != REVISION_DOC_VERSION {
                return Err(corrupt_doc_error(
                    oid,
                    &format!(
                        "unsupported document version {} (this binary supports {})",
                        doc.version, REVISION_DOC_VERSION
                    ),
                ));
            }
            Ok(Some((doc, blob)))
        }
        Err(crate::internal::notes::NotesError::NotFound { .. }) => Ok(None),
        Err(error) => Err(anyhow::anyhow!(
            "failed to read the revision metadata note for {oid}: {error}"
        )),
    }
}

async fn store_revision_doc(oid: &str, doc: &RevisionDoc) -> Result<()> {
    let text = serde_json::to_string_pretty(doc).context("failed to serialize metadata doc")?;
    // Bound the WHOLE document, not just individual values, so a commit's
    // metadata note cannot grow into a multi-MiB blob.
    if text.len() > MAX_VALUE_LEN {
        return Err(anyhow::anyhow!(
            "the revision metadata document for {oid} would exceed {} bytes ({} after this \
             change); remove entries first",
            MAX_VALUE_LEN,
            text.len()
        ));
    }
    crate::internal::notes::add(REVISION_NOTES_REF, oid, &text, true)
        .await
        .map_err(|error| anyhow::anyhow!("failed to write the revision metadata note: {error}"))?;
    Ok(())
}

impl MetadataKv {
    /// Get one revision metadata entry: the notes layer wins; otherwise the
    /// LAST matching trailer (the requested key is passed as an extra
    /// RECOGNIZED key so a mixed final block containing `<key>: v` qualifies —
    /// the 1.9 hook built for this).
    pub async fn revision_get(
        oid: &str,
        commit_message: &str,
        key: &str,
    ) -> Result<Option<RevisionEntry>> {
        if let Some(doc) = load_revision_doc(oid).await? {
            let hit = doc
                .entries
                .iter()
                .find(|(stored, _)| stored.eq_ignore_ascii_case(key));
            if let Some((stored_key, entry)) = hit {
                return Ok(Some(RevisionEntry {
                    key: stored_key.clone(),
                    value: entry.value.clone(),
                    value_type: entry.value_type.clone(),
                    source: RevisionSource::Note,
                }));
            }
        }
        let trailers =
            crate::internal::log::trailer::parse_trailers_with_recognized(commit_message, &[key]);
        Ok(trailers
            .iter()
            .rev()
            .find(|trailer| trailer.key_matches(key))
            .map(|trailer| RevisionEntry {
                key: trailer.key.clone(),
                value: trailer.value.clone(),
                value_type: "text".to_string(),
                source: RevisionSource::Trailer,
            }))
    }

    /// Set one revision metadata entry in the NOTES layer (the commit itself is
    /// never mutated). Returns the previous NOTE value (a shadowed trailer was
    /// never in the mutable layer, so it is not "previous"). Read-modify-write
    /// of the JSON doc; concurrent writers can lose an update (documented v1
    /// limitation, same class as the branch-scope pool-connection races).
    pub async fn revision_set(
        oid: &str,
        key: &str,
        value: &str,
        value_type: MetadataValueType,
    ) -> Result<Option<String>> {
        let mut doc = load_revision_doc(oid).await?.unwrap_or(RevisionDoc {
            version: REVISION_DOC_VERSION,
            entries: Default::default(),
        });
        // Case-insensitive upsert: drop any case-variant, keep the new spelling.
        let previous_key = doc
            .entries
            .keys()
            .find(|stored| stored.eq_ignore_ascii_case(key))
            .cloned();
        let previous = previous_key
            .and_then(|stored| doc.entries.remove(&stored))
            .map(|entry| entry.value);
        doc.entries.insert(
            key.to_string(),
            RevisionDocEntry {
                value: value.to_string(),
                value_type: value_type.as_str().to_string(),
            },
        );
        store_revision_doc(oid, &doc).await?;
        Ok(previous)
    }

    /// Remove one revision metadata entry from the NOTES layer. Trailer values
    /// are part of the immutable commit and cannot be unset.
    pub async fn revision_unset(
        oid: &str,
        commit_message: &str,
        key: &str,
    ) -> Result<RevisionUnsetOutcome> {
        let trailer_remains =
            crate::internal::log::trailer::parse_trailers_with_recognized(commit_message, &[key])
                .iter()
                .any(|trailer| trailer.key_matches(key));

        let Some((mut doc, loaded_blob)) = load_revision_doc_with_blob(oid).await? else {
            return Ok(if trailer_remains {
                RevisionUnsetOutcome::OnlyTrailer
            } else {
                RevisionUnsetOutcome::Absent
            });
        };
        let stored_key = doc
            .entries
            .keys()
            .find(|stored| stored.eq_ignore_ascii_case(key))
            .cloned();
        let Some(stored_key) = stored_key else {
            return Ok(if trailer_remains {
                RevisionUnsetOutcome::OnlyTrailer
            } else {
                RevisionUnsetOutcome::Absent
            });
        };
        doc.entries.remove(&stored_key);
        if doc.entries.is_empty() {
            // Deleting the whole note removes whatever blob is CURRENT — so
            // verify it still is the doc we loaded first, or a concurrent
            // `set` between our load and this delete would have its update
            // destroyed. (A residual window remains between this check and
            // the remove — same documented lost-update class as every
            // read-modify-write on this store — but the common race now gets
            // a retry instead of silent data loss.)
            match crate::internal::notes::show(REVISION_NOTES_REF, Some(oid)).await {
                Ok((_object, current_blob, _text)) if current_blob == loaded_blob => {}
                Ok(_) | Err(crate::internal::notes::NotesError::NotFound { .. }) => {
                    return Err(anyhow::anyhow!(
                        "the revision metadata note for {oid} changed while removing the last \
                         entry (concurrent writer?); re-run the command"
                    ));
                }
                Err(other) => {
                    return Err(anyhow::anyhow!(
                        "failed to re-verify the revision metadata note before removal: {other}"
                    ));
                }
            }
            crate::internal::notes::remove(REVISION_NOTES_REF, &[oid.to_string()])
                .await
                .map_err(|error| match error {
                    crate::internal::notes::NotesError::NotFound { .. } => anyhow::anyhow!(
                        "the revision metadata note for {oid} changed while removing the last \
                         entry (concurrent writer?); re-run the command"
                    ),
                    other => anyhow::anyhow!(
                        "failed to remove the emptied revision metadata note: {other}"
                    ),
                })?;
        } else {
            store_revision_doc(oid, &doc).await?;
        }
        Ok(if trailer_remains {
            RevisionUnsetOutcome::RemovedTrailerRemains
        } else {
            RevisionUnsetOutcome::Removed
        })
    }

    /// List merged revision metadata: note entries plus trailer occurrences
    /// (note shadows same-key trailers, ASCII case-insensitive). Sorted by key
    /// (case-insensitive); duplicate trailer keys keep message order. The
    /// trailer layer is parsed with the PLAIN rules here (the block must
    /// qualify on its own — the recognized-key strengthening applies to `get`
    /// only, documented). `key_prefix` filters case-insensitively (matching
    /// the scope's key semantics, unlike the branch scope's exact prefix).
    pub async fn revision_list(
        oid: &str,
        commit_message: &str,
        key_prefix: Option<&str>,
    ) -> Result<Vec<RevisionEntry>> {
        let doc = load_revision_doc(oid).await?;
        let mut entries: Vec<RevisionEntry> = Vec::new();
        if let Some(doc) = &doc {
            for (key, entry) in &doc.entries {
                entries.push(RevisionEntry {
                    key: key.clone(),
                    value: entry.value.clone(),
                    value_type: entry.value_type.clone(),
                    source: RevisionSource::Note,
                });
            }
        }
        for trailer in crate::internal::log::trailer::parse_trailers(commit_message) {
            let shadowed = doc.as_ref().is_some_and(|doc| {
                doc.entries
                    .keys()
                    .any(|stored| stored.eq_ignore_ascii_case(&trailer.key))
            });
            if !shadowed {
                entries.push(RevisionEntry {
                    key: trailer.key,
                    value: trailer.value,
                    value_type: "text".to_string(),
                    source: RevisionSource::Trailer,
                });
            }
        }
        if let Some(prefix) = key_prefix {
            let prefix = prefix.to_ascii_lowercase();
            entries.retain(|entry| entry.key.to_ascii_lowercase().starts_with(&prefix));
        }
        // Stable merged order: key (case-insensitive), notes before trailers
        // for equal keys, trailer duplicates keep message order (stable sort).
        entries.sort_by(|a, b| {
            a.key
                .to_ascii_lowercase()
                .cmp(&b.key.to_ascii_lowercase())
                .then_with(|| match (a.source, b.source) {
                    (RevisionSource::Note, RevisionSource::Trailer) => std::cmp::Ordering::Less,
                    (RevisionSource::Trailer, RevisionSource::Note) => std::cmp::Ordering::Greater,
                    _ => std::cmp::Ordering::Equal,
                })
        });
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {

    #[cfg(unix)]
    #[test]
    fn cov2_v1_identity_bytes_and_mac_frozen() {
        use std::{fs, os::unix::fs::PermissionsExt};

        use sha2::{Digest, Sha256};

        use crate::internal::ai::{
            capture::{
                checkpoint::{
                    reasoning_turn_descriptor_body_for_schema_version,
                    reasoning_turn_slot_body_for_schema_version,
                },
                key::{
                    ReasoningIdentityMacPurpose,
                    authenticate_reasoning_identity_existing_key_for_test,
                },
            },
            capture_scope::CaptureScope,
        };

        // These whole-byte SHA-256 and MAC answers were calculated from the
        // frozen identity-v1 serializer and tag/domain strings before adding
        // v2. The fixture key is synthetic, never a repository credential.
        let repo = "cov2-repo";
        let session = "cov2-session";
        let turn = "turn:alpha";
        let scope = CaptureScope {
            repo_id: repo.to_owned(),
            worktree_id: String::new(),
            workspace_id: None,
            workspace_fence: None,
        };
        let root = tempfile::tempdir().expect("create isolated v1 golden fixture");
        let storage = root.path().join("storage");
        let private = storage.join("private");
        fs::create_dir(&storage).expect("create fixture storage");
        fs::create_dir(&private).expect("create private key directory");
        fs::set_permissions(&private, fs::Permissions::from_mode(0o700))
            .expect("secure private directory");
        let key_path = private.join("agent-capture-dedup-v1.key");
        fs::write(&key_path, [b'B'; 32]).expect("write synthetic fixed key");
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))
            .expect("secure fixture key");

        let descriptor_purpose =
            ReasoningIdentityMacPurpose::descriptor_for_schema_version(1).unwrap();
        let record_purpose = ReasoningIdentityMacPurpose::record_for_schema_version(1).unwrap();
        let map_purpose = ReasoningIdentityMacPurpose::map_for_schema_version(1).unwrap();
        assert_eq!(map_purpose, record_purpose, "frozen v1 map uses Record MAC");
        let sign = |body: &[u8], purpose| {
            authenticate_reasoning_identity_existing_key_for_test(&storage, body, None, purpose)
                .expect("sign with the existing synthetic fixture key")
        };

        let slot = reasoning_turn_slot_body_for_schema_version(&scope, session, turn, 1)
            .expect("canonical v1 turn slot");
        let slot_mac = sign(&slot, descriptor_purpose);
        let entries = vec![(
            "native_proven",
            "issuer:test",
            "11".repeat(32),
            "22".repeat(32),
            "locator:test",
            "opencode",
            "signature",
            "33".repeat(32),
        )];
        let descriptor =
            reasoning_turn_descriptor_body_for_schema_version(&scope, session, turn, &entries, 1)
                .expect("canonical v1 descriptor");
        let descriptor_mac = sign(&descriptor, descriptor_purpose);
        let record_body = ReasoningIdentityRecordBody {
            context: ReasoningIdentityRecordContext {
                repository_id: repo.to_owned(),
                session_id: session.to_owned(),
                logical_turn_key: slot_mac.clone(),
                coverage_schema_version: 1,
                claim_revision: 1,
                claim_fence_token: 2,
                checkpoint_id: "checkpoint-1".to_owned(),
            },
            kind: ReasoningIdentityKind::Nonempty,
            descriptor_mac: descriptor_mac.clone(),
        };
        let record_bytes = ReasoningIdentityRecord::authentication_body(&record_body)
            .expect("canonical v1 record body");
        let mut unsupported = record_body.clone();
        unsupported.context.coverage_schema_version = 3;
        assert!(ReasoningIdentityRecord::authentication_body(&unsupported).is_err());
        let record_mac = sign(&record_bytes, record_purpose);
        let record = ReasoningIdentityRecord::from_body_and_mac(record_body, record_mac.clone())
            .expect("closed v1 record");
        let record_envelope = record.encode().expect("canonical v1 record envelope");
        let records = std::collections::BTreeMap::from([(slot_mac.clone(), record)]);
        let map_bytes = ReasoningIdentitySessionMap::authentication_body(repo, session, &records)
            .expect("canonical v1 whole-map body");
        let map_mac = sign(&map_bytes, map_purpose);
        let map = ReasoningIdentitySessionMap::from_records_and_mac(
            repo.to_owned(),
            session.to_owned(),
            records,
            map_mac.clone(),
        )
        .expect("closed v1 whole map");
        let map_envelope = map.encode().expect("canonical v1 map envelope");

        for (name, actual, expected) in [
            (
                "slot",
                slot.as_slice(),
                "e8778866139fbe1cedc22eb902ab11280e266a15d3eba04cd46e08148c66004d",
            ),
            (
                "descriptor",
                descriptor.as_slice(),
                "ba5af77054719ae0545db8c74c2abc90bcbf964e2e4f666744dc01434c37a6eb",
            ),
            (
                "record body",
                record_bytes.as_slice(),
                "dcd8fd0483e1d236de7318703277aae7b357ac1e2268718d26e2bcc69103de37",
            ),
            (
                "record envelope",
                record_envelope.as_bytes(),
                "80b9a8b709149ee2dc36048061cf8ebac1f688267ee5da757fede55c38a90dc2",
            ),
            (
                "map body",
                map_bytes.as_slice(),
                "595b44ca9f1ba5c6ded70368808c3b501db08f0695f8354acf5dc835c0ca7eb7",
            ),
            (
                "map envelope",
                map_envelope.as_bytes(),
                "05c413b0b1d1a3eba544fc7f0e5dcb71f10b3a67d4401510452e323cf0263d0d",
            ),
        ] {
            assert_eq!(
                hex::encode(Sha256::digest(actual)),
                expected,
                "{name} drifted"
            );
        }
        assert_eq!(
            slot_mac,
            "reasoning-identity/hmac-v1/06589d8f32484845f01b8a2b5bf879c60f84b3564087694550976057ede30227"
        );
        assert_eq!(
            descriptor_mac,
            "reasoning-identity/hmac-v1/d6abec939340a2efa90b8ba74c3327f42cd1aa7b60ab59ffdd623a5fee252aad"
        );
        assert_eq!(
            record_mac,
            "reasoning-record/hmac-v1/a3c522269ed8c5f22e37057d900a477bc53b6028bfaf64df00350ef20dcc7445"
        );
        assert_eq!(
            map_mac,
            "reasoning-record/hmac-v1/05462402ee2a44a026bee5d740cf9e7e23b19de9c0ae696946f02593eb0082ee"
        );
        assert_eq!(
            fs::read(key_path).expect("read unchanged fixture key"),
            [b'B'; 32]
        );
    }

    fn v2_private_map_fixture() -> (
        String,
        ReasoningIdentityRecordV2,
        ReasoningIdentitySessionMapV2,
    ) {
        use crate::internal::ai::capture::key::ReasoningIdentityMacPurpose;

        let slot = format!(
            "{}{}",
            ReasoningIdentityMacPurpose::DescriptorV2.prefix(),
            "a".repeat(64)
        );
        let body = ReasoningIdentityRecordBody {
            context: ReasoningIdentityRecordContext {
                repository_id: "repo".to_owned(),
                session_id: "session".to_owned(),
                logical_turn_key: slot.clone(),
                coverage_schema_version: 2,
                claim_revision: 1,
                claim_fence_token: 2,
                checkpoint_id: "checkpoint".to_owned(),
            },
            kind: ReasoningIdentityKind::Empty,
            descriptor_mac: format!(
                "{}{}",
                ReasoningIdentityMacPurpose::DescriptorV2.prefix(),
                "b".repeat(64)
            ),
        };
        let record = ReasoningIdentityRecordV2::from_body_and_mac(
            body,
            format!(
                "{}{}",
                ReasoningIdentityMacPurpose::RecordV2.prefix(),
                "c".repeat(64)
            ),
        )
        .expect("closed v2 record fixture");
        let map = ReasoningIdentitySessionMapV2::from_records_and_mac(
            "repo".to_owned(),
            "session".to_owned(),
            std::collections::BTreeMap::from([(slot.clone(), record.clone())]),
            format!(
                "{}{}",
                ReasoningIdentityMacPurpose::SessionMapV2.prefix(),
                "d".repeat(64)
            ),
        )
        .expect("closed v2 map fixture");
        (slot, record, map)
    }

    #[test]
    fn cov2_v1_v2_private_map_codecs() {
        use crate::internal::ai::capture::key::ReasoningIdentityMacPurpose;

        // Include the historical full-byte/MAC oracle in this exact selector.
        #[cfg(unix)]
        cov2_v1_identity_bytes_and_mac_frozen();

        let slot = format!(
            "{}{}",
            ReasoningIdentityMacPurpose::Descriptor.prefix(),
            "a".repeat(64)
        );
        let v1_body = ReasoningIdentityRecordBody {
            context: ReasoningIdentityRecordContext {
                repository_id: "repo".to_owned(),
                session_id: "session".to_owned(),
                logical_turn_key: slot.clone(),
                coverage_schema_version: 1,
                claim_revision: 1,
                claim_fence_token: 2,
                checkpoint_id: "checkpoint".to_owned(),
            },
            kind: ReasoningIdentityKind::Empty,
            descriptor_mac: format!(
                "{}{}",
                ReasoningIdentityMacPurpose::Descriptor.prefix(),
                "b".repeat(64)
            ),
        };
        let v1_record_mac = format!(
            "{}{}",
            ReasoningIdentityMacPurpose::Record.prefix(),
            "c".repeat(64)
        );
        let v1_record =
            ReasoningIdentityRecord::from_body_and_mac(v1_body.clone(), v1_record_mac.clone())
                .expect("frozen v1 fixture record");
        let expected_v1_record = format!(
            "{{\"version\":1,\"body\":{{\"context\":{{\"repository_id\":\"repo\",\"session_id\":\"session\",\"logical_turn_key\":\"{slot}\",\"coverage_schema_version\":1,\"claim_revision\":1,\"claim_fence_token\":2,\"checkpoint_id\":\"checkpoint\"}},\"kind\":\"empty\",\"descriptor_mac\":\"{}{}\"}},\"mac\":\"{v1_record_mac}\"}}",
            ReasoningIdentityMacPurpose::Descriptor.prefix(),
            "b".repeat(64)
        );
        assert_eq!(v1_record.encode().unwrap(), expected_v1_record);
        let mut v2_context_in_legacy_codec = v1_body.clone();
        v2_context_in_legacy_codec.context.coverage_schema_version = 2;
        assert!(ReasoningIdentityRecord::authentication_body(&v2_context_in_legacy_codec).is_err());
        assert!(
            ReasoningIdentityRecord::from_body_and_mac(
                v2_context_in_legacy_codec,
                v1_record_mac.clone(),
            )
            .is_err()
        );
        let v2_context_in_v1_envelope = expected_v1_record.replacen(
            "\"coverage_schema_version\":1",
            "\"coverage_schema_version\":2",
            1,
        );
        assert!(ReasoningIdentityRecord::decode(&v2_context_in_v1_envelope).is_err());
        let v1_map_mac = format!(
            "{}{}",
            ReasoningIdentityMacPurpose::Record.prefix(),
            "d".repeat(64)
        );
        let v1_map = ReasoningIdentitySessionMap::from_records_and_mac(
            "repo".to_owned(),
            "session".to_owned(),
            std::collections::BTreeMap::from([(slot.clone(), v1_record)]),
            v1_map_mac.clone(),
        )
        .expect("frozen v1 fixture map");
        assert_eq!(
            v1_map.encode().unwrap(),
            format!(
                "{{\"version\":1,\"repository_id\":\"repo\",\"session_id\":\"session\",\"records\":{{\"{slot}\":{expected_v1_record}}},\"mac\":\"{v1_map_mac}\"}}"
            )
        );
        assert_eq!(ReasoningIdentitySessionMap::KEY, "identity-v1");

        let (_, v2_record, v2_map) = v2_private_map_fixture();
        assert_eq!(ReasoningIdentitySessionMapV2::KEY, "identity-v2");
        let encoded_v2 = v2_map.encode().expect("encode v2 map");
        let decoded_v2 = ReasoningIdentitySessionMapV2::decode(&encoded_v2).expect("decode v2 map");
        assert_eq!(decoded_v2.records().len(), 1);
        assert_eq!(decoded_v2.mac(), v2_map.mac());
        assert!(decoded_v2.validate_owner("repo", "session").is_ok());
        assert!(decoded_v2.validate_owner("other", "session").is_err());
        assert!(
            v2_record
                .validate_context(&v2_record.body().context)
                .is_ok()
        );
        assert!(ReasoningIdentitySessionMap::decode(&encoded_v2).is_err());
        assert!(ReasoningIdentitySessionMapV2::decode(&v1_map.encode().unwrap()).is_err());

        let v2_record_bytes = ReasoningIdentityRecordV2::authentication_body(v2_record.body())
            .expect("v2 record authentication body");
        assert!(v2_record_bytes.starts_with(b"[\"reasoning-identity-record-v2\",2,"));
        let v2_map_bytes = ReasoningIdentitySessionMapV2::authentication_body(
            "repo",
            "session",
            decoded_v2.records(),
        )
        .expect("v2 map authentication body");
        assert!(v2_map_bytes.starts_with(b"[\"reasoning-identity-session-map-v2\",2,"));
        assert_ne!(
            ReasoningIdentityRecord::authentication_body(&v1_body).unwrap(),
            v2_record_bytes
        );
        assert_ne!(
            ReasoningIdentitySessionMap::authentication_body("repo", "session", v1_map.records())
                .unwrap(),
            v2_map_bytes
        );
    }

    #[test]
    fn cov2_private_map_duplicate_and_unknown_reject() {
        assert!(
            validate_reasoning_identity_private_map_keys([
                ReasoningIdentitySessionMap::KEY,
                ReasoningIdentitySessionMapV2::KEY,
            ])
            .is_ok()
        );
        for keys in [
            vec!["identity-v2", "identity-v2"],
            vec!["identity-v3"],
            vec!["identity-v1", "unknown"],
        ] {
            assert!(validate_reasoning_identity_private_map_keys(keys).is_err());
        }

        let (slot, record, map) = v2_private_map_fixture();
        let text = map.encode().expect("encode closed v2 map");
        let record_text = record.encode().expect("encode closed v2 record");
        let duplicate_slot = text.replacen(
            "\"records\":{",
            &format!("\"records\":{{\"{slot}\":{record_text},"),
            1,
        );
        assert!(ReasoningIdentitySessionMapV2::decode(&duplicate_slot).is_err());
        let duplicate_outer = text.replacen("\"version\":2", "\"version\":2,\"version\":2", 1);
        assert!(ReasoningIdentitySessionMapV2::decode(&duplicate_outer).is_err());
        let duplicate_record =
            record_text.replacen("\"version\":2", "\"version\":2,\"version\":2", 1);
        assert!(ReasoningIdentityRecordV2::decode(&duplicate_record).is_err());

        let mut value: serde_json::Value = serde_json::from_str(&text).expect("fixture JSON");
        value["unknown"] = "opaque-canary-never-in-errors".into();
        assert!(ReasoningIdentitySessionMapV2::decode(&value.to_string()).is_err());
        value.as_object_mut().unwrap().remove("unknown");
        value["version"] = 1.into();
        assert!(ReasoningIdentitySessionMapV2::decode(&value.to_string()).is_err());
        value["version"] = 2.into();
        value["records"][slot.as_str()]["body"]["context"]["coverage_schema_version"] = 1.into();
        assert!(ReasoningIdentitySessionMapV2::decode(&value.to_string()).is_err());
        value["records"][slot.as_str()]["body"]["context"]["coverage_schema_version"] = 2.into();
        value["mac"] = record.mac().into();
        assert!(ReasoningIdentitySessionMapV2::decode(&value.to_string()).is_err());
        assert!(
            ReasoningIdentitySessionMapV2::decode(
                &"x".repeat(REASONING_IDENTITY_SESSION_MAP_MAX_BYTES + 1)
            )
            .is_err()
        );
    }

    #[test]
    fn reasoning_identity_record_is_closed_and_context_bound() {
        use crate::internal::ai::capture::key::ReasoningIdentityMacPurpose;
        let context = ReasoningIdentityRecordContext {
            repository_id: "synthetic-repository".to_owned(),
            session_id: "synthetic-session".to_owned(),
            logical_turn_key: "synthetic-turn".to_owned(),
            coverage_schema_version: 1,
            claim_revision: 1,
            claim_fence_token: 2,
            checkpoint_id: "synthetic-checkpoint".to_owned(),
        };
        let body = ReasoningIdentityRecordBody {
            context: context.clone(),
            kind: ReasoningIdentityKind::Nonempty,
            descriptor_mac: format!(
                "{}{}",
                ReasoningIdentityMacPurpose::Descriptor.prefix(),
                "a".repeat(64)
            ),
        };
        let record = ReasoningIdentityRecord::from_body_and_mac(
            body.clone(),
            format!(
                "{}{}",
                ReasoningIdentityMacPurpose::Record.prefix(),
                "b".repeat(64)
            ),
        )
        .unwrap();
        let text = record.encode().unwrap();
        let decoded = ReasoningIdentityRecord::decode(&text).unwrap();
        assert!(decoded.validate_context(&context).is_ok());
        assert_eq!(decoded.body().kind, ReasoningIdentityKind::Nonempty);
        assert_eq!(decoded.mac(), record.mac());
        assert_eq!(
            ReasoningIdentityRecord::authentication_body(&body).unwrap(),
            ReasoningIdentityRecord::authentication_body(decoded.body()).unwrap()
        );
        for axis in 0..7 {
            let mut wrong = context.clone();
            match axis {
                0 => wrong.repository_id.push_str("-wrong"),
                1 => wrong.session_id.push_str("-wrong"),
                2 => wrong.logical_turn_key.push_str("-wrong"),
                3 => wrong.claim_revision += 1,
                4 => wrong.claim_fence_token += 1,
                5 => wrong.checkpoint_id.push_str("-wrong"),
                _ => wrong.coverage_schema_version += 1,
            }
            assert!(decoded.validate_context(&wrong).is_err());
        }
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        for axis in 0..8 {
            let mut wrong = value.clone();
            match axis {
                0 => wrong["version"] = 2.into(),
                1 => {
                    wrong["opaque-canary-never-in-errors"] = "synthetic-private-value".into();
                }
                2 => {
                    wrong["body"]["unknown"] = true.into();
                }
                3 => {
                    wrong["body"]["context"]["unknown"] = false.into();
                }
                4 => wrong["body"]["kind"] = "unknown".into(),
                5 => wrong["mac"] = body.descriptor_mac.clone().into(),
                6 => wrong["body"]["descriptor_mac"] = "not-a-domain-mac".into(),
                _ => {
                    wrong.as_object_mut().unwrap().remove("body");
                }
            }
            let error = ReasoningIdentityRecord::decode(&wrong.to_string()).unwrap_err();
            assert_eq!(
                error.to_string(),
                "private reasoning identity is invalid; start a new capture session"
            );
        }
        let duplicate = text.replacen("\"version\":1", "\"version\":1,\"version\":1", 1);
        assert!(ReasoningIdentityRecord::decode(&duplicate).is_err());
        let debug = format!("{record:?}");
        assert!(!debug.contains("synthetic-session"));
        assert!(!debug.contains("synthetic-turn"));
        assert!(!debug.contains(record.mac()));
        assert!(!debug.contains(&body.descriptor_mac));
        assert_eq!(
            MetadataScope::AgentReasoningIdentity.as_str(),
            "agent_reasoning_identity"
        );
        let slot = format!(
            "{}{}",
            ReasoningIdentityMacPurpose::Descriptor.prefix(),
            "c".repeat(64)
        );
        let mut slotted_body = body.clone();
        slotted_body.context.logical_turn_key = slot.clone();
        let slotted_record =
            ReasoningIdentityRecord::from_body_and_mac(slotted_body, record.mac().to_owned())
                .unwrap();
        let records = std::collections::BTreeMap::from([(slot.clone(), slotted_record.clone())]);
        let map = ReasoningIdentitySessionMap::from_records_and_mac(
            context.repository_id.clone(),
            context.session_id.clone(),
            records,
            record.mac().to_owned(),
        )
        .unwrap();
        let encoded = map.encode().unwrap();
        let decoded = ReasoningIdentitySessionMap::decode(&encoded).unwrap();
        assert!(
            decoded
                .validate_owner(&context.repository_id, &context.session_id)
                .is_ok()
        );
        assert!(
            decoded
                .validate_owner("another-repository", &context.session_id)
                .is_err()
        );
        assert!(
            decoded
                .validate_owner(&context.repository_id, "another-session")
                .is_err()
        );
        assert_eq!(decoded.records().len(), 1);
        assert_eq!(decoded.mac(), map.mac());
        let duplicate_slot = encoded.replacen(
            "\"records\":{",
            &format!(
                "\"records\":{{\"{slot}\":{},",
                slotted_record.encode().unwrap()
            ),
            1,
        );
        assert!(ReasoningIdentitySessionMap::decode(&duplicate_slot).is_err());
        let mut value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        value["unknown"] = "opaque-canary-never-in-errors".into();
        let error = ReasoningIdentitySessionMap::decode(&value.to_string())
            .err()
            .unwrap();
        assert_eq!(
            error.to_string(),
            "private reasoning identity is invalid; start a new capture session"
        );
        assert!(
            ReasoningIdentitySessionMap::decode(
                &"x".repeat(REASONING_IDENTITY_SESSION_MAP_MAX_BYTES + 1)
            )
            .is_err()
        );
        const {
            assert!(
                REASONING_IDENTITY_SESSION_MAP_MAX_BYTES
                    > crate::internal::ai::traces::REASONING_ARTIFACT_MANIFEST_MAX_BYTES
            );
        }
        assert!(
            ReasoningIdentitySessionMap::validate_size(
                REASONING_IDENTITY_SESSION_MAP_MAX_BYTES + 1
            )
            .is_err_and(|error| error.is::<ReasoningIdentityMapCapacity>())
        );
        let mut too_many = std::collections::BTreeMap::new();
        for ordinal in 0..=REASONING_IDENTITY_SESSION_MAP_MAX_ENTRIES {
            let slot = format!(
                "{}{:064x}",
                ReasoningIdentityMacPurpose::Descriptor.prefix(),
                ordinal
            );
            let mut body = body.clone();
            body.context.logical_turn_key = slot.clone();
            too_many.insert(
                slot,
                ReasoningIdentityRecord::from_body_and_mac(body, record.mac().to_owned()).unwrap(),
            );
        }
        assert!(
            ReasoningIdentitySessionMap::authentication_body(
                &context.repository_id,
                &context.session_id,
                &too_many
            )
            .is_err_and(|error| error.is::<ReasoningIdentityMapCapacity>())
        );
        // These synthetic tag strings validate codec syntax only. The key
        // owner and actual coverage/store tests must verify cryptographic MACs.
    }
    use super::*;

    #[test]
    fn key_validation_rejects_empty_whitespace_and_oversize() {
        assert!(validate_key("protect").is_ok());
        assert!(validate_key("lineage.parent").is_ok());
        assert!(validate_key("").is_err());
        assert!(validate_key("has space").is_err());
        assert!(validate_key("has\ttab").is_err());
        assert!(validate_key(&"k".repeat(MAX_KEY_LEN + 1)).is_err());
        assert!(validate_key(&"k".repeat(MAX_KEY_LEN)).is_ok());
    }

    #[test]
    fn value_validation_allows_empty_and_bounds_size() {
        assert!(validate_value("").is_ok());
        assert!(validate_value("v").is_ok());
        assert!(validate_value(&"v".repeat(MAX_VALUE_LEN)).is_ok());
        assert!(validate_value(&"v".repeat(MAX_VALUE_LEN + 1)).is_err());
    }

    #[test]
    fn typed_value_validation_matrix() {
        use MetadataValueType::*;
        for ok in ["42", "-7", "+1", "007", "3.14", "-0"] {
            assert!(validate_typed_value(Numeric, ok).is_ok(), "{ok:?}");
        }
        for bad in [
            "", "NaN", "inf", "1e999", "0x10", "1,000", "abc", " 12 ", "12 ",
        ] {
            assert!(validate_typed_value(Numeric, bad).is_err(), "{bad:?}");
        }
        for ok in ["", "aGVsbG8=", "AAAA"] {
            assert!(validate_typed_value(Binary, ok).is_ok(), "{ok:?}");
        }
        for bad in ["not base64!", "aGVsbG8", "%%%"] {
            assert!(validate_typed_value(Binary, bad).is_err(), "{bad:?}");
        }
        assert!(validate_typed_value(Text, "anything at all").is_ok());
    }

    #[test]
    fn truthy_parse_is_fail_closed() {
        for on in ["true", "1", "yes", "on", "TRUE", " weird-garbage ", ""] {
            assert!(truthy_fail_closed(on), "{on:?} must count as set");
        }
        for off in ["false", "0", "no", "off", "FALSE", " Off "] {
            assert!(!truthy_fail_closed(off), "{off:?} must count as unset");
        }
    }
}
