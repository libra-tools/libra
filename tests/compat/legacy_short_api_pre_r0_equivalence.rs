//! `compat_legacy_short_api_pre_r0_equivalence` — R0-6 (plan-20260714
//! §B.6.0.1): the LEGACY tuple API `generate_short_format_status` keeps its
//! pre-R0 shape. Renames are decomposed into endpoint states (staged
//! old=`D `/new=`A `; unstaged old merges an unstaged `D`, new becomes
//! `??`), so pre-R0 consumers never see an `R` column and chains produce no
//! duplicate rows. Rename-aware consumers use `generate_short_status_entries`.

use std::path::PathBuf;

use libra::command::status::{Changes, generate_short_format_status};

fn p(path: &str) -> PathBuf {
    PathBuf::from(path)
}

#[test]
fn staged_rename_decomposes_to_delete_plus_add() {
    let staged = Changes {
        renamed: vec![(p("old.txt"), p("new.txt"))],
        ..Default::default()
    };
    let unstaged = Changes::default();
    assert_eq!(
        generate_short_format_status(&staged, &unstaged),
        vec![(p("new.txt"), 'A', ' '), (p("old.txt"), 'D', ' ')],
    );
}

#[test]
fn unstaged_rename_decomposes_to_unstaged_delete_plus_untracked() {
    let staged = Changes::default();
    let unstaged = Changes {
        renamed: vec![(p("old.txt"), p("new.txt"))],
        ..Default::default()
    };
    assert_eq!(
        generate_short_format_status(&staged, &unstaged),
        vec![(p("new.txt"), '?', '?'), (p("old.txt"), ' ', 'D')],
    );
}

#[test]
fn rename_destination_worktree_states_merge_into_endpoints() {
    // RM: staged rename + worktree modification of the destination.
    let staged = Changes {
        renamed: vec![(p("old.txt"), p("new.txt"))],
        ..Default::default()
    };
    let unstaged = Changes {
        modified: vec![p("new.txt")],
        ..Default::default()
    };
    assert_eq!(
        generate_short_format_status(&staged, &unstaged),
        vec![(p("new.txt"), 'A', 'M'), (p("old.txt"), 'D', ' ')],
        "RM decomposes to AM + D"
    );

    // RD: staged rename + worktree deletion of the destination.
    let unstaged = Changes {
        deleted: vec![p("new.txt")],
        ..Default::default()
    };
    assert_eq!(
        generate_short_format_status(&staged, &unstaged),
        vec![(p("new.txt"), 'A', 'D'), (p("old.txt"), 'D', ' ')],
        "RD decomposes to AD + D"
    );
}

#[test]
fn chain_rename_yields_d_ad_untracked_without_duplicates() {
    // a→b staged, b→c unstaged: a = `D `, b = `AD`, c = `??`.
    let staged = Changes {
        renamed: vec![(p("a.txt"), p("b.txt"))],
        ..Default::default()
    };
    let unstaged = Changes {
        renamed: vec![(p("b.txt"), p("c.txt"))],
        ..Default::default()
    };
    let rows = generate_short_format_status(&staged, &unstaged);
    assert_eq!(
        rows,
        vec![
            (p("a.txt"), 'D', ' '),
            (p("b.txt"), 'A', 'D'),
            (p("c.txt"), '?', '?'),
        ],
        "chain decomposition with merged shared endpoint and no duplicates"
    );
}
