//! Guard the optional live compatibility workflow shape.
//!
//! The live-cloud gate requires external cloud secrets, so it must remain
//! outside the base required-check workflow. This test locks the local
//! contract that `compat-live-cloud` is manual/scheduled, secret-gated, and
//! absent from `base.yml`; the former Code-era live AI job and its deleted
//! targets (plan-20260920) must not silently return.

use std::{fs, path::PathBuf};

#[test]
fn live_compat_workflow_is_optional_and_secret_gated() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let live = fs::read_to_string(repo.join(".github/workflows/live-compat.yml"))
        .expect("read .github/workflows/live-compat.yml");
    let base = fs::read_to_string(repo.join(".github/workflows/base.yml"))
        .expect("read .github/workflows/base.yml");

    for required in [
        "workflow_dispatch: {}",
        "schedule:",
        "name: compat-live-cloud",
        "LIBRA_D1_ACCOUNT_ID",
        "LIBRA_STORAGE_SECRET_KEY",
        "skip=true",
        "--features test-live-cloud",
        "--test cloud_storage_backup_test",
        "--test agent_cloud_tombstone_test",
    ] {
        assert!(
            live.contains(required),
            "live compatibility workflow is missing expected marker: {required}"
        );
    }

    for forbidden in ["pull_request:", "push:"] {
        assert!(
            !live.contains(forbidden),
            "live compatibility workflow must not run as a required PR/push gate: {forbidden}"
        );
    }

    // plan-20260920: the Code-era live AI job was removed together with the
    // `ai_agent_test` / `ai_chat_agent_test` targets and the `test-live-ai`
    // feature's only consumers.
    for forbidden in [
        "compat-live-ai",
        "DEEPSEEK_API_KEY",
        "--features test-live-ai",
        "ai_agent_test",
        "ai_chat_agent_test",
    ] {
        assert!(
            !live.contains(forbidden),
            "live compatibility workflow must not keep the removed Code-era live AI surface: {forbidden}"
        );
    }

    for live_job in ["compat-live-ai", "compat-live-cloud"] {
        assert!(
            !base.contains(live_job),
            "base.yml required-check workflow must not include optional live job {live_job}"
        );
    }
}
