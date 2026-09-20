//! Shared integration-test helpers.
//!
//! Scenario focus: reusable mocks and fixtures that keep AI and command tests
//! deterministic while avoiding live provider dependencies.

#[allow(dead_code)]
pub mod mock_codex;
#[allow(dead_code)]
#[allow(dead_code)]
pub mod mock_completion_model;
#[allow(dead_code)]
pub mod mock_provider_server;
