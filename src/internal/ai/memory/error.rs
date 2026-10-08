//! Error type for the deterministic Episode projection.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("memory projection error: {0}")]
    Projection(String),
    #[error("git object error: {0}")]
    Git(#[from] git_internal::errors::GitError),
    #[error("change store error: {0}")]
    Change(#[from] crate::internal::change::ChangeStoreError),
    #[error("database error: {0}")]
    Database(#[from] sea_orm::DbErr),
}
