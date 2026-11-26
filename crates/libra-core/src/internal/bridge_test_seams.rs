//! Process-wide fault-injection seam slots for isolated `agent_bridge` tests
//! (gated on `feature = "test-bridge-hooks"` and `debug_assertions`).
//!
//! Sunk into `libra-core` in `WSM-02B` so `crates/libra-ai` (in `WSM-04`) and
//! root `libra` share the exact same seam slots without upward dependencies.

#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
mod inner {
    use std::{
        path::PathBuf,
        sync::{
            Mutex,
            atomic::{AtomicBool, Ordering},
        },
    };

    static BRIDGE_CRASH_AFTER_STAGE_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);
    static BRIDGE_CRASH_AFTER_COMMIT_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);
    static BRIDGE_CRASH_BEFORE_HEAD_MOVE: AtomicBool = AtomicBool::new(false);
    static BRIDGE_CRASH_AFTER_HEAD_MOVE: AtomicBool = AtomicBool::new(false);
    static BRIDGE_CRASH_AFTER_PREPARE: AtomicBool = AtomicBool::new(false);
    static BRIDGE_CRASH_BEFORE_RELEASE: AtomicBool = AtomicBool::new(false);
    static BRIDGE_FAIL_AFTER_STAGE: AtomicBool = AtomicBool::new(false);
    static BRIDGE_FAIL_AFTER_COMMIT: AtomicBool = AtomicBool::new(false);
    static BRIDGE_FORCE_LOCK_CONTENTION: AtomicBool = AtomicBool::new(false);
    static BRIDGE_FORCE_FENCE_MISMATCH: AtomicBool = AtomicBool::new(false);
    static BRIDGE_SLOW_WRITE_MS: Mutex<Option<u64>> = Mutex::new(None);

    #[doc(hidden)]
    pub fn set_crash_after_stage_dir(path: Option<PathBuf>) {
        *BRIDGE_CRASH_AFTER_STAGE_DIR
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = path;
    }

    #[doc(hidden)]
    pub fn take_crash_after_stage_dir() -> Option<PathBuf> {
        BRIDGE_CRASH_AFTER_STAGE_DIR
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    #[doc(hidden)]
    pub fn set_crash_after_commit_dir(path: Option<PathBuf>) {
        *BRIDGE_CRASH_AFTER_COMMIT_DIR
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = path;
    }

    #[doc(hidden)]
    pub fn take_crash_after_commit_dir() -> Option<PathBuf> {
        BRIDGE_CRASH_AFTER_COMMIT_DIR
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    #[doc(hidden)]
    pub fn set_crash_before_head_move(value: bool) {
        BRIDGE_CRASH_BEFORE_HEAD_MOVE.store(value, Ordering::SeqCst);
    }

    #[doc(hidden)]
    pub fn take_crash_before_head_move() -> bool {
        BRIDGE_CRASH_BEFORE_HEAD_MOVE.swap(false, Ordering::SeqCst)
    }

    #[doc(hidden)]
    pub fn set_crash_after_head_move(value: bool) {
        BRIDGE_CRASH_AFTER_HEAD_MOVE.store(value, Ordering::SeqCst);
    }

    #[doc(hidden)]
    pub fn take_crash_after_head_move() -> bool {
        BRIDGE_CRASH_AFTER_HEAD_MOVE.swap(false, Ordering::SeqCst)
    }

    #[doc(hidden)]
    pub fn set_crash_after_prepare(value: bool) {
        BRIDGE_CRASH_AFTER_PREPARE.store(value, Ordering::SeqCst);
    }

    #[doc(hidden)]
    pub fn take_crash_after_prepare() -> bool {
        BRIDGE_CRASH_AFTER_PREPARE.swap(false, Ordering::SeqCst)
    }

    #[doc(hidden)]
    pub fn set_crash_before_release(value: bool) {
        BRIDGE_CRASH_BEFORE_RELEASE.store(value, Ordering::SeqCst);
    }

    #[doc(hidden)]
    pub fn take_crash_before_release() -> bool {
        BRIDGE_CRASH_BEFORE_RELEASE.swap(false, Ordering::SeqCst)
    }

    #[doc(hidden)]
    pub fn set_fail_after_stage(value: bool) {
        BRIDGE_FAIL_AFTER_STAGE.store(value, Ordering::SeqCst);
    }

    #[doc(hidden)]
    pub fn take_fail_after_stage() -> bool {
        BRIDGE_FAIL_AFTER_STAGE.swap(false, Ordering::SeqCst)
    }

    #[doc(hidden)]
    pub fn set_fail_after_commit(value: bool) {
        BRIDGE_FAIL_AFTER_COMMIT.store(value, Ordering::SeqCst);
    }

    #[doc(hidden)]
    pub fn take_fail_after_commit() -> bool {
        BRIDGE_FAIL_AFTER_COMMIT.swap(false, Ordering::SeqCst)
    }

    #[doc(hidden)]
    pub fn set_force_lock_contention(value: bool) {
        BRIDGE_FORCE_LOCK_CONTENTION.store(value, Ordering::SeqCst);
    }

    #[doc(hidden)]
    pub fn take_force_lock_contention() -> bool {
        BRIDGE_FORCE_LOCK_CONTENTION.swap(false, Ordering::SeqCst)
    }

    #[doc(hidden)]
    pub fn set_force_fence_mismatch(value: bool) {
        BRIDGE_FORCE_FENCE_MISMATCH.store(value, Ordering::SeqCst);
    }

    #[doc(hidden)]
    pub fn take_force_fence_mismatch() -> bool {
        BRIDGE_FORCE_FENCE_MISMATCH.swap(false, Ordering::SeqCst)
    }

    #[doc(hidden)]
    pub fn set_slow_write_ms(ms: Option<u64>) {
        *BRIDGE_SLOW_WRITE_MS
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = ms;
    }

    #[doc(hidden)]
    pub fn slow_write_ms() -> Option<u64> {
        *BRIDGE_SLOW_WRITE_MS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
#[doc(hidden)]
pub use inner::*;
