//! OL-09 census guard for the shared mutation classifier.

use libra::internal::operation::{MutationClass, classify_command};

#[test]
fn representative_command_census_has_no_unclassified_mutation() {
    let cases = [
        ("status", MutationClass::ReadOnly),
        ("add", MutationClass::WorkspaceMutation),
        ("commit", MutationClass::RepoMutation),
        ("rebase", MutationClass::SequencerMutation),
        ("worktree", MutationClass::LibraStateMutation),
        ("shell", MutationClass::ExternalOrUnknown),
        ("internal-worker", MutationClass::InternalWorker),
    ];
    for (name, expected) in cases {
        assert_eq!(classify_command(name).unwrap(), expected);
    }
    assert!(classify_command("new-command-not-in-census").is_err());
}
