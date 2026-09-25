use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};

use super::*;

fn calculate_metadata_hash(json: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    json.hash(&mut hasher);
    hasher.finish()
}

pub(super) async fn sync_metadata(
    db_conn: &sea_orm::DatabaseConnection,
    r2_storage: &RemoteStorage,
    progress: &dyn CloudSyncProgress,
) -> CloudResult<MetadataSyncOutcome> {
    progress.on_metadata_starting();
    let references = reference::Entity::find()
        .all(db_conn)
        .await
        .map_err(|e| CloudError::Generic(format!("Failed to fetch references: {}", e)))?;

    // Sort to ensure deterministic output for hashing.
    let mut sorted_refs = references;
    sorted_refs.sort_by(|a, b| {
        let a_kind = format!("{:?}", a.kind);
        let b_kind = format!("{:?}", b.kind);
        let a_key = (&a.name, &a.remote, a_kind);
        let b_key = (&b.name, &b.remote, b_kind);
        a_key.cmp(&b_key)
    });

    let json = serde_json::to_vec(&sorted_refs)
        .map_err(|e| CloudError::Generic(format!("Failed to serialize metadata: {}", e)))?;

    let current_hash = calculate_metadata_hash(&json);

    // Check if hash matches last sync.
    if let Some(stored) = ConfigKv::get("cloud.metadata_hash")
        .await
        .ok()
        .flatten()
        .map(|e| e.value)
        && let Ok(stored_hash) = stored.parse::<u64>()
        && stored_hash == current_hash
    {
        progress.on_metadata_skipped();
        return Ok(MetadataSyncOutcome::Skipped);
    }

    r2_storage
        .put_metadata(&json)
        .await
        .map_err(|e| CloudError::R2(format!("Failed to upload metadata: {}", e)))?;

    // Update stored hash.
    let _ = ConfigKv::set("cloud.metadata_hash", &current_hash.to_string(), false).await;

    progress.on_metadata_synced(sorted_refs.len());
    Ok(MetadataSyncOutcome::Synced {
        references: sorted_refs.len(),
    })
}
