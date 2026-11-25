//! SeaORM entity for the `config_kv` flat key/value configuration table with optional vault encryption.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "config_kv")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true)]
    pub id: i64,
    /// Full dotted key, e.g. "remote.origin.url", "vault.env.GEMINI_API_KEY"
    pub key: String,
    /// Plain-text value, or vault-encrypted ciphertext (hex-encoded) when `encrypted == 1`
    pub value: String,
    /// 0 = plaintext, 1 = vault-encrypted
    pub encrypted: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
