//! Permission ruleset machinery (OC-Phase 2 P2.3).
//!
//! This module is the in-memory half of the [Permission Ruleset & Approval
//! Feedback Protocol] from `docs/development/commands/_general.md`. It defines the
//! data shapes and the two pure algorithms (`evaluate`, `disabled`) that
//! the tool registry uses to decide which tools a given agent + ruleset
//! pair can see.
//!
//! What this module owns:
//! - [`PermissionAction`], [`PermissionRule`], [`PermissionRuleset`] types
//!   with `serde` round-trip support.
//! - [`evaluate()`] — `findLast` wildcard match across one or more rulesets,
//!   matching opencode's `permission/evaluate.ts` semantics so a future
//!   joint ruleset (session ∪ project) is just a list concatenation.
//! - [`disabled`] — pattern=`*` deny pre-filter that produces the set of
//!   tool names the model should never see in its schema.
//! - [`ApprovedRuleset`] / [`ApprovedRulesetStore`] (OC-Phase 2 P2.5 / W4-07):
//!   persistent projection over the `approved_permission` SQLite table
//!   that materialises an in-memory [`PermissionRuleset`] of every
//!   `Always`-reply approval for the active project.
//! - [`ApprovalRuntimeCache`] (W4-13): repo_id-keyed ApprovalStore scope and
//!   lease-takeover invalidation of session memos.
//!
//! What this module does **not** own:
//! - The prompt + Reply state machine (`Once` / `Always` / `Reject`)
//!   that decides when to call into [`ApprovedRulesetStore`]; that lives
//!   in [`crate::internal::ai::sandbox::ApprovalCachePolicy`], which
//!   reads the loaded [`ApprovedRuleset`] through its
//!   `approved_ruleset` field.
//! - `ToolRegistry::available_for` is on the registry itself; this module
//!   only exports the algorithm it consumes.
//!
//! [Permission Ruleset & Approval Feedback Protocol]: https://github.com/genedna/libra/blob/main/docs/development/commands/_general.md

pub mod approved;
pub mod evaluate;
pub mod inheritance;
pub mod rule;
pub mod runtime_cache;

pub use approved::{
    ApprovalProvenance, ApprovedPermissionRecord, ApprovedRuleset, ApprovedRulesetStore,
    ApprovedStoreError,
};
pub use evaluate::{EDIT_TOOLS, disabled, edit_tools, evaluate, wildcard_match};
pub use inheritance::{agent_permission_spec_to_ruleset, assert_no_escalation, child_ruleset};
pub use rule::{PermissionAction, PermissionRule, PermissionRuleset};
pub use runtime_cache::{
    ApprovalRuntimeCache, ApprovalRuntimeCacheError, approval_cache_scope_key,
    resolve_approval_runtime_cache, revoke_session_approval_memos, unbound_approval_cache_scope,
};
