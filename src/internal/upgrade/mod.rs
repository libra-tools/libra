//! Auto-upgrade subsystem (plan-20260714 Part A).
//!
//! This module owns everything related to upgrading the installed `libra`
//! binary itself: the per-user upgrade configuration under
//! `{LIBRA_HOME}/upgrade/` (§A.3), and — in later slices — the official-install
//! marker, signed-manifest verification, anti-rollback state and the
//! crash-safe install transaction.
//!
//! Upgrade configuration is a **reserved namespace**: it lives in
//! `{LIBRA_HOME}/upgrade/settings.json`, never in the SQLite `config_kv`
//! store. The `libra config` command routes every spelling that can reach an
//! `upgrade.*` key through a dedicated router (see
//! `command::config::route_upgrade_namespace`) so the two stores can never
//! disagree about the upgrade mode.

pub mod home;
pub mod manifest;
pub mod platform;
pub mod settings;
pub mod trusted_keys;
