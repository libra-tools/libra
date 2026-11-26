mod loose;
mod pack;

use std::path::Path;

use git_internal::{errors::GitError, hash::ObjectHash, internal::object::types::ObjectType};

pub(super) fn read_loose(
    path: &Path,
    max_payload: Option<u64>,
) -> Result<(Vec<u8>, ObjectType), GitError> {
    loose::read(path, max_payload)
}

pub(super) fn loose_cost(path: &Path) -> Result<u64, GitError> {
    loose::load_cost(path)
}

pub(super) fn loose_decoded_size(path: &Path) -> Result<Option<u64>, GitError> {
    loose::decoded_size(path)
}

pub(super) fn pack_costs(
    pack_dir: &Path,
    hashes: &[ObjectHash],
) -> Result<Vec<Option<u64>>, GitError> {
    pack::load_costs(pack_dir, hashes)
}

pub(super) fn pack_costs_with_limit(
    pack_dir: &Path,
    hashes: &[ObjectHash],
    aggregate_limit: u64,
) -> Result<Vec<Option<u64>>, GitError> {
    pack::load_costs_with_limit(pack_dir, hashes, aggregate_limit)
}

pub(super) fn pack_decoded_sizes(
    pack_dir: &Path,
    hashes: &[ObjectHash],
) -> Result<Vec<Option<u64>>, GitError> {
    pack::decoded_sizes(pack_dir, hashes)
}
