//! `PermissionAction`, `PermissionRule`, and `PermissionRuleset` types.
//!
//! These mirror the opencode `permission/index.ts` shapes verbatim so a
//! future joint Libra ⇄ opencode ruleset is a list concatenation, not a
//! semantic translation.

use serde::{Deserialize, Serialize};

/// Three-state action a [`PermissionRule`] can prescribe.
///
/// `Allow` and `Deny` are obvious; `Ask` triggers an interactive prompt
/// (the runtime's `PermissionService::ask()` path). The string form is
/// snake_case so it round-trips cleanly through TOML / JSON and matches
/// opencode's wire format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionAction {
    Allow,
    Deny,
    Ask,
}

/// A single permission rule.
///
/// Field meanings:
/// - `permission`: the **permission key** the rule applies to (e.g. `edit`,
///   `bash`, `task`, `external_directory`, or a tool name). The string `"*"`
///   matches any permission key.
/// - `pattern`: a glob-style pattern restricting which inputs this rule
///   covers (e.g. `src/**` for a path, `git status` for a shell command,
///   `*` for "any input").
/// - `action`: the action to take when both `permission` and `pattern`
///   wildcard-match.
///
/// Order in a [`PermissionRuleset`] is significant: [`super::evaluate()`]
/// returns the **last** matching rule (`findLast` semantics). Earlier
/// rules act as fallbacks that later rules override.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionRule {
    pub permission: String,
    pub pattern: String,
    pub action: PermissionAction,
}

impl PermissionRule {
    /// Convenience constructor used by tests and config parsers.
    pub fn new(
        permission: impl Into<String>,
        pattern: impl Into<String>,
        action: PermissionAction,
    ) -> Self {
        Self {
            permission: permission.into(),
            pattern: pattern.into(),
            action,
        }
    }
}

/// An ordered list of [`PermissionRule`]s.
///
/// Ordering is intentional: rules listed later override earlier ones,
/// matching opencode's `findLast` semantics. Concatenating two rulesets
/// (`a.iter().chain(b.iter())`) is the documented way to merge a session
/// ruleset over a project ruleset.
pub type PermissionRuleset = Vec<PermissionRule>;

#[cfg(test)]
mod tests {
    use super::*;

    /// Scenario: `PermissionAction` round-trips through JSON as the
    /// documented snake_case strings. The wire format is the public
    /// contract for hand-edited TOML / JSON config.
    #[test]
    fn permission_action_serde_strings() {
        assert_eq!(
            serde_json::to_string(&PermissionAction::Allow).unwrap(),
            "\"allow\""
        );
        assert_eq!(
            serde_json::to_string(&PermissionAction::Deny).unwrap(),
            "\"deny\""
        );
        assert_eq!(
            serde_json::to_string(&PermissionAction::Ask).unwrap(),
            "\"ask\""
        );

        for raw in ["\"allow\"", "\"deny\"", "\"ask\""] {
            let _: PermissionAction =
                serde_json::from_str(raw).expect("documented variant must parse");
        }
    }

    /// Scenario: an unknown action string is rejected. A typo like
    /// `"deyn"` must NOT silently coerce to a default; a permission
    /// decision is too security-sensitive for that.
    #[test]
    fn permission_action_rejects_unknown_variant() {
        let err = serde_json::from_str::<PermissionAction>("\"deyn\"").unwrap_err();
        assert!(err.to_string().contains("deyn"));
    }

    /// Scenario: `PermissionRule` JSON round-trips with all three required
    /// fields and rejects unknown fields so a typo (`permision`, missing
    /// `s`) surfaces at parse time instead of silently being ignored.
    #[test]
    fn permission_rule_round_trip_and_rejects_unknown_fields() {
        let rule = PermissionRule::new("edit", "src/**", PermissionAction::Allow);
        let json = serde_json::to_string(&rule).unwrap();
        let back: PermissionRule = serde_json::from_str(&json).unwrap();
        assert_eq!(back, rule);

        let bad = r#"{"permision":"edit","pattern":"*","action":"allow"}"#;
        let err = serde_json::from_str::<PermissionRule>(bad).unwrap_err();
        assert!(err.to_string().contains("permision"));
    }
}
