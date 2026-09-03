use sea_orm::{Iterable, PrimaryKeyTrait};

use super::{
    memory_episode_path, memory_episode_search_doc, memory_head, memory_link_index,
    memory_note_index, memory_path_summary, memory_projection_state, memory_revision_index,
};

fn key_names<P>() -> Vec<String>
where
    P: Iterable + std::fmt::Debug,
{
    P::iter().map(|key| format!("{key:?}")).collect()
}

#[test]
fn memory_entities_expose_expected_primary_keys() {
    assert_eq!(
        key_names::<memory_head::PrimaryKey>(),
        ["ScopeKey", "Namespace", "Path", "NoteId"]
    );
    assert_eq!(
        key_names::<memory_path_summary::PrimaryKey>(),
        ["ScopeKey", "Namespace", "Path"]
    );
    assert_eq!(key_names::<memory_note_index::PrimaryKey>(), ["NoteId"]);
    assert_eq!(
        key_names::<memory_revision_index::PrimaryKey>(),
        ["RevisionOid"]
    );
    assert_eq!(
        key_names::<memory_link_index::PrimaryKey>(),
        ["SourceRevisionOid", "TargetNoteId", "LinkKind"]
    );
    assert_eq!(
        key_names::<memory_projection_state::PrimaryKey>(),
        ["ScopeKey"]
    );
    assert_eq!(
        key_names::<memory_episode_path::PrimaryKey>(),
        ["NoteId", "RevisionOid", "CodePath"]
    );
    assert_eq!(
        key_names::<memory_episode_search_doc::PrimaryKey>(),
        ["Rowid"]
    );

    assert!(!memory_head::PrimaryKey::auto_increment());
    assert!(!memory_path_summary::PrimaryKey::auto_increment());
    assert!(!memory_note_index::PrimaryKey::auto_increment());
    assert!(!memory_revision_index::PrimaryKey::auto_increment());
    assert!(!memory_link_index::PrimaryKey::auto_increment());
    assert!(!memory_projection_state::PrimaryKey::auto_increment());
    assert!(!memory_episode_path::PrimaryKey::auto_increment());
    assert!(memory_episode_search_doc::PrimaryKey::auto_increment());
}
