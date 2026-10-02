//! Tag operations that resolve target objects, build annotated or lightweight tags, persist refs in the database, and write tag objects to storage.

use std::io;

use git_internal::{
    errors::GitError,
    hash::ObjectHash,
    internal::object::{
        ObjectTrait,
        blob::Blob,
        commit::Commit,
        signature::{Signature, SignatureType},
        tag::Tag as git_internalTag,
        tree::Tree,
        types::ObjectType,
    },
};
use sea_orm::{ActiveModelTrait, ColumnTrait, DbErr, EntityTrait, QueryFilter, Set};

use crate::{
    command::load_object,
    internal::{
        branch::BranchStoreError, config::ConfigKv, db::get_db_conn_instance, head::Head,
        model::reference,
    },
    utils::{client_storage::ClientStorage, path, util},
};

// Constants for tag references
const TAG_REF_PREFIX: &str = "refs/tags/";
const DEFAULT_USER: &str = "user";
const DEFAULT_EMAIL: &str = "user@example.com";
const UNKNOWN_TAG: &str = "<unknown>";

/// Enum representing the possible object types a tag can point to.
#[derive(Debug)]
pub enum TagObject {
    Commit(Commit),
    Tag(git_internalTag),
    Tree(Tree),
    Blob(Blob),
}

impl TagObject {
    pub fn get_type(&self) -> ObjectType {
        match self {
            TagObject::Commit(_) => ObjectType::Commit,
            TagObject::Tag(_) => ObjectType::Tag,
            TagObject::Tree(_) => ObjectType::Tree,
            TagObject::Blob(_) => ObjectType::Blob,
        }
    }

    pub fn to_data(&self) -> Result<Vec<u8>, GitError> {
        match self {
            TagObject::Commit(c) => c.to_data(),
            TagObject::Tag(t) => t.to_data(),
            TagObject::Tree(t) => t.to_data(),
            TagObject::Blob(b) => b.to_data(),
        }
    }
}

/// Represents a tag in the context of Libra, containing its name and the object it points to.
pub struct Tag {
    pub name: String,
    pub object: TagObject,
}

/// Lightweight snapshot of a tag reference row.
///
/// This intentionally preserves the raw ref target string so recovery paths
/// such as `tag -d` can operate even when the target object is missing or the
/// stored hash is invalid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagReference {
    pub target: Option<String>,
}

/// Semantic failures that can occur while creating a tag.
#[derive(Debug, thiserror::Error)]
pub enum CreateTagError {
    #[error("Cannot create tag: HEAD does not point to a commit")]
    HeadUnborn,
    #[error("failed to resolve HEAD commit: {0}")]
    ResolveHead(#[source] BranchStoreError),
    #[error("Tag '{0}' already exists")]
    AlreadyExists(String),
    #[error("failed to query existing tag refs: {0}")]
    CheckExisting(#[source] DbErr),
    #[error("failed to serialize annotated tag object: {0}")]
    SerializeTag(#[source] GitError),
    #[error("failed to store annotated tag object: {0}")]
    StoreObject(#[source] io::Error),
    #[error("failed to persist tag reference: {0}")]
    PersistReference(#[source] DbErr),
    #[error("failed to sign annotated tag: {0}")]
    VaultSign(String),
}

/// The object a new tag points at, unpeeled, together with its actual type:
/// a commit, or an annotated tag whose chain peels to a commit (the new tag is
/// then a nested tag).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TagTarget {
    pub id: ObjectHash,
    pub kind: ObjectType,
}

#[derive(Debug, Clone)]
pub struct CreateTagResult {
    pub name: String,
    pub target: ObjectHash,
    pub annotated: bool,
    pub message: Option<String>,
}

/// Creates a new tag, either lightweight or annotated, pointing to the current HEAD commit.
///
/// * `name` - The name of the tag.
/// * `message` - If `Some`, creates an annotated tag with the given message. If `None`, creates a lightweight tag.
pub async fn create(
    name: &str,
    message: Option<String>,
    force: bool,
    sign: bool,
) -> Result<CreateTagResult, CreateTagError> {
    create_with_target(name, message, force, sign, None).await
}

/// Like [`create`], but points the tag at `target` instead of HEAD when one is
/// given (`libra tag <name> <commit>`). A lightweight tag's ref stores
/// `target.id`; an annotated or signed tag object records `object <target.id>`
/// and `type <target.kind>`, and a signature covers exactly those header
/// values, so `tag -v` verifies what is stored. HEAD is not read at all when a
/// target is given, so tagging works on an unborn branch.
pub async fn create_with_target(
    name: &str,
    message: Option<String>,
    force: bool,
    sign: bool,
    target: Option<TagTarget>,
) -> Result<CreateTagResult, CreateTagError> {
    let target = match target {
        Some(target) => target,
        None => match Head::current_commit_result().await {
            Ok(Some(head_commit_id)) => TagTarget {
                id: head_commit_id,
                kind: ObjectType::Commit,
            },
            Ok(None) => return Err(CreateTagError::HeadUnborn),
            Err(source) => return Err(CreateTagError::ResolveHead(source)),
        },
    };

    let db = get_db_conn_instance().await;
    let exists = reference::Entity::find()
        .filter(reference::Column::Name.eq(format!("{}{}", TAG_REF_PREFIX, name)))
        .filter(reference::Column::Kind.eq(reference::ConfigKind::Tag))
        .one(&db)
        .await
        .map_err(CreateTagError::CheckExisting)?;

    if exists.is_some() && !force {
        return Err(CreateTagError::AlreadyExists(name.to_string()));
    }

    let ref_target_id: ObjectHash;
    let create_message = message.clone();
    if let Some(msg) = message {
        // Create an annotated tag object
        let user_name = ConfigKv::get("user.name")
            .await
            .ok()
            .flatten()
            .map(|e| e.value)
            .unwrap_or_else(|| DEFAULT_USER.to_string());
        let user_email = ConfigKv::get("user.email")
            .await
            .ok()
            .flatten()
            .map(|e| e.value)
            .unwrap_or_else(|| DEFAULT_EMAIL.to_string());
        let tagger_signature = Signature::new(SignatureType::Tagger, user_name, user_email);

        // `tag -s`: sign the canonical (unsigned) tag content with the vault PGP
        // key and append the armored signature block to the message, matching
        // Git's signed-tag layout. The tagger is built exactly once so the
        // signed bytes match the stored object.
        let msg = if sign {
            let unseal_key = crate::internal::vault::load_unseal_key()
                .await
                .ok_or_else(|| {
                    CreateTagError::VaultSign(
                        "tag signing requested but no vault unseal key was found".to_string(),
                    )
                })?;
            let unsigned_content = format!(
                "object {}\ntype {}\ntag {}\ntagger {}\n\n{}",
                target.id, target.kind, name, tagger_signature, msg
            );
            let root_dir = util::storage_path();
            let sig_hex = crate::internal::vault::pgp_sign(
                &root_dir,
                &unseal_key,
                unsigned_content.as_bytes(),
            )
            .await
            .map_err(|e| CreateTagError::VaultSign(format!("vault PGP signing failed: {e}")))?;
            let armored = crate::internal::vault::signature_to_armored(&sig_hex).map_err(|e| {
                CreateTagError::VaultSign(format!("failed to format PGP signature: {e}"))
            })?;
            format!("{msg}\n{armored}")
        } else {
            msg
        };

        let git_internal_tag = git_internalTag::new(
            target.id,
            target.kind,
            name.to_string(),
            tagger_signature,
            msg,
        );

        // The ID is now calculated inside git_internalTag::new, so we can use it directly.
        let tag_data = git_internal_tag
            .to_data()
            .map_err(CreateTagError::SerializeTag)?;
        let storage = ClientStorage::init(path::objects());
        storage
            .put(&git_internal_tag.id, &tag_data, git_internal_tag.get_type())
            .map_err(CreateTagError::StoreObject)?;

        ref_target_id = git_internal_tag.id;
    } else {
        // A lightweight tag's ref stores the target object itself.
        ref_target_id = target.id;
    };

    // Save the reference in the database
    let ref_target = ref_target_id.to_string();
    match exists {
        Some(existing_ref) => {
            let mut existing_ref: reference::ActiveModel = existing_ref.into();
            existing_ref.commit = Set(Some(ref_target));
            existing_ref
                .update(&db)
                .await
                .map_err(CreateTagError::PersistReference)?;
        }
        None => {
            let new_ref = reference::ActiveModel {
                name: Set(Some(format!("{}{}", TAG_REF_PREFIX, name))),
                kind: Set(reference::ConfigKind::Tag),
                commit: Set(Some(ref_target)),
                ..Default::default()
            };
            new_ref
                .insert(&db)
                .await
                .map_err(CreateTagError::PersistReference)?;
        }
    }

    Ok(CreateTagResult {
        name: name.to_string(),
        target: ref_target_id,
        annotated: create_message.is_some(),
        message: create_message,
    })
}

/// Marker that introduces the armored PGP signature appended to a signed tag's
/// message (see [`create`] with `sign = true`).
const TAG_SIGNATURE_MARKER: &str = "-----BEGIN PGP SIGNATURE-----";

/// Semantic failures while verifying a signed tag (`tag -v`).
#[derive(Debug, thiserror::Error)]
pub enum VerifyTagError {
    #[error("tag '{0}' not found")]
    NotFound(String),
    #[error("tag '{0}' is not an annotated tag")]
    NotAnnotated(String),
    #[error("tag '{0}' is not signed")]
    NotSigned(String),
    #[error("no vault unseal key found")]
    NoVaultKey,
    #[error("failed to load tag object: {0}")]
    LoadObject(String),
    #[error("vault verification failed: {0}")]
    Vault(String),
}

/// Verify the PGP signature embedded in an annotated tag (`tag -v`). Reconstructs
/// the canonical unsigned tag content (everything before the appended signature)
/// and checks it against the armored signature via the vault PGP key. Returns
/// `Ok(true)` for a good signature and `Ok(false)` for a bad one.
pub async fn verify(name: &str) -> Result<bool, VerifyTagError> {
    let tag_ref = find_tag_ref(name)
        .await
        .map_err(|e| VerifyTagError::LoadObject(e.to_string()))?
        .ok_or_else(|| VerifyTagError::NotFound(name.to_string()))?;
    let target = tag_ref
        .target
        .ok_or_else(|| VerifyTagError::NotFound(name.to_string()))?;
    let object_id = crate::internal::object_format::parse_repo_oid(&target)
        .map_err(|e| VerifyTagError::LoadObject(e.to_string()))?;
    let tag = load_object::<git_internalTag>(&object_id)
        .map_err(|_| VerifyTagError::NotAnnotated(name.to_string()))?;

    let Some(pos) = tag.message.find(TAG_SIGNATURE_MARKER) else {
        return Err(VerifyTagError::NotSigned(name.to_string()));
    };
    let armored = &tag.message[pos..];
    let original_message = tag.message[..pos]
        .strip_suffix('\n')
        .unwrap_or(&tag.message[..pos]);

    // Reconstruct the exact bytes that were signed (the tag object content minus
    // the appended signature block), matching `create`'s serialization.
    let unsigned_content = format!(
        "object {}\ntype {}\ntag {}\ntagger {}\n\n{}",
        tag.object_hash, tag.object_type, tag.tag_name, tag.tagger, original_message
    );
    let sig_hex = crate::internal::vault::armored_to_signature_hex(armored)
        .map_err(|e| VerifyTagError::Vault(e.to_string()))?;
    let unseal_key = crate::internal::vault::load_unseal_key()
        .await
        .ok_or(VerifyTagError::NoVaultKey)?;
    let root_dir = util::storage_path();
    crate::internal::vault::pgp_verify(
        &root_dir,
        &unseal_key,
        unsigned_content.as_bytes(),
        &sig_hex,
    )
    .await
    .map_err(|e| VerifyTagError::Vault(e.to_string()))
}

/// Typed failures from [`list`]. Lets callers distinguish a transient SQLite
/// read failure (mapped to `IoReadFailed`) from a genuinely corrupt tag row
/// (`RepoCorrupt`) without parsing the underlying error string.
#[derive(Debug, thiserror::Error)]
pub enum ListTagError {
    #[error("failed to query tag references: {0}")]
    Query(#[from] DbErr),

    #[error("tag '{name}' is missing the commit field")]
    MissingCommit { name: String },

    #[error("tag '{name}' has invalid object hash: {detail}")]
    InvalidObjectHash { name: String, detail: String },

    #[error("tag reference row is missing the name field")]
    MissingName,

    #[error("failed to load object for tag '{name}': {detail}")]
    LoadObject { name: String, detail: String },
}

/// Lists all tags available in the repository.
pub async fn list() -> Result<Vec<Tag>, ListTagError> {
    let db_conn = get_db_conn_instance().await;
    let models = reference::Entity::find()
        .filter(reference::Column::Kind.eq(reference::ConfigKind::Tag))
        .all(&db_conn)
        .await?;

    let mut tags = Vec::new();
    for m in models {
        let display_name = m.name.as_deref().unwrap_or(UNKNOWN_TAG).to_string();
        let commit_str = m
            .commit
            .as_ref()
            .ok_or_else(|| ListTagError::MissingCommit {
                name: display_name.clone(),
            })?;
        let object_id =
            crate::internal::object_format::parse_repo_oid(commit_str).map_err(|e| {
                ListTagError::InvalidObjectHash {
                    name: display_name.clone(),
                    detail: e.to_string(),
                }
            })?;
        let object = load_object_trait(&object_id)
            .await
            .map_err(|e| ListTagError::LoadObject {
                name: display_name.clone(),
                detail: e.to_string(),
            })?;
        let stored_name = m.name.as_ref().ok_or(ListTagError::MissingName)?;
        let tag_name = stored_name
            .strip_prefix(TAG_REF_PREFIX)
            .unwrap_or(stored_name.as_str())
            .to_string();
        tags.push(Tag {
            name: tag_name,
            object,
        });
    }
    Ok(tags)
}

/// Deletes a tag reference from the repository.
pub async fn delete(name: &str) -> Result<(), anyhow::Error> {
    let db_conn = get_db_conn_instance().await;
    let full_ref_name = format!("{}{}", TAG_REF_PREFIX, name);

    let result = reference::Entity::delete_many()
        .filter(reference::Column::Name.eq(full_ref_name))
        .filter(reference::Column::Kind.eq(reference::ConfigKind::Tag))
        .exec(&db_conn)
        .await?;

    if result.rows_affected == 0 {
        Err(anyhow::anyhow!("tag '{}' not found", name))
    } else {
        Ok(())
    }
}

/// Finds the raw tag reference row without dereferencing the target object.
pub async fn find_tag_ref(name: &str) -> Result<Option<TagReference>, DbErr> {
    let db_conn = get_db_conn_instance().await;
    let full_ref_name = format!("{}{}", TAG_REF_PREFIX, name);

    let model = reference::Entity::find()
        .filter(reference::Column::Name.eq(full_ref_name))
        .filter(reference::Column::Kind.eq(reference::ConfigKind::Tag))
        .one(&db_conn)
        .await?;

    Ok(model.map(|m| TagReference { target: m.commit }))
}

/// Finds a tag by name and returns the tag object and the final commit
pub async fn find_tag_and_commit(name: &str) -> Result<Option<(TagObject, Commit)>, GitError> {
    let db_conn = get_db_conn_instance().await;
    let full_ref_name = format!("{}{}", TAG_REF_PREFIX, name);

    let model = reference::Entity::find()
        .filter(reference::Column::Name.eq(full_ref_name))
        .filter(reference::Column::Kind.eq(reference::ConfigKind::Tag))
        .one(&db_conn)
        .await
        .map_err(|e| GitError::CustomError(e.to_string()))?;

    if let Some(m) = model {
        let commit_str = m
            .commit
            .as_ref()
            .ok_or_else(|| GitError::CustomError("Tag is missing commit field".to_string()))?;
        let target_id = crate::internal::object_format::parse_repo_oid(commit_str)
            .map_err(|_| GitError::InvalidHashValue(commit_str.to_string()))?;
        let ref_object = load_object_trait(&target_id).await?;

        // If the ref points to a tag object, dereference it to get the commit
        let commit_id = if let TagObject::Tag(tag_object) = &ref_object {
            tag_object.object_hash
        } else {
            target_id
        };

        let commit: Commit = load_object(&commit_id)?;
        Ok(Some((ref_object, commit)))
    } else {
        Ok(None)
    }
}

/// Load a Git object and return it as a `TagObject`.
pub async fn load_object_trait(hash: &ObjectHash) -> Result<TagObject, GitError> {
    // Use ClientStorage to get the object type first
    let storage = ClientStorage::init(path::objects());
    let obj_type = storage
        .get_object_type(hash)
        .map_err(|e| GitError::ObjectNotFound(format!("{}: {}", hash, e)))?;
    match obj_type {
        ObjectType::Commit => {
            let commit = load_object::<Commit>(hash)
                .map_err(|e| GitError::ObjectNotFound(format!("{}: {}", hash, e)))?;
            Ok(TagObject::Commit(commit))
        }
        ObjectType::Tag => {
            let tag = load_object::<git_internalTag>(hash)
                .map_err(|e| GitError::ObjectNotFound(format!("{}: {}", hash, e)))?;
            Ok(TagObject::Tag(tag))
        }
        ObjectType::Tree => {
            let tree = load_object::<Tree>(hash)
                .map_err(|e| GitError::ObjectNotFound(format!("{}: {}", hash, e)))?;
            Ok(TagObject::Tree(tree))
        }
        ObjectType::Blob => {
            let blob = load_object::<Blob>(hash)
                .map_err(|e| GitError::ObjectNotFound(format!("{}: {}", hash, e)))?;
            Ok(TagObject::Blob(blob))
        }
        _ => Err(GitError::ObjectNotFound(hash.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the `Display` format contract for [`CreateTagError`]. The
    /// strings are produced via `thiserror` `#[error(...)]` attributes
    /// and surface in the `tag` CLI command's human + `--json` output
    /// envelope. Future refactors that touch the `#[error(...)]`
    /// attributes will trip this test.
    #[test]
    fn create_tag_error_display_pins_each_variant() {
        // Static-message variants (the source-chained ones below cover
        // the `{0}` interpolation slot).
        assert_eq!(
            CreateTagError::HeadUnborn.to_string(),
            "Cannot create tag: HEAD does not point to a commit",
        );
        assert_eq!(
            CreateTagError::AlreadyExists("v1.0.0".to_string()).to_string(),
            "Tag 'v1.0.0' already exists",
        );

        // Source-chained variants: only assert the surface prefix; the
        // `{0}` portion forwards to the source error's Display, which
        // is owned by the upstream crate or by another pinned test
        // (BranchStoreError in v0.17.272). Pin every source-chained
        // variant so the test honours the "each variant" name — a
        // future patch that drops the trailing space or rewrites the
        // prefix on any variant trips this guard regardless of which
        // source error variant the caller happened to construct.
        let resolve = CreateTagError::ResolveHead(BranchStoreError::NotFound("main".to_string()));
        assert!(
            resolve
                .to_string()
                .starts_with("failed to resolve HEAD commit: "),
            "unexpected: {resolve}",
        );
        let check_existing = CreateTagError::CheckExisting(DbErr::Custom("ignored".to_string()));
        assert!(
            check_existing
                .to_string()
                .starts_with("failed to query existing tag refs: "),
            "unexpected: {check_existing}",
        );
        let serialize =
            CreateTagError::SerializeTag(GitError::ConversionError("ignored".to_string()));
        assert!(
            serialize
                .to_string()
                .starts_with("failed to serialize annotated tag object: "),
            "unexpected: {serialize}",
        );
        let store =
            CreateTagError::StoreObject(io::Error::new(io::ErrorKind::PermissionDenied, "ignored"));
        assert!(
            store
                .to_string()
                .starts_with("failed to store annotated tag object: "),
            "unexpected: {store}",
        );
        let persist = CreateTagError::PersistReference(DbErr::Custom("ignored".to_string()));
        assert!(
            persist
                .to_string()
                .starts_with("failed to persist tag reference: "),
            "unexpected: {persist}",
        );
    }
}
