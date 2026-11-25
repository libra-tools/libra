//! Unit tests for SeaORM reference models and serialization assumptions.
//!
//! Scenario focus: stable model field names, JSON compatibility, and edge cases that
//! generated database entities do not express on their own.

use super::reference::ConfigKind;

#[test]
fn test_config_kind_serialization() {
    let kind = ConfigKind::Branch;
    let serialized = serde_json::to_string(&kind).unwrap();
    // It should be serialized as "Branch" string, not integer
    assert_eq!(serialized, "\"Branch\"");

    let deserialized: ConfigKind = serde_json::from_str(&serialized).unwrap();
    assert_eq!(deserialized, ConfigKind::Branch);

    let kind = ConfigKind::Head;
    let serialized = serde_json::to_string(&kind).unwrap();
    assert_eq!(serialized, "\"Head\"");
}
