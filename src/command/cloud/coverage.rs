//! Discover missing cloud index rows from the references included in the backup.

use std::collections::HashSet;

use git_internal::internal::object::types::ObjectType;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};

use super::{CloudError, CloudResult, object_index, reference};
use crate::{
    command::maintenance::{extend_object_references, parse_object_hash},
    utils::{client_storage::ClientStorage, util},
};

const PAGE_SIZE: usize = 256;

pub(super) async fn missing_rows<C: ConnectionTrait>(
    conn: &C,
    repo_id: &str,
) -> CloudResult<Vec<(String, String, i64)>> {
    let read_error = |error| CloudError::Generic(format!("read cloud object coverage: {error}"));
    let storage = ClientStorage::init_local_existing_with_alternates(
        util::try_get_storage_path(None)?.join("objects"),
    );
    let mut seen = HashSet::new();
    let mut missing = Vec::new();
    // Start with the newest refs so concurrent insertions cannot extend the walk.
    let mut before = None;
    loop {
        let mut query = reference::Entity::find().filter(reference::Column::Commit.is_not_null());
        if let Some(id) = before {
            query = query.filter(reference::Column::Id.lt(id));
        }
        let refs = query
            .select_only()
            .columns([reference::Column::Id, reference::Column::Commit])
            .order_by_desc(reference::Column::Id)
            .limit(PAGE_SIZE as u64)
            .into_tuple::<(i64, String)>()
            .all(conn)
            .await
            .map_err(read_error)?;
        if refs.is_empty() {
            break;
        }
        let mut stack = Vec::new();
        for (id, tip) in refs {
            before = Some(id);
            stack.push(parse_object_hash(&tip).ok_or_else(|| {
                CloudError::Generic(format!("invalid cloud reference target {tip}"))
            })?);
        }
        while !stack.is_empty() {
            let mut page = Vec::new();
            while page.len() < PAGE_SIZE
                && let Some(oid) = stack.pop()
            {
                if seen.insert(oid) {
                    page.push(oid);
                }
            }
            if page.is_empty() {
                continue;
            }
            let kinds = storage
                .get_object_types_bounded_many(&page)
                .map_err(|error| {
                    CloudError::Generic(format!("inspect reachable objects for backup: {error}"))
                })?;
            let indexed: HashSet<String> = object_index::Entity::find()
                .select_only()
                .column(object_index::Column::OId)
                .filter(object_index::Column::RepoId.eq(repo_id))
                .filter(object_index::Column::OId.is_in(page.iter().map(ToString::to_string)))
                .into_tuple()
                .all(conn)
                .await
                .map_err(read_error)?
                .into_iter()
                .collect();
            let mut gaps = Vec::new();
            for oid in page {
                let kind = kinds.get(&oid).ok_or_else(|| CloudError::Generic(format!("reachable object {oid} is missing; fetch the full history and hydrate missing objects before backing up")))?;
                if *kind != ObjectType::Blob {
                    let data = storage.get(&oid).map_err(|e| {
                        CloudError::Generic(format!("read reachable {kind} {oid}: {e}"))
                    })?;
                    // Restored metadata has no shallow boundary or promisor remote.
                    extend_object_references(oid, *kind, &data, false, &mut stack).map_err(
                        |e| CloudError::Generic(format!("read references in {kind} {oid}: {e}")),
                    )?;
                }
                if !indexed.contains(&oid.to_string()) {
                    gaps.push(oid);
                }
            }
            let sizes = storage
                .decoded_object_sizes(&gaps)
                .map_err(|e| CloudError::Generic(format!("read reachable object sizes: {e}")))?;
            for (oid, size) in gaps.into_iter().zip(sizes) {
                let size = size.and_then(|s| i64::try_from(s).ok()).ok_or_else(|| {
                    CloudError::Generic(format!(
                        "cannot determine the size of reachable object {oid}"
                    ))
                })?;
                missing.push((oid.to_string(), kinds[&oid].to_string(), size));
            }
        }
    }
    Ok(missing)
}
