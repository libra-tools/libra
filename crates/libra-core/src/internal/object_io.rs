//! Core object read/write and target-commit resolution primitives sunk from
//! `command/mod.rs`.

use git_internal::{errors::GitError, hash::ObjectHash, internal::object::ObjectTrait};

use crate::{
    internal::{lazy_fetch, replace},
    utils::util,
};

/// Load an object by hash, applying `refs/replace/*` substitution and promisor
/// lazy fetch when missing locally.
#[doc(hidden)]
pub fn load_object<T>(hash: &ObjectHash) -> Result<T, GitError>
where
    T: ObjectTrait,
{
    let hash = replace::resolve(*hash);
    let storage = util::try_objects_storage().map_err(GitError::IOError)?;
    let data = match storage.get(&hash) {
        Ok(data) => data,
        Err(error) => match lazy_fetch::try_lazy_fetch_object(&hash) {
            Ok(true) => storage.get(&hash)?,
            Ok(false) => return Err(error),
            Err(unregistered) => return Err(GitError::CustomError(unregistered.to_string())),
        },
    };
    T::from_bytes(&data.to_vec(), hash)
}

/// RAW load: no `refs/replace` substitution and no promisor lazy fetch.
#[doc(hidden)]
pub fn load_object_raw<T>(hash: &ObjectHash) -> Result<T, GitError>
where
    T: ObjectTrait,
{
    let storage = util::try_objects_storage().map_err(GitError::IOError)?;
    let data = storage.get(hash)?;
    T::from_bytes(&data.to_vec(), *hash)
}

/// Persist an object into the repository object store.
#[doc(hidden)]
pub fn save_object<T>(object: &T, obj_id: &ObjectHash) -> Result<(), GitError>
where
    T: ObjectTrait,
{
    let storage = util::objects_storage()?;
    let data = object.to_data()?;
    storage.put(obj_id, &data, object.get_type())?;
    Ok(())
}

/// Resolve a branch name or commit revision to a commit [`ObjectHash`].
#[doc(hidden)]
pub async fn get_target_commit(
    branch_or_commit: &str,
) -> Result<ObjectHash, Box<dyn std::error::Error>> {
    util::get_commit_base(branch_or_commit)
        .await
        .map_err(|e| Box::new(std::io::Error::other(e)) as Box<dyn std::error::Error>)
}
