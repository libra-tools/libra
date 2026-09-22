use std::collections::{HashMap, HashSet};

use git_internal::internal::object::commit::Commit;

pub(super) type RevListChildren = HashMap<String, Vec<String>>;

pub(super) fn build_rev_list_children(commits: &[Commit]) -> RevListChildren {
    let visible_ids = commits
        .iter()
        .map(|commit| commit.id.to_string())
        .collect::<HashSet<_>>();
    let mut children = RevListChildren::new();

    for commit in commits {
        let child_id = commit.id.to_string();
        for parent_id in &commit.parent_commit_ids {
            // SHALLOW-DISPLAY: children among already-selected commits
            let parent_id = parent_id.to_string();
            if visible_ids.contains(&parent_id) {
                children
                    .entry(parent_id)
                    .or_default()
                    .push(child_id.clone());
            }
        }
    }

    children
}

#[cfg(test)]
mod tests {
    use git_internal::{
        hash::{ObjectHash, get_hash_kind},
        internal::object::signature::{Signature, SignatureType},
    };

    use super::*;

    fn test_hash(byte: u8) -> ObjectHash {
        ObjectHash::from_bytes(&vec![byte; get_hash_kind().size()])
            .expect("test hash bytes should match active hash kind")
    }

    fn test_signature() -> Signature {
        Signature {
            signature_type: SignatureType::Committer,
            name: "tester".to_string(),
            email: "tester@example.com".to_string(),
            timestamp: 1,
            timezone: "+0000".to_string(),
        }
    }

    fn test_commit(id: ObjectHash, parents: Vec<ObjectHash>) -> Commit {
        Commit {
            id,
            tree_id: id,
            parent_commit_ids: parents, // SHALLOW-DISPLAY: test fixture
            author: test_signature(),
            committer: test_signature(),
            message: "test".to_string(),
        }
    }

    #[test]
    fn test_build_rev_list_children_preserves_traversal_child_order() {
        let root = test_hash(0x01);
        let main = test_hash(0x02);
        let side = test_hash(0x03);
        let merge = test_hash(0x04);
        let outside = test_hash(0x05);
        let commits = vec![
            test_commit(merge, vec![main, side]),
            test_commit(side, vec![root]),
            test_commit(main, vec![root]),
            test_commit(root, vec![outside]),
        ];

        let children = build_rev_list_children(&commits);

        assert_eq!(
            children.get(&root.to_string()),
            Some(&vec![side.to_string(), main.to_string()])
        );
        assert_eq!(
            children.get(&main.to_string()),
            Some(&vec![merge.to_string()])
        );
        assert_eq!(
            children.get(&side.to_string()),
            Some(&vec![merge.to_string()])
        );
        assert!(!children.contains_key(&outside.to_string()));
    }
}
