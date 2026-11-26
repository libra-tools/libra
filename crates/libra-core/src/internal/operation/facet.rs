//! Uniform capture and restore contracts for mutable repository state.
//!
//! A facet owns one part of repository state.  Registering facets centrally
//! lets snapshot and restore code fail closed when a new mutable state owner
//! has not yet supplied capture/validation/restore semantics.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use git_internal::hash::ObjectHash;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Stable name of a state facet.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FacetName(String);

impl FacetName {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for FacetName {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for FacetName {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl fmt::Display for FacetName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// How a facet participates in recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestorePolicy {
    AutoRestore,
    Rebuild,
    NeverRestore,
}

/// A captured facet payload and its bounded metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FacetCapture {
    pub facet: FacetName,
    pub schema_version: u32,
    pub payload_oid: Option<ObjectHash>,
    pub meta: serde_json::Value,
}

/// Context supplied to a facet while capturing state.
#[derive(Debug, Default)]
pub struct FacetCaptureCtx {
    pub repo_id: Option<String>,
    pub workspace_id: Option<String>,
}

/// Context supplied to a facet while restoring state.
#[derive(Debug, Default)]
pub struct FacetRestoreCtx {
    pub repo_id: Option<String>,
    pub workspace_id: Option<String>,
}

/// Semantic facet delta used by future `op revert` implementations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FacetDiff {
    pub changes: serde_json::Value,
}

/// Errors returned by facet implementations and the registry boundary.
#[derive(Debug, Error)]
pub enum FacetError {
    #[error("facet '{0}' is not registered")]
    Unregistered(FacetName),
    #[error("facet name must not be empty")]
    EmptyName,
    #[error("facet '{facet}' returned a capture for '{returned}'")]
    NameMismatch {
        facet: FacetName,
        returned: FacetName,
    },
    #[error("facet '{facet}' schema version mismatch: expected {expected}, got {actual}")]
    SchemaVersionMismatch {
        facet: FacetName,
        expected: u32,
        actual: u32,
    },
    #[error("facet metadata contains a floating-point number")]
    NonCanonicalMetadata,
    #[error("facet capture is not fully registered")]
    IncompleteCapture,
    #[error("facet capture failed: {0}")]
    Capture(String),
    #[error("facet validation failed: {0}")]
    Validation(String),
    #[error("facet restore failed: {0}")]
    Restore(String),
    #[error("facet diff failed: {0}")]
    Diff(String),
}

/// Registry of every mutable state owner known to the operation layer.
#[derive(Default)]
pub struct FacetRegistry {
    facets: BTreeMap<FacetName, Box<dyn StateFacet>>,
}

impl fmt::Debug for FacetRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FacetRegistry")
            .field("facets", &self.facets.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl FacetRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, facet: Box<dyn StateFacet>) -> Result<(), FacetError> {
        let name = facet.name();
        if name.as_str().trim().is_empty() {
            return Err(FacetError::EmptyName);
        }
        if self.facets.contains_key(&name) {
            return Err(FacetError::Validation(format!(
                "facet '{name}' was registered more than once"
            )));
        }
        self.facets.insert(name, facet);
        Ok(())
    }

    pub fn get(&self, name: &FacetName) -> Option<&dyn StateFacet> {
        self.facets.get(name).map(Box::as_ref)
    }

    pub fn len(&self) -> usize {
        self.facets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.facets.is_empty()
    }

    /// Capture one registered facet and validate the returned envelope before
    /// it can be included in a fully-restorable snapshot.
    pub fn capture(
        &self,
        name: &FacetName,
        ctx: &FacetCaptureCtx,
    ) -> Result<FacetCapture, FacetError> {
        let facet = self
            .get(name)
            .ok_or_else(|| FacetError::Unregistered(name.clone()))?;
        let capture = facet.capture(ctx)?;
        self.validate_capture(&capture)?;
        Ok(capture)
    }

    pub fn validate_capture(&self, capture: &FacetCapture) -> Result<(), FacetError> {
        let facet = self
            .get(&capture.facet)
            .ok_or_else(|| FacetError::Unregistered(capture.facet.clone()))?;
        if capture.schema_version != facet.schema_version() {
            return Err(FacetError::SchemaVersionMismatch {
                facet: capture.facet.clone(),
                expected: facet.schema_version(),
                actual: capture.schema_version,
            });
        }
        validate_metadata(&capture.meta)?;
        facet.validate(capture)
    }

    /// Unknown or unregistered facets are never considered fully restorable.
    pub fn is_fully_restorable(&self, captures: &[FacetCapture]) -> bool {
        self.validate_captures(captures).is_ok()
            && captures.iter().all(|capture| {
                self.get(&capture.facet)
                    .is_some_and(|facet| facet.restore_policy() != RestorePolicy::NeverRestore)
            })
    }

    /// Validate a complete capture set before it can be used for restore.
    ///
    /// The registry is the source of truth for the mutable-state surface.
    /// Requiring an exact, duplicate-free set prevents an omitted facet from
    /// being mistaken for a clean snapshot. Individual captures are passed
    /// through the same schema, metadata, and facet-specific validation used
    /// by capture.
    pub fn validate_captures(&self, captures: &[FacetCapture]) -> Result<(), FacetError> {
        if captures.is_empty() || captures.len() != self.facets.len() {
            return Err(FacetError::IncompleteCapture);
        }
        let mut names = BTreeSet::new();
        for capture in captures {
            if !names.insert(capture.facet.clone()) {
                return Err(FacetError::IncompleteCapture);
            }
            self.validate_capture(capture)?;
        }
        if names.len() != self.facets.len() || self.facets.keys().any(|name| !names.contains(name))
        {
            return Err(FacetError::IncompleteCapture);
        }
        Ok(())
    }

    pub fn policies(&self, captures: &[FacetCapture]) -> BTreeMap<FacetName, RestorePolicy> {
        captures
            .iter()
            .filter_map(|capture| {
                self.get(&capture.facet)
                    .map(|facet| (capture.facet.clone(), facet.restore_policy()))
            })
            .collect()
    }
}

/// Trait implemented by each mutable state owner.
pub trait StateFacet: Send + Sync {
    fn name(&self) -> FacetName;
    fn schema_version(&self) -> u32;
    fn restore_policy(&self) -> RestorePolicy;
    fn capture(&self, ctx: &FacetCaptureCtx) -> Result<FacetCapture, FacetError>;
    fn validate(&self, capture: &FacetCapture) -> Result<(), FacetError>;
    fn restore(&self, capture: &FacetCapture, ctx: &mut FacetRestoreCtx) -> Result<(), FacetError>;
    fn diff(&self, from: &FacetCapture, to: &FacetCapture) -> Result<FacetDiff, FacetError>;
    fn roots(&self, capture: &FacetCapture) -> Vec<ObjectHash>;
}

fn validate_metadata(value: &serde_json::Value) -> Result<(), FacetError> {
    match value {
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::String(_) => {
            Ok(())
        }
        serde_json::Value::Number(number) => {
            if number.is_i64() || number.is_u64() {
                Ok(())
            } else {
                Err(FacetError::NonCanonicalMetadata)
            }
        }
        serde_json::Value::Array(values) => values.iter().try_for_each(validate_metadata),
        serde_json::Value::Object(values) => values.values().try_for_each(validate_metadata),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestFacet {
        policy: RestorePolicy,
    }

    impl StateFacet for TestFacet {
        fn name(&self) -> FacetName {
            FacetName::from("test")
        }

        fn schema_version(&self) -> u32 {
            1
        }

        fn restore_policy(&self) -> RestorePolicy {
            self.policy
        }

        fn capture(&self, _ctx: &FacetCaptureCtx) -> Result<FacetCapture, FacetError> {
            Ok(FacetCapture {
                facet: self.name(),
                schema_version: 1,
                payload_oid: None,
                meta: serde_json::json!({"count": 1}),
            })
        }

        fn validate(&self, _capture: &FacetCapture) -> Result<(), FacetError> {
            Ok(())
        }

        fn restore(
            &self,
            _capture: &FacetCapture,
            _ctx: &mut FacetRestoreCtx,
        ) -> Result<(), FacetError> {
            Ok(())
        }

        fn diff(&self, _from: &FacetCapture, _to: &FacetCapture) -> Result<FacetDiff, FacetError> {
            Ok(FacetDiff {
                changes: serde_json::json!({}),
            })
        }

        fn roots(&self, _capture: &FacetCapture) -> Vec<ObjectHash> {
            Vec::new()
        }
    }

    #[test]
    fn registry_rejects_unregistered_capture() {
        let registry = FacetRegistry::new();
        let error = registry
            .capture(&FacetName::from("missing"), &FacetCaptureCtx::default())
            .expect_err("unregistered facets must fail closed");
        assert!(matches!(error, FacetError::Unregistered(_)));
    }

    #[test]
    fn never_restore_facet_is_not_fully_restorable() {
        let mut registry = FacetRegistry::new();
        registry
            .register(Box::new(TestFacet {
                policy: RestorePolicy::NeverRestore,
            }))
            .expect("register facet");
        let capture = registry
            .capture(&FacetName::from("test"), &FacetCaptureCtx::default())
            .expect("capture facet");
        assert!(!registry.is_fully_restorable(&[capture]));
    }

    #[test]
    fn floating_point_metadata_is_rejected() {
        let mut registry = FacetRegistry::new();
        registry
            .register(Box::new(TestFacet {
                policy: RestorePolicy::AutoRestore,
            }))
            .expect("register facet");
        let capture = FacetCapture {
            facet: FacetName::from("test"),
            schema_version: 1,
            payload_oid: None,
            meta: serde_json::json!({"ratio": 1.5}),
        };
        assert!(matches!(
            registry.validate_capture(&capture),
            Err(FacetError::NonCanonicalMetadata)
        ));
    }

    #[test]
    fn incomplete_capture_sets_fail_closed() {
        let mut registry = FacetRegistry::new();
        registry
            .register(Box::new(TestFacet {
                policy: RestorePolicy::AutoRestore,
            }))
            .expect("register facet");
        assert!(!registry.is_fully_restorable(&[]));
        let capture = registry
            .capture(&FacetName::from("test"), &FacetCaptureCtx::default())
            .expect("capture facet");
        assert!(registry.is_fully_restorable(&[capture]));
    }

    #[test]
    fn invalid_capture_cannot_be_fully_restorable() {
        let mut registry = FacetRegistry::new();
        registry
            .register(Box::new(TestFacet {
                policy: RestorePolicy::AutoRestore,
            }))
            .expect("register facet");
        let capture = FacetCapture {
            facet: FacetName::from("test"),
            schema_version: 1,
            payload_oid: None,
            meta: serde_json::json!({"ratio": 1.5}),
        };
        assert!(!registry.is_fully_restorable(&[capture]));
    }
}
