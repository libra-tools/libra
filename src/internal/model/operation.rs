//! SeaORM entity definition for command-level operation audit records.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "operation")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub op_id: String,
    pub repo_id: String,
    pub view_id: String,
    pub command_name: String,
    pub description: String,
    pub actor: String,
    pub args_digest: Option<String>,
    pub start_ts: i64,
    pub end_ts: Option<i64>,
    pub status: String,
    /// Worktree scope the operation ran in (Part C W1 §C.9): main = `""`,
    /// linked = its stable instance id. Scopes the duplicate-submission
    /// window per-worktree.
    pub worktree_id: String,
    /// How `worktree_id` came to hold its value (Part C W0 §C.11):
    /// `"declared"` — the process that ran the operation recorded its own
    /// scope; `"unknown"` — the row predates the scope column in a
    /// repository with linked-worktree evidence, so its `""` means "not
    /// recorded", not "main". `op restore` refuses `unknown` rows rather
    /// than guess (ADR-0714-08).
    pub scope_provenance: String,
    /// Whether `op restore` may replay this operation (Part C W1 §C.9).
    ///
    /// The snapshot covers HEAD and refs only — it cannot restore an index, a
    /// working tree, or sequencer state. Operations that changed one of those
    /// (every sequencer control action) record `0` here, and `op restore`
    /// refuses them before doing anything. A stored property, not a check
    /// against `command_name`: the name is a mutable label, and a renamed
    /// command must not silently become restorable.
    pub restorable: i32,
    /// Non-NULL while this row is a sequencer CONTROL action's claim on its
    /// worktree's single control slot (Part C W1 §C.9). The partial unique
    /// index on `(repo_id, worktree_id) WHERE status = 'running' AND
    /// control_slot IS NOT NULL` is what makes one control per worktree an
    /// invariant rather than a check.
    pub control_slot: Option<String>,
    /// `<host>/<pid>` of the process holding a `running` claim, so a claim
    /// left by a killed process can be PROVEN dead rather than guessed from
    /// age — a control action may legitimately sit for a long time in an
    /// editor or a hook.
    pub claim_owner: Option<String>,
    /// What KIND of scope this operation ran in (Part C W1 §C.9):
    /// `main` / `linked` / `repository` / `unknown`.
    ///
    /// `worktree_id` alone cannot express it: a repository-scope operation
    /// recorded from main carries the same empty id as a main-scope one, and
    /// `op restore` must refuse the former while allowing the latter.
    pub scope_kind: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
