//! plan-20260714 §C.12 named regression `worktree_docs_match_v2_layout`
//! (W3): the user-facing documentation contract matches the v2 worktree
//! layout — isolated per-worktree HEAD/index over a `commondir` +
//! `worktree_id` gitdir — and never again claims that all worktrees share
//! HEAD or index.
//!
//! The W0 acceptance line is explicit: "文档 contract 不再出现『所有
//! worktree 共享 HEAD/index』"。 Docs drift silently: an old sentence
//! surviving a rewrite would tell users a checkout in one worktree moves
//! another's HEAD — the exact behavior the whole Part C effort removed. So
//! the claim is pinned in BOTH directions, on every page that describes the
//! layout.

/// Every page that documents the worktree layout, with its language noted in
/// the failure message.
const PAGES: &[(&str, &str)] = &[
    (
        "docs/commands/worktree.md",
        include_str!("../../docs/commands/worktree.md"),
    ),
    (
        "docs/commands/zh-CN/worktree.md",
        include_str!("../../docs/commands/zh-CN/worktree.md"),
    ),
    ("COMPATIBILITY.md", include_str!("../../COMPATIBILITY.md")),
];

#[test]
fn worktree_docs_match_v2_layout() {
    for (page, text) in PAGES {
        // POSITIVE: the v2 layout's load-bearing facts are described. The
        // per-worktree gitdir names its two marker files, and isolation is
        // stated in the page's own language.
        for needle in ["commondir", "worktree_id"] {
            assert!(
                text.contains(needle),
                "{page} no longer mentions `{needle}` — the v2 layout \
                 description was lost"
            );
        }
        let states_isolation = text.contains("isolated HEAD")
            || text.contains("ISOLATED HEAD")
            || text.contains("its own `HEAD")
            || text.contains("own `HEAD <sha>`")
            || text.contains("各自拥有 HEAD")
            || text.contains("隔离");
        assert!(
            states_isolation,
            "{page} no longer states per-worktree HEAD isolation"
        );

        // NEGATIVE: the pre-v2 shared-state claim must not resurface as a
        // PRESENT-TENSE contract. Historical notes are fine — every current
        // page mentions the legacy layout only as "legacy shared-`.libra`" /
        // 「旧的共享布局」 — so the pin targets the specific dead sentences.
        for forbidden in [
            "all worktrees share HEAD",
            "worktrees share the same HEAD",
            "share HEAD and index",
            "shares HEAD and index",
            "所有 worktree 共享 HEAD",
            "共享 HEAD/index",
            "共享 HEAD 和 index",
        ] {
            assert!(
                !text.contains(forbidden),
                "{page} claims the pre-v2 shared-state contract again \
                 (found {forbidden:?}) — linked worktrees have isolated \
                 HEAD/index since registry v2"
            );
        }
    }
}
