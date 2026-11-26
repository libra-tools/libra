//! Multi-process concurrency stress tests, dual-feature migration roundtrip,
//! and post-unlock `libra-migrate` binary bidirectional roundtrip integration
//! tests (`RB-12`, `RB-29c`).

#[cfg(feature = "storage-redb")]
mod redb_stress {
    use std::{
        env, fs,
        path::{Path, PathBuf},
        process::Command,
        time::Duration,
    };

    use libra::storage::{
        AgentSessionRecord, CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION, CaptureAgentKind,
        CaptureBatchPayload, CaptureQueueLane, CaptureScopeState, CaptureSessionState,
        CaptureStore as _, FRAME_FLAG_NONE, FRAME_FLAG_PRIORITY, GitStore as _,
        INITIAL_STORAGE_GENERATION, QueueAppendOptions, RedbCaptureStore, RedbGitStore,
        RefCasOutcome, RefKey, RefTarget, append_postcard_frame_with_options,
        canonical_capture_redb_rel_path, canonical_git_redb_rel_path, drain_capture_queue,
        init_capture_queue,
    };
    use tempfile::tempdir;

    const ROLE_ENV: &str = "LIBRA_RB12_STRESS_ROLE";
    const STORAGE_ENV: &str = "LIBRA_RB12_STORAGE_DIR";
    const GIT_DB_ENV: &str = "LIBRA_RB12_GIT_DB";
    const CAP_DB_ENV: &str = "LIBRA_RB12_CAP_DB";
    const WORKER_ID_ENV: &str = "LIBRA_RB12_WORKER_ID";

    const PRODUCER_COUNT: usize = 32;
    const EVENTS_PER_PRODUCER: usize = 8;
    const GIT_READER_COUNT: usize = 4;
    const GIT_WRITER_COUNT: usize = 2;
    const CAPTURE_READER_COUNT: usize = 4;
    const READER_ITERS: usize = 25;
    const WRITER_ITERS: usize = 15;

    fn make_session_record(
        worker_id: usize,
        seq: usize,
        lane: CaptureQueueLane,
    ) -> AgentSessionRecord {
        let lane_tag = match lane {
            CaptureQueueLane::Priority => "prio",
            CaptureQueueLane::Normal => "norm",
        };
        let session_id = format!("rb12-{lane_tag}-w{worker_id:02}-e{seq:02}");
        AgentSessionRecord {
            session_id: session_id.clone(),
            agent_kind: CaptureAgentKind::Codex,
            provider_session_id: format!("prov-{session_id}"),
            thread_id: None,
            state: CaptureSessionState::Active,
            working_dir: "/workspace/rb12-stress".to_string(),
            worktree_id: Some("main".to_string()),
            parent_commit: None,
            parent_session_id: None,
            metadata_json: format!(r#"{{"worker":{worker_id},"seq":{seq},"lane":"{lane_tag}"}}"#),
            redaction_report: "{}".to_string(),
            started_at: 1_760_000_000 + (worker_id as i64) * 100 + (seq as i64),
            last_event_at: 1_760_000_000 + (worker_id as i64) * 100 + (seq as i64),
            stopped_at: None,
            schema_version: 1,
            sync_revision: 1,
            repo_id: Some("rb12-stress-repo".to_string()),
            workspace_id: Some("ws-rb12-stress".to_string()),
            workspace_fence: Some(1),
            scope_state: CaptureScopeState::Scoped,
        }
    }

    fn format_stress_oid(worker_id: usize, iter: usize) -> String {
        format!("{worker_id:08x}{iter:08x}{:024x}", 0x5242_1200u64)
    }

    async fn run_child_role(role: &str) {
        let storage_dir = PathBuf::from(env::var_os(STORAGE_ENV).expect("storage dir env"));
        let git_db = PathBuf::from(env::var_os(GIT_DB_ENV).expect("git db env"));
        let cap_db = PathBuf::from(env::var_os(CAP_DB_ENV).expect("cap db env"));
        let worker_id: usize = env::var(WORKER_ID_ENV)
            .expect("worker id env")
            .parse()
            .expect("parse worker id");

        match role {
            "producer" => {
                let lane = if worker_id % 2 == 0 {
                    CaptureQueueLane::Priority
                } else {
                    CaptureQueueLane::Normal
                };
                let flags = if lane == CaptureQueueLane::Priority {
                    FRAME_FLAG_PRIORITY
                } else {
                    FRAME_FLAG_NONE
                };
                let opts = QueueAppendOptions {
                    lock_timeout: Duration::from_secs(15),
                    ..QueueAppendOptions::default()
                };
                for seq in 0..EVENTS_PER_PRODUCER {
                    let rec = make_session_record(worker_id, seq, lane);
                    let dedup_key = format!("rb12:session:{}", rec.session_id);
                    let payload = CaptureBatchPayload::UpsertSession(rec);
                    append_postcard_frame_with_options(
                        &storage_dir,
                        lane,
                        flags,
                        &dedup_key,
                        CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                        &payload,
                        opts,
                    )
                    .unwrap_or_else(|err| {
                        panic!(
                            "producer {worker_id} seq {seq} failed without lock tolerance: {err:?}"
                        )
                    });
                }
            }
            "git_reader" => {
                let store = RedbGitStore::open_read_only(&git_db)
                    .unwrap_or_else(|err| panic!("git_reader {worker_id} open_read_only: {err:?}"));
                let shared_key = RefKey::LocalBranch {
                    name: "stress-shared".to_string(),
                };
                for iter in 0..READER_ITERS {
                    let target = store.get_ref(&shared_key).await.unwrap_or_else(|err| {
                        panic!("git_reader {worker_id} iter {iter} get_ref failed: {err:?}")
                    });
                    assert!(
                        target.is_some(),
                        "git_reader {worker_id} iter {iter} saw missing stress-shared ref"
                    );
                    let refs = store.list_refs(None).await.unwrap_or_else(|err| {
                        panic!("git_reader {worker_id} iter {iter} list_refs failed: {err:?}")
                    });
                    assert!(!refs.is_empty());
                }
            }
            "git_writer" => {
                let store = RedbGitStore::open_read_write(&git_db).unwrap_or_else(|err| {
                    panic!("git_writer {worker_id} open_read_write: {err:?}")
                });
                let shared_key = RefKey::LocalBranch {
                    name: "stress-shared".to_string(),
                };
                let own_key = RefKey::LocalBranch {
                    name: format!("stress-writer-{worker_id}"),
                };
                for iter in 0..WRITER_ITERS {
                    // Own branch deterministic CAS advance.
                    let cur_own = store.get_ref(&own_key).await.unwrap_or_else(|err| {
                        panic!("git_writer {worker_id} get own ref: {err:?}")
                    });
                    let next_own = RefTarget::Direct(format_stress_oid(worker_id + 10, iter + 1));
                    let own_res = store
                        .update_ref_cas(&own_key, cur_own.as_ref(), Some(&next_own), &[])
                        .await
                        .unwrap_or_else(|err| {
                            panic!("git_writer {worker_id} iter {iter} own CAS error: {err:?}")
                        });
                    assert!(
                        matches!(own_res, RefCasOutcome::Applied { .. }),
                        "expected Applied, got {own_res:?}"
                    );

                    // Shared branch optimistic CAS loop (retries only on CasConflict, never lock error).
                    loop {
                        let cur_shared = store.get_ref(&shared_key).await.unwrap_or_else(|err| {
                            panic!("git_writer {worker_id} get shared ref: {err:?}")
                        });
                        let next_shared =
                            RefTarget::Direct(format_stress_oid(worker_id + 1, iter + 1));
                        match store
                            .update_ref_cas(
                                &shared_key,
                                cur_shared.as_ref(),
                                Some(&next_shared),
                                &[],
                            )
                            .await
                            .unwrap_or_else(|err| {
                                panic!(
                                    "git_writer {worker_id} iter {iter} shared CAS error: {err:?}"
                                )
                            }) {
                            RefCasOutcome::Applied { .. } => break,
                            RefCasOutcome::CasConflict { .. } => continue,
                        }
                    }
                }
            }
            "capture_reader" => {
                let store = RedbCaptureStore::open_read_only(&cap_db)
                    .await
                    .unwrap_or_else(|err| {
                        panic!("capture_reader {worker_id} open_read_only: {err:?}")
                    });
                for iter in 0..READER_ITERS {
                    let _sessions = store.list_sessions_by_started_desc().await.unwrap_or_else(|err| {
                        panic!("capture_reader {worker_id} iter {iter} list_sessions failed: {err:?}")
                    });
                }
            }
            other => panic!("unknown RB-12 stress worker role: {other}"),
        }
    }

    fn spawn_worker(
        exe: &Path,
        role: &str,
        worker_id: usize,
        storage_dir: &Path,
        git_db: &Path,
        cap_db: &Path,
    ) -> std::process::Child {
        Command::new(exe)
            .arg("--exact")
            .arg("redb_stress::multiprocess_32_producers_and_concurrent_mvcc_readers_writers")
            .arg("--nocapture")
            .env(ROLE_ENV, role)
            .env(WORKER_ID_ENV, worker_id.to_string())
            .env(STORAGE_ENV, storage_dir)
            .env(GIT_DB_ENV, git_db)
            .env(CAP_DB_ENV, cap_db)
            .spawn()
            .unwrap_or_else(|err| panic!("spawn {role} worker {worker_id}: {err}"))
    }

    /// `RB-12` AC1: 32 dual-lane producer processes + 4 looping `RedbGitStore`
    /// read-only processes + 2 looping `RedbGitStore` CAS write processes +
    /// 4 `RedbCaptureStore` read-only processes on the same repository all exit 0
    /// with zero lock errors and 100% of events ingested.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn multiprocess_32_producers_and_concurrent_mvcc_readers_writers() {
        if let Ok(role) = env::var(ROLE_ENV) {
            run_child_role(&role).await;
            return;
        }

        let tmp = tempdir().expect("create stress repo tempdir");
        let storage_dir = tmp.path().join(".libra");
        let redb_dir = storage_dir.join("redb");
        let private_dir = storage_dir.join("private");
        fs::create_dir_all(&redb_dir).expect("mkdir .libra/redb");
        fs::create_dir_all(&private_dir).expect("mkdir .libra/private");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&private_dir, fs::Permissions::from_mode(0o700))
                .expect("chmod 0700 private");
        }

        let git_db = storage_dir.join(canonical_git_redb_rel_path(INITIAL_STORAGE_GENERATION));
        let cap_db = storage_dir.join(canonical_capture_redb_rel_path(INITIAL_STORAGE_GENERATION));

        // Seed git.redb with initial `stress-shared` branch.
        {
            let git_store = RedbGitStore::create(&git_db).expect("create RedbGitStore");
            let seed_key = RefKey::LocalBranch {
                name: "stress-shared".to_string(),
            };
            let seed_target =
                RefTarget::Direct("0000000000000000000000000000000000000001".to_string());
            let outcome = git_store
                .update_ref_cas(&seed_key, None, Some(&seed_target), &[])
                .await
                .expect("seed stress-shared");
            assert!(matches!(outcome, RefCasOutcome::Applied { .. }));
        }

        // Seed capture.redb and initialize dual-lane capture queue.
        let cap_store = RedbCaptureStore::create(&cap_db)
            .await
            .expect("create RedbCaptureStore");
        init_capture_queue(&storage_dir).expect("init dual-lane capture queue");

        let exe = env::current_exe().expect("locate current stress test binary");
        let mut children: Vec<(&'static str, usize, std::process::Child)> = Vec::new();

        for id in 0..PRODUCER_COUNT {
            children.push((
                "producer",
                id,
                spawn_worker(&exe, "producer", id, &storage_dir, &git_db, &cap_db),
            ));
        }
        for id in 0..GIT_READER_COUNT {
            children.push((
                "git_reader",
                id,
                spawn_worker(&exe, "git_reader", id, &storage_dir, &git_db, &cap_db),
            ));
        }
        for id in 0..GIT_WRITER_COUNT {
            children.push((
                "git_writer",
                id,
                spawn_worker(&exe, "git_writer", id, &storage_dir, &git_db, &cap_db),
            ));
        }
        for id in 0..CAPTURE_READER_COUNT {
            children.push((
                "capture_reader",
                id,
                spawn_worker(&exe, "capture_reader", id, &storage_dir, &git_db, &cap_db),
            ));
        }

        // Continuously drain queue while producers and readers are active.
        for _ in 0..10 {
            let _ = drain_capture_queue(&storage_dir, &cap_store)
                .await
                .expect("concurrent drain_capture_queue");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        // Wait for all 42 child processes and assert zero failures.
        for (role, id, mut child) in children {
            let status = child
                .wait()
                .unwrap_or_else(|err| panic!("wait for {role} {id}: {err}"));
            assert!(
                status.success(),
                "stress child {role} #{id} exited with non-zero status: {status}"
            );
        }

        // Final drain guarantees 100% of producer frames are committed to capture.redb.
        loop {
            let report = drain_capture_queue(&storage_dir, &cap_store)
                .await
                .expect("final drain_capture_queue");
            if report.applied_frames == 0 && report.deduplicated_frames == 0 {
                break;
            }
        }

        // Verify 100% of 32 * 8 = 256 events are durably present in capture.redb.
        let sessions = cap_store
            .list_sessions_by_started_desc()
            .await
            .expect("list ingested sessions");
        let expected_total = PRODUCER_COUNT * EVENTS_PER_PRODUCER;
        assert_eq!(
            sessions.len(),
            expected_total,
            "expected all {expected_total} events from {PRODUCER_COUNT} dual-lane producers to be ingested"
        );
        for worker_id in 0..PRODUCER_COUNT {
            let lane = if worker_id % 2 == 0 {
                CaptureQueueLane::Priority
            } else {
                CaptureQueueLane::Normal
            };
            let lane_tag = match lane {
                CaptureQueueLane::Priority => "prio",
                CaptureQueueLane::Normal => "norm",
            };
            for seq in 0..EVENTS_PER_PRODUCER {
                let sid = format!("rb12-{lane_tag}-w{worker_id:02}-e{seq:02}");
                let found = cap_store
                    .get_session(&sid)
                    .await
                    .expect("get_session")
                    .unwrap_or_else(|| panic!("missing ingested session `{sid}`"));
                assert_eq!(found.session_id, sid);
            }
        }

        // Verify both git CAS writers completed every iteration cleanly.
        let git_ro = RedbGitStore::open_read_only(&git_db).expect("final open_read_only git.redb");
        for id in 0..GIT_WRITER_COUNT {
            let own_key = RefKey::LocalBranch {
                name: format!("stress-writer-{id}"),
            };
            let expected = RefTarget::Direct(format_stress_oid(id + 10, WRITER_ITERS));
            let actual = git_ro
                .get_ref(&own_key)
                .await
                .expect("read writer branch")
                .expect("writer branch exists");
            assert_eq!(actual, expected);
        }
    }
}

#[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]
mod dual_feature_stress {
    use std::{fs, path::Path, process::Command};

    use libra::{
        internal::db::schema::{DatabaseRole, create_database_for_role},
        storage::{
            AgentSessionRecord, CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION, CaptureAgentKind,
            CaptureBatchPayload, CaptureQueueLane, CaptureScopeState, CaptureSessionState,
            CaptureStore as _, ConfigStore as _, FRAME_FLAG_NONE, FRAME_FLAG_PRIORITY,
            GitStore as _, INITIAL_STORAGE_GENERATION, LIBRA_CONFIG_GLOBAL_DB_ENV,
            LIBRA_CONFIG_SYSTEM_DB_ENV, LIBRA_STORAGE_BACKEND_ENV, RedbCaptureStore, RedbGitStore,
            RedbRepairContext, RefCasOutcome, RefKey, RefTarget, SqliteCaptureStore,
            SqliteConfigStore, SqliteGitStore, StorageBackendKind, append_postcard_frame,
            drain_capture_queue, execute_forward_migration, execute_reverse_migration,
            init_capture_queue, read_storage_manifest, resolve_redb_generation_paths,
            storage_manifest_path,
        },
    };
    use sea_orm::ConnectionTrait;
    use tempfile::tempdir;

    /// `RB-12` AC4 & AC6: SQLite -> redb forward migration, dual-lane capture queue
    /// with concurrent MVCC read/CAS writes, then redb -> SQLite reverse migration with
    /// 100% data integrity verification across both `libra.db` and `capture.db`.
    pub(super) async fn dual_feature_migration_and_stress_roundtrip_impl() {
        let tmp = tempdir().expect("create dual-feature roundtrip tempdir");
        let dot_libra = tmp.path().join(".libra");
        fs::create_dir_all(&dot_libra).expect("mkdir .libra");

        // 1. Seed legacy SQLite repository database.
        let sqlite_git_path = dot_libra.join("libra.db");
        let db = create_database_for_role(
            sqlite_git_path.to_str().expect("utf8 db path"),
            DatabaseRole::Repository,
        )
        .await
        .expect("create sqlite repo");
        db.execute_unprepared(
            "INSERT INTO reference (name, kind, \"commit\", remote) \
             VALUES ('main', 'Branch', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', NULL);",
        )
        .await
        .expect("seed main branch");
        db.close().await.expect("close sqlite repo");

        // 2. Forward migrate SQLite -> redb.
        let fwd = execute_forward_migration(&dot_libra, INITIAL_STORAGE_GENERATION)
            .await
            .expect("forward migrate sqlite -> redb");
        assert_eq!(fwd.generation, INITIAL_STORAGE_GENERATION);
        let paths = resolve_redb_generation_paths(&dot_libra)
            .expect("resolve redb paths")
            .expect("redb paths present");

        // 3. Exercise live redb GitStore CAS writes + dual-lane queue appends + drain.
        let git_store = RedbGitStore::open_read_write(&paths.git_db_path).expect("open git.redb");
        let feature_key = RefKey::LocalBranch {
            name: "post-forward-stress".to_string(),
        };
        let feature_target =
            RefTarget::Direct("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string());
        assert!(matches!(
            git_store
                .update_ref_cas(&feature_key, None, Some(&feature_target), &[])
                .await
                .expect("CAS insert post-forward branch"),
            RefCasOutcome::Applied { .. }
        ));
        drop(git_store);

        init_capture_queue(&dot_libra).expect("init dual-lane capture queue");
        let cap_store = RedbCaptureStore::open_read_write(
            &paths.capture_db_path,
            RedbRepairContext::WriteCommand,
        )
        .await
        .expect("open capture.redb");
        for idx in 0..16usize {
            let (lane, flags) = if idx % 2 == 0 {
                (CaptureQueueLane::Priority, FRAME_FLAG_PRIORITY)
            } else {
                (CaptureQueueLane::Normal, FRAME_FLAG_NONE)
            };
            let sid = format!("rb12-roundtrip-session-{idx:02}");
            let rec = AgentSessionRecord {
                session_id: sid.clone(),
                agent_kind: CaptureAgentKind::ClaudeCode,
                provider_session_id: format!("prov-{sid}"),
                thread_id: None,
                state: CaptureSessionState::Stopped,
                working_dir: "/workspace/roundtrip".to_string(),
                worktree_id: Some("main".to_string()),
                parent_commit: None,
                parent_session_id: None,
                metadata_json: r#"{"roundtrip":true}"#.to_string(),
                redaction_report: "{}".to_string(),
                started_at: 1_760_100_000 + idx as i64,
                last_event_at: 1_760_100_100 + idx as i64,
                stopped_at: Some(1_760_100_200 + idx as i64),
                schema_version: 1,
                sync_revision: 1,
                repo_id: Some("rb12-roundtrip-repo".to_string()),
                workspace_id: Some("ws-rb12-roundtrip".to_string()),
                workspace_fence: Some(1),
                scope_state: CaptureScopeState::Scoped,
            };
            let dedup_key = format!("rb12:roundtrip:{sid}");
            let payload = CaptureBatchPayload::UpsertSession(rec);
            append_postcard_frame(
                &dot_libra,
                lane,
                flags,
                &dedup_key,
                CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION,
                &payload,
            )
            .expect("append roundtrip capture frame");
        }
        let drained = drain_capture_queue(&dot_libra, &cap_store)
            .await
            .expect("drain roundtrip capture queue");
        assert_eq!(drained.applied_frames, 16);
        drop(cap_store);

        // 4. Reverse migrate redb -> SQLite.
        let rev = execute_reverse_migration(&dot_libra)
            .await
            .expect("reverse migrate redb -> sqlite");
        let manifest = read_storage_manifest(&dot_libra)
            .expect("read manifest")
            .expect("manifest exists");
        assert_eq!(manifest.backend, StorageBackendKind::Sqlite);
        assert!(manifest.finalized);
        assert_eq!(rev.generation, manifest.generation);

        // 5. Verify 100% of git refs and capture sessions survived in restored SQLite stores.
        let restored_git = SqliteGitStore::open_read_only(&sqlite_git_path)
            .await
            .expect("open restored SqliteGitStore");
        assert_eq!(
            restored_git
                .get_ref(&RefKey::LocalBranch {
                    name: "main".to_string(),
                })
                .await
                .expect("read main ref"),
            Some(RefTarget::Direct(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()
            ))
        );
        assert_eq!(
            restored_git
                .get_ref(&feature_key)
                .await
                .expect("read post-forward ref"),
            Some(feature_target)
        );

        let restored_cap_path = dot_libra.join("private").join("capture.db");
        let restored_cap = SqliteCaptureStore::open_read_only(&restored_cap_path)
            .await
            .expect("open restored SqliteCaptureStore");
        let restored_sessions = restored_cap
            .list_sessions_by_started_desc()
            .await
            .expect("list restored capture sessions");
        assert_eq!(
            restored_sessions.len(),
            16,
            "all 16 queue-ingested sessions must survive redb -> sqlite reverse migration"
        );
    }

    fn make_roundtrip_session(session_id: &str, started_at: i64) -> AgentSessionRecord {
        AgentSessionRecord {
            session_id: session_id.to_string(),
            agent_kind: CaptureAgentKind::Codex,
            provider_session_id: format!("prov-{session_id}"),
            thread_id: None,
            state: CaptureSessionState::Stopped,
            working_dir: "/workspace/rb29c-roundtrip".to_string(),
            worktree_id: Some("main".to_string()),
            parent_commit: None,
            parent_session_id: None,
            metadata_json: format!(r#"{{"session":"{session_id}"}}"#),
            redaction_report: "{}".to_string(),
            started_at,
            last_event_at: started_at + 10,
            stopped_at: Some(started_at + 20),
            schema_version: 1,
            sync_revision: 1,
            repo_id: Some("rb29c-roundtrip-repo".to_string()),
            workspace_id: Some("ws-rb29c-roundtrip".to_string()),
            workspace_fence: Some(1),
            scope_state: CaptureScopeState::Scoped,
        }
    }

    /// `RB-29c` AC1: End-to-end post-unlock bidirectional roundtrip exercising the
    /// real compiled `libra-migrate` and `libra` binaries:
    /// 1. Seed a real SQLite repository (commits, refs, HEAD, config, capture session).
    /// 2. Run `libra-migrate --to redb --repo <repo>` -> exit 0, finalized redb manifest
    ///    and `.libra/redb/git.gen-*.redb`.
    /// 3. Perform new live writes on the Redb repository via unlocked `libra` CLI
    ///    (`LIBRA_STORAGE_BACKEND=redb`: config, commit, branch) + new capture session,
    ///    plus verify fresh `LIBRA_STORAGE_BACKEND=redb` `libra init`.
    /// 4. Run `libra-migrate --to sqlite --repo <repo>` -> exit 0, converts back to SQLite
    ///    preserving all pre-migration and post-Redb-cutover writes.
    /// 5. Verify 100% readback consistency through both the `libra` CLI and SQLite stores.
    pub(super) fn post_unlock_libra_migrate_binary_bidirectional_roundtrip_impl() {
        let tmp = tempdir().expect("create post-unlock bidirectional roundtrip tempdir");
        let repo_dir = tmp.path().join("repo");
        let fresh_redb_dir = tmp.path().join("fresh-redb-repo");
        let home_dir = tmp.path().join("home");
        let xdg_dir = home_dir.join(".config");
        let global_db = home_dir.join("global-config.db");
        let system_db = home_dir.join("system-config.db");
        fs::create_dir_all(&repo_dir).expect("mkdir repo");
        fs::create_dir_all(&fresh_redb_dir).expect("mkdir fresh-redb-repo");
        fs::create_dir_all(&xdg_dir).expect("mkdir isolated xdg config");

        let libra_bin = env!("CARGO_BIN_EXE_libra");
        let migrate_bin = env!("CARGO_BIN_EXE_libra-migrate");

        let run_libra = |cwd: &Path, backend_env: Option<&str>, args: &[&str]| -> String {
            let mut cmd = Command::new(libra_bin);
            cmd.current_dir(cwd)
                .args(args)
                .env("HOME", &home_dir)
                .env("USERPROFILE", &home_dir)
                .env("XDG_CONFIG_HOME", &xdg_dir)
                .env(LIBRA_CONFIG_GLOBAL_DB_ENV, &global_db)
                .env(LIBRA_CONFIG_SYSTEM_DB_ENV, &system_db);
            match backend_env {
                Some(v) => {
                    cmd.env(LIBRA_STORAGE_BACKEND_ENV, v);
                }
                None => {
                    cmd.env_remove(LIBRA_STORAGE_BACKEND_ENV);
                }
            }
            let output = cmd
                .output()
                .unwrap_or_else(|err| panic!("spawn libra {args:?}: {err}"));
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            assert!(
                output.status.success(),
                "libra {args:?} (backend_env={backend_env:?}) failed (status={:?}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
                output.status
            );
            stdout
        };

        let run_migrate = |args: &[&str]| -> String {
            let output = Command::new(migrate_bin)
                .args(args)
                .env("HOME", &home_dir)
                .env("USERPROFILE", &home_dir)
                .env("XDG_CONFIG_HOME", &xdg_dir)
                .env(LIBRA_CONFIG_GLOBAL_DB_ENV, &global_db)
                .env(LIBRA_CONFIG_SYSTEM_DB_ENV, &system_db)
                .env_remove(LIBRA_STORAGE_BACKEND_ENV)
                .output()
                .unwrap_or_else(|err| panic!("spawn libra-migrate {args:?}: {err}"));
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            assert!(
                output.status.success(),
                "libra-migrate {args:?} failed (status={:?}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
                output.status
            );
            stdout
        };

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current-thread tokio runtime for store verification");

        // -----------------------------------------------------------------
        // 1. Seed a real SQLite repository (default backend before RB-33).
        // -----------------------------------------------------------------
        run_libra(&repo_dir, None, &["init", "--vault", "false"]);
        let dot_libra = repo_dir.join(".libra");
        let sqlite_git_path = dot_libra.join("libra.db");
        assert!(
            sqlite_git_path.is_file(),
            "default libra init must create legacy SQLite libra.db"
        );
        assert!(
            !storage_manifest_path(&dot_libra).exists(),
            "fresh default SQLite init must not write storage-format.json"
        );

        run_libra(&repo_dir, None, &["config", "user.name", "RB29c Tester"]);
        run_libra(
            &repo_dir,
            None,
            &["config", "user.email", "rb29c@example.com"],
        );
        run_libra(
            &repo_dir,
            None,
            &["config", "roundtrip.origin", "sqlite-initial"],
        );

        fs::write(repo_dir.join("hello.txt"), "seeded on sqlite\n").expect("write hello.txt");
        run_libra(&repo_dir, None, &["add", "hello.txt"]);
        run_libra(
            &repo_dir,
            None,
            &["commit", "-m", "seed sqlite commit", "--no-verify"],
        );
        run_libra(&repo_dir, None, &["branch", "sqlite-branch"]);

        let initial_head_oid = run_libra(&repo_dir, None, &["rev-parse", "HEAD"])
            .trim()
            .to_string();
        assert!(
            !initial_head_oid.is_empty(),
            "initial SQLite HEAD commit OID must be non-empty"
        );

        // Seed a pre-migration capture session in `.libra/private/capture.db`.
        let private_dir = dot_libra.join("private");
        fs::create_dir_all(&private_dir).expect("mkdir .libra/private");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&private_dir, fs::Permissions::from_mode(0o700))
                .expect("chmod 0700 .libra/private");
        }
        let sqlite_cap_path = private_dir.join("capture.db");
        let sqlite_session = make_roundtrip_session("rb29c-sqlite-seed-session", 1_760_200_000);
        rt.block_on(async {
            let cap_store = SqliteCaptureStore::create(&sqlite_cap_path)
                .await
                .expect("create SQLite capture store");
            cap_store
                .put_copy_receipt(&libra::storage::CaptureCatalogCopyReceiptRecord {
                    id: 1,
                    copied_at: "2026-11-26T00:00:00Z".to_string(),
                })
                .await
                .expect("seed Layout B capture_catalog_copy_receipt");
            cap_store
                .upsert_session(&sqlite_session)
                .await
                .expect("seed SQLite capture session");
        });

        // -----------------------------------------------------------------
        // 2. Real `libra-migrate --to redb --repo <repo>` forward cutover.
        // -----------------------------------------------------------------
        let repo_arg = repo_dir.to_str().expect("utf8 repo path");
        let fwd_out = run_migrate(&["--to", "redb", "--repo", repo_arg]);
        assert!(
            fwd_out.contains("Migrated repository to redb"),
            "unexpected forward migration stdout: {fwd_out}"
        );

        let fwd_manifest = read_storage_manifest(&dot_libra)
            .expect("read manifest after forward migration")
            .expect("storage-format.json must exist after forward migration");
        assert_eq!(fwd_manifest.backend, StorageBackendKind::Redb);
        assert!(
            fwd_manifest.finalized,
            "forward migration manifest must be finalized"
        );
        let redb_paths = resolve_redb_generation_paths(&dot_libra)
            .expect("resolve redb generation paths")
            .expect("redb generation paths must be present");
        assert!(
            redb_paths.git_db_path.is_file(),
            "expected git.gen-*.redb to exist at {}",
            redb_paths.git_db_path.display()
        );
        assert!(
            redb_paths.capture_db_path.is_file(),
            "expected capture.gen-*.redb to exist at {}",
            redb_paths.capture_db_path.display()
        );
        assert!(
            !sqlite_git_path.exists(),
            "legacy libra.db must be unlinked after finalized forward migration"
        );

        // -----------------------------------------------------------------
        // 3. Perform NEW writes on the Redb repository via unlocked `libra` CLI.
        // -----------------------------------------------------------------
        let origin_on_redb = run_libra(
            &repo_dir,
            Some("redb"),
            &["config", "get", "roundtrip.origin"],
        );
        assert_eq!(origin_on_redb.trim(), "sqlite-initial");

        let sqlite_branch_on_redb =
            run_libra(&repo_dir, Some("redb"), &["rev-parse", "sqlite-branch"]);
        assert_eq!(sqlite_branch_on_redb.trim(), initial_head_oid);

        run_libra(
            &repo_dir,
            Some("redb"),
            &["config", "roundtrip.redb_write", "written-on-redb"],
        );
        fs::write(repo_dir.join("redb.txt"), "written while on redb\n").expect("write redb.txt");
        run_libra(&repo_dir, Some("redb"), &["add", "redb.txt"]);
        run_libra(
            &repo_dir,
            Some("redb"),
            &["commit", "-m", "post-cutover redb commit", "--no-verify"],
        );
        run_libra(&repo_dir, Some("redb"), &["branch", "redb-branch"]);

        let redb_head_oid = run_libra(&repo_dir, Some("redb"), &["rev-parse", "HEAD"])
            .trim()
            .to_string();
        assert_ne!(
            redb_head_oid, initial_head_oid,
            "new commit on Redb must advance HEAD past initial SQLite commit"
        );

        // Record a new capture session while the repository is running on Redb.
        let redb_session = make_roundtrip_session("rb29c-redb-live-session", 1_760_200_100);
        rt.block_on(async {
            let cap_store = RedbCaptureStore::open_read_write(
                &redb_paths.capture_db_path,
                RedbRepairContext::WriteCommand,
            )
            .await
            .expect("open Redb capture store");
            cap_store
                .upsert_session(&redb_session)
                .await
                .expect("write post-cutover Redb capture session");
        });

        // Also verify fresh `libra init` honors explicit `LIBRA_STORAGE_BACKEND=redb`.
        run_libra(&fresh_redb_dir, Some("redb"), &["init", "--vault", "false"]);
        let fresh_dot_libra = fresh_redb_dir.join(".libra");
        let fresh_manifest = read_storage_manifest(&fresh_dot_libra)
            .expect("read fresh redb init manifest")
            .expect("fresh LIBRA_STORAGE_BACKEND=redb init must publish storage-format.json");
        assert_eq!(fresh_manifest.backend, StorageBackendKind::Redb);
        assert!(fresh_manifest.finalized);
        assert!(
            !fresh_dot_libra.join("libra.db").exists(),
            "fresh LIBRA_STORAGE_BACKEND=redb init must not create libra.db"
        );

        // -----------------------------------------------------------------
        // 4. Real `libra-migrate --to sqlite --repo <repo>` reverse cutover.
        // -----------------------------------------------------------------
        let rev_out = run_migrate(&["--to", "sqlite", "--repo", repo_arg]);
        assert!(
            rev_out.contains("Migrated repository to sqlite"),
            "unexpected reverse migration stdout: {rev_out}"
        );

        let rev_manifest = read_storage_manifest(&dot_libra)
            .expect("read manifest after reverse migration")
            .expect("storage-format.json must exist after reverse migration");
        assert_eq!(rev_manifest.backend, StorageBackendKind::Sqlite);
        assert!(
            rev_manifest.finalized,
            "reverse migration manifest must be finalized"
        );
        assert!(
            sqlite_git_path.is_file(),
            "libra.db must be restored after reverse migration"
        );

        // -----------------------------------------------------------------
        // 5. Verify 100% readback consistency from restored SQLite repository.
        // -----------------------------------------------------------------
        assert_eq!(
            run_libra(&repo_dir, None, &["rev-parse", "HEAD"]).trim(),
            redb_head_oid
        );
        assert_eq!(
            run_libra(&repo_dir, None, &["rev-parse", "sqlite-branch"]).trim(),
            initial_head_oid
        );
        assert_eq!(
            run_libra(&repo_dir, None, &["rev-parse", "redb-branch"]).trim(),
            redb_head_oid
        );
        assert_eq!(
            run_libra(&repo_dir, None, &["config", "get", "roundtrip.origin"]).trim(),
            "sqlite-initial"
        );
        assert_eq!(
            run_libra(&repo_dir, None, &["config", "get", "roundtrip.redb_write"]).trim(),
            "written-on-redb"
        );
        let head_commit_body = run_libra(&repo_dir, None, &["cat-file", "-p", "HEAD"]);
        assert!(
            head_commit_body.contains("post-cutover redb commit")
                && head_commit_body.contains(&initial_head_oid),
            "restored HEAD commit must preserve post-cutover message and parent OID: {head_commit_body}"
        );
        let seed_commit_body = run_libra(&repo_dir, None, &["cat-file", "-p", "sqlite-branch"]);
        assert!(
            seed_commit_body.contains("seed sqlite commit"),
            "restored sqlite-branch commit must preserve original message: {seed_commit_body}"
        );

        // Direct SQLite store readback verification across Git, Config, and Capture stores.
        rt.block_on(async {
            let restored_git = SqliteGitStore::open_read_only(&sqlite_git_path)
                .await
                .expect("open restored SqliteGitStore");
            assert_eq!(
                restored_git
                    .get_ref(&RefKey::LocalBranch {
                        name: "sqlite-branch".to_string(),
                    })
                    .await
                    .expect("read sqlite-branch"),
                Some(RefTarget::Direct(initial_head_oid.clone()))
            );
            assert_eq!(
                restored_git
                    .get_ref(&RefKey::LocalBranch {
                        name: "redb-branch".to_string(),
                    })
                    .await
                    .expect("read redb-branch"),
                Some(RefTarget::Direct(redb_head_oid.clone()))
            );

            let restored_cfg =
                SqliteConfigStore::open_read_only(&sqlite_git_path, DatabaseRole::Repository)
                    .await
                    .expect("open restored SqliteConfigStore");
            assert_eq!(
                restored_cfg
                    .get("roundtrip.origin")
                    .await
                    .expect("read roundtrip.origin")
                    .map(|e| e.value),
                Some("sqlite-initial".to_string())
            );
            assert_eq!(
                restored_cfg
                    .get("roundtrip.redb_write")
                    .await
                    .expect("read roundtrip.redb_write")
                    .map(|e| e.value),
                Some("written-on-redb".to_string())
            );

            let restored_cap = SqliteCaptureStore::open_read_only(&sqlite_cap_path)
                .await
                .expect("open restored SqliteCaptureStore");
            let restored_sqlite_session = restored_cap
                .get_session("rb29c-sqlite-seed-session")
                .await
                .expect("read restored sqlite capture session")
                .expect("pre-migration capture session must survive roundtrip");
            assert_eq!(
                restored_sqlite_session.session_id,
                sqlite_session.session_id
            );
            let restored_redb_session = restored_cap
                .get_session("rb29c-redb-live-session")
                .await
                .expect("read restored redb capture session")
                .expect("post-cutover Redb capture session must survive reverse migration");
            assert_eq!(restored_redb_session.session_id, redb_session.session_id);
        });
    }
}

#[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dual_feature_migration_and_stress_roundtrip() {
    dual_feature_stress::dual_feature_migration_and_stress_roundtrip_impl().await;
}

#[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]
#[test]
fn post_unlock_libra_migrate_binary_bidirectional_roundtrip() {
    dual_feature_stress::post_unlock_libra_migrate_binary_bidirectional_roundtrip_impl();
}

#[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]
mod rb29b_ast_audit {
    use std::{
        collections::{BTreeMap, BTreeSet, HashMap},
        fmt, fs,
        path::{Path, PathBuf},
    };

    use syn::{parse::Parse, spanned::Spanned, visit::Visit};

    /// Explicitly exempt files per Requirement #5 / RB-29b:
    /// 3 external read-only SQLite parsers + 3 forbidden-to-modify files.
    const EXEMPT_FILES: &[&str] = &[
        "src/internal/ai/capture/store.rs",
        "src/internal/ai/capture/detach.rs",
        "src/internal/ai/provider_index_io.rs",
        "src/internal/ai/observed_agents/codex_index.rs",
        "src/internal/ai/observed_agents/builtin/hermes.rs",
        "src/utils/shared_perm.rs",
    ];

    /// Direct SQLite/SeaORM database opener function names (unqualified or final segment).
    const DIRECT_OPENER_FNS: &[&str] = &[
        "get_db_conn_instance",
        "get_db_conn_instance_for_path",
        "open_cached_repository",
        "open_database_without_migrations",
        "open_connection_without_schema_management",
        "open_repository_with_busy_timeout",
        "open_readonly_connection_for_role",
        "establish_connection",
        "establish_connection_for_role",
        "establish_connection_with_lock_timeout",
        "establish_connection_with_lock_timeout_for_role",
        "create_database",
        "create_database_for_role",
        "request_db",
        "request_db_checked",
    ];

    /// Connection-derived SQLite peer helpers defined in `src/internal/db.rs` that take an
    /// already-open `&DatabaseConnection` / `&impl ConnectionTrait`. They cannot open a DB
    /// cold; they are allowed inside connection-passing SQLite functions or paired
    /// `StorageBackendKind::Sqlite`/`Redb` dispatch, and rejected from unwired callers.
    const CONN_DERIVED_PEER_FNS: &[&str] = &["open_paired_repository", "repository_peer"];

    /// Capture store SQLite opener functions (`capture::store::open*`).
    const CAPTURE_STORE_OPENERS: &[&str] = &[
        "open",
        "open_readonly",
        "open_current",
        "open_existing_writer",
    ];

    /// Concrete SQLite store wrappers whose `open*` / `create` constructors open SQLite.
    const SQLITE_STORE_TYPES: &[&str] = &[
        "SqliteGitStore",
        "SqliteGitStateStore",
        "SqliteCaptureStore",
        "SqliteConfigStore",
    ];

    /// SeaORM / SQLite connection type identifiers in function signatures.
    const CONN_TYPE_IDENTS: &[&str] = &[
        "DatabaseConnection",
        "DatabaseTransaction",
        "DbConn",
        "ConnectionTrait",
    ];

    /// Direct SeaORM raw query / SQLite write-tx helpers that must never run outside
    /// a connection-passing function or paired `StorageBackendKind::Sqlite`/`Redb` dispatch.
    const DIRECT_SQLITE_QUERY_METHODS: &[&str] = &[
        "execute_raw",
        "execute_unprepared",
        "query_one_raw",
        "query_all_raw",
    ];

    #[derive(Debug, Clone)]
    pub(super) struct SourceUnit {
        pub(super) path: String,
        pub(super) source: String,
    }

    #[derive(Debug, Clone)]
    pub(super) struct AuditParseError {
        pub(super) path: String,
        pub(super) line: usize,
        pub(super) message: String,
    }

    impl fmt::Display for AuditParseError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(
                f,
                "fail-closed syn parse error in {}:{}: {}",
                self.path, self.line, self.message
            )
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
    pub(super) struct AuditViolation {
        pub(super) path: String,
        pub(super) line: usize,
        pub(super) function: String,
        pub(super) kind: &'static str,
        pub(super) detail: String,
    }

    impl fmt::Display for AuditViolation {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(
                f,
                "{}:{}:{} [{}] {}",
                self.path, self.line, self.function, self.kind, self.detail
            )
        }
    }

    fn normalize_rel(path: &Path) -> String {
        path.to_string_lossy().replace('\\', "/")
    }

    fn is_statically_excluded_path(rel: &str) -> bool {
        if rel.starts_with("src/storage/")
            || rel == "src/internal/db.rs"
            || rel.starts_with("src/internal/db/")
            || rel.starts_with("src/internal/model/")
            || rel.starts_with("src/bin/")
        {
            return true;
        }
        if EXEMPT_FILES.contains(&rel) {
            return true;
        }
        if rel.contains("/tests/") {
            return true;
        }
        let path = Path::new(rel);
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            && (matches!(stem, "test" | "tests")
                || stem.ends_with("_test")
                || stem.ends_with("_tests"))
        {
            return true;
        }
        false
    }

    fn collect_src_files(repo_root: &Path) -> Vec<SourceUnit> {
        let src_dir = repo_root.join("src");
        let mut paths = Vec::new();
        let mut stack = vec![src_dir];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir)
                .unwrap_or_else(|err| panic!("failed to read '{}': {err}", dir.display()))
            {
                let entry = entry.unwrap_or_else(|err| {
                    panic!("failed to read entry in '{}': {err}", dir.display())
                });
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    paths.push(path);
                }
            }
        }
        paths.sort();

        let mut units = Vec::new();
        for path in paths {
            let rel = path
                .strip_prefix(repo_root)
                .map(normalize_rel)
                .unwrap_or_else(|_| normalize_rel(&path));
            if is_statically_excluded_path(&rel) {
                continue;
            }
            let source = fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("failed to read '{}': {err}", path.display()));
            units.push(SourceUnit { path: rel, source });
        }
        units
    }

    /// True ONLY for exact `#[cfg(test)]` or `#[cfg(all(test, ...))]` attributes.
    /// `#[cfg(any(test, ...))]`, `#[cfg(not(test))]`, and `#[cfg(feature = "storage-sqlite")]`
    /// all compile in production and MUST return `false`.
    fn is_cfg_test_meta(meta: &syn::Meta) -> bool {
        match meta {
            syn::Meta::Path(path) => path.is_ident("test"),
            syn::Meta::List(list) if list.path.is_ident("all") => {
                let mut found_test = false;
                let _ = list.parse_nested_meta(|nested| {
                    if nested.path.is_ident("test") {
                        found_test = true;
                        Ok(())
                    } else if nested.path.is_ident("all") {
                        let content;
                        syn::parenthesized!(content in nested.input);
                        let nested_metas: syn::punctuated::Punctuated<syn::Meta, syn::Token![,]> =
                            content.parse_terminated(syn::Meta::parse, syn::Token![,])?;
                        if nested_metas.iter().any(is_cfg_test_meta) {
                            found_test = true;
                        }
                        Ok(())
                    } else {
                        if nested.input.peek(syn::Token![=]) {
                            let _eq: syn::Token![=] = nested.input.parse()?;
                            let _val: syn::Expr = nested.input.parse()?;
                        } else if nested.input.peek(syn::token::Paren) {
                            let content;
                            syn::parenthesized!(content in nested.input);
                            let _: proc_macro2::TokenStream = content.parse()?;
                        }
                        Ok(())
                    }
                });
                found_test
            }
            _ => false,
        }
    }

    fn has_cfg_test(attrs: &[syn::Attribute]) -> bool {
        attrs.iter().any(|attr| {
            if !attr.path().is_ident("cfg") {
                return false;
            }
            attr.parse_args::<syn::Meta>()
                .map(|meta| is_cfg_test_meta(&meta))
                .unwrap_or(false)
        })
    }

    fn extract_path_attr(attrs: &[syn::Attribute]) -> Option<String> {
        for attr in attrs {
            if !attr.path().is_ident("path") {
                continue;
            }
            if let syn::Meta::NameValue(nv) = &attr.meta
                && let syn::Expr::Lit(expr_lit) = &nv.value
                && let syn::Lit::Str(lit_str) = &expr_lit.lit
            {
                return Some(lit_str.value());
            }
        }
        None
    }

    fn syn_item_attrs(item: &syn::Item) -> &[syn::Attribute] {
        match item {
            syn::Item::Const(i) => &i.attrs,
            syn::Item::Enum(i) => &i.attrs,
            syn::Item::ExternCrate(i) => &i.attrs,
            syn::Item::Fn(i) => &i.attrs,
            syn::Item::ForeignMod(i) => &i.attrs,
            syn::Item::Impl(i) => &i.attrs,
            syn::Item::Macro(i) => &i.attrs,
            syn::Item::Mod(i) => &i.attrs,
            syn::Item::Static(i) => &i.attrs,
            syn::Item::Struct(i) => &i.attrs,
            syn::Item::Trait(i) => &i.attrs,
            syn::Item::TraitAlias(i) => &i.attrs,
            syn::Item::Type(i) => &i.attrs,
            syn::Item::Union(i) => &i.attrs,
            syn::Item::Use(i) => &i.attrs,
            _ => &[],
        }
    }

    fn syn_impl_item_attrs(item: &syn::ImplItem) -> &[syn::Attribute] {
        match item {
            syn::ImplItem::Const(i) => &i.attrs,
            syn::ImplItem::Fn(i) => &i.attrs,
            syn::ImplItem::Type(i) => &i.attrs,
            syn::ImplItem::Macro(i) => &i.attrs,
            _ => &[],
        }
    }

    fn syn_trait_item_attrs(item: &syn::TraitItem) -> &[syn::Attribute] {
        match item {
            syn::TraitItem::Const(i) => &i.attrs,
            syn::TraitItem::Fn(i) => &i.attrs,
            syn::TraitItem::Type(i) => &i.attrs,
            syn::TraitItem::Macro(i) => &i.attrs,
            _ => &[],
        }
    }

    fn syn_stmt_attrs(stmt: &syn::Stmt) -> &[syn::Attribute] {
        match stmt {
            syn::Stmt::Local(l) => &l.attrs,
            syn::Stmt::Item(i) => syn_item_attrs(i),
            syn::Stmt::Expr(e, _) => syn_expr_attrs(e),
            syn::Stmt::Macro(m) => &m.attrs,
        }
    }

    fn syn_expr_attrs(expr: &syn::Expr) -> &[syn::Attribute] {
        match expr {
            syn::Expr::Array(e) => &e.attrs,
            syn::Expr::Assign(e) => &e.attrs,
            syn::Expr::Async(e) => &e.attrs,
            syn::Expr::Await(e) => &e.attrs,
            syn::Expr::Binary(e) => &e.attrs,
            syn::Expr::Block(e) => &e.attrs,
            syn::Expr::Break(e) => &e.attrs,
            syn::Expr::Call(e) => &e.attrs,
            syn::Expr::Cast(e) => &e.attrs,
            syn::Expr::Closure(e) => &e.attrs,
            syn::Expr::Const(e) => &e.attrs,
            syn::Expr::Continue(e) => &e.attrs,
            syn::Expr::Field(e) => &e.attrs,
            syn::Expr::ForLoop(e) => &e.attrs,
            syn::Expr::Group(e) => &e.attrs,
            syn::Expr::If(e) => &e.attrs,
            syn::Expr::Index(e) => &e.attrs,
            syn::Expr::Infer(e) => &e.attrs,
            syn::Expr::Let(e) => &e.attrs,
            syn::Expr::Lit(e) => &e.attrs,
            syn::Expr::Loop(e) => &e.attrs,
            syn::Expr::Macro(e) => &e.attrs,
            syn::Expr::Match(e) => &e.attrs,
            syn::Expr::MethodCall(e) => &e.attrs,
            syn::Expr::Paren(e) => &e.attrs,
            syn::Expr::Path(e) => &e.attrs,
            syn::Expr::Range(e) => &e.attrs,
            syn::Expr::RawAddr(e) => &e.attrs,
            syn::Expr::Reference(e) => &e.attrs,
            syn::Expr::Repeat(e) => &e.attrs,
            syn::Expr::Return(e) => &e.attrs,
            syn::Expr::Struct(e) => &e.attrs,
            syn::Expr::Try(e) => &e.attrs,
            syn::Expr::TryBlock(e) => &e.attrs,
            syn::Expr::Tuple(e) => &e.attrs,
            syn::Expr::Unary(e) => &e.attrs,
            syn::Expr::Unsafe(e) => &e.attrs,
            syn::Expr::While(e) => &e.attrs,
            syn::Expr::Yield(e) => &e.attrs,
            _ => &[],
        }
    }

    fn path_segments(path: &syn::Path) -> Vec<String> {
        path.segments.iter().map(|s| s.ident.to_string()).collect()
    }

    fn path_to_string(path: &syn::Path) -> String {
        path_segments(path).join("::")
    }

    fn self_type_name(ty: &syn::Type) -> Option<String> {
        match ty {
            syn::Type::Path(tp) => tp.path.segments.last().map(|s| s.ident.to_string()),
            syn::Type::Reference(r) => self_type_name(&r.elem),
            syn::Type::Paren(p) => self_type_name(&p.elem),
            syn::Type::Group(g) => self_type_name(&g.elem),
            _ => None,
        }
    }

    /// Resolve out-of-line `#[cfg(test)] mod foo;` / `#[cfg(test)] #[path = "..."] mod foo;`
    /// declarations across parsed files so test-only helper files (like `src/internal/source_scan.rs`)
    /// are excluded without guessing.
    fn collect_cfg_test_out_of_line_modules(parsed: &[(String, syn::File)]) -> BTreeSet<String> {
        struct OutOfLineModFinder {
            parent_rel: String,
            cfg_test_targets: Vec<String>,
            non_test_targets: Vec<String>,
            in_cfg_test: bool,
        }

        impl OutOfLineModFinder {
            fn resolve_mod_candidates(
                &self,
                mod_ident: &str,
                path_attr: Option<&str>,
            ) -> Vec<String> {
                let parent = Path::new(&self.parent_rel);
                let parent_dir = parent.parent().unwrap_or_else(|| Path::new(""));
                let file_name = parent.file_name().and_then(|s| s.to_str()).unwrap_or("");
                let file_stem = parent.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                let is_mod_root = matches!(file_name, "mod.rs" | "lib.rs" | "main.rs");

                if let Some(explicit) = path_attr {
                    let base = if is_mod_root {
                        parent_dir.to_path_buf()
                    } else {
                        // Rust resolves `#[path]` on a non-mod.rs file relative to parent_dir first
                        // (or subdir for inline mods); record both to be safe.
                        parent_dir.to_path_buf()
                    };
                    let mut out = vec![normalize_rel(&base.join(explicit))];
                    if !is_mod_root {
                        out.push(normalize_rel(&parent_dir.join(file_stem).join(explicit)));
                    }
                    return out;
                }

                let base = if is_mod_root {
                    parent_dir.to_path_buf()
                } else {
                    parent_dir.join(file_stem)
                };
                vec![
                    normalize_rel(&base.join(format!("{mod_ident}.rs"))),
                    normalize_rel(&base.join(mod_ident).join("mod.rs")),
                ]
            }
        }

        impl<'ast> Visit<'ast> for OutOfLineModFinder {
            fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
                let is_test = self.in_cfg_test || has_cfg_test(&item.attrs);
                if item.content.is_none() {
                    let ident = item.ident.to_string();
                    let path_attr = extract_path_attr(&item.attrs);
                    let candidates = self.resolve_mod_candidates(&ident, path_attr.as_deref());
                    if is_test {
                        self.cfg_test_targets.extend(candidates);
                    } else {
                        self.non_test_targets.extend(candidates);
                    }
                    return;
                }
                let prev = self.in_cfg_test;
                self.in_cfg_test = is_test;
                syn::visit::visit_item_mod(self, item);
                self.in_cfg_test = prev;
            }
        }

        let mut test_only = BTreeSet::new();
        let mut non_test = BTreeSet::new();
        for (rel, file) in parsed {
            let mut finder = OutOfLineModFinder {
                parent_rel: rel.clone(),
                cfg_test_targets: Vec::new(),
                non_test_targets: Vec::new(),
                in_cfg_test: false,
            };
            finder.visit_file(file);
            test_only.extend(finder.cfg_test_targets);
            non_test.extend(finder.non_test_targets);
        }
        // Only treat a file as test-only if it is NEVER included as a non-test module.
        for prod in non_test {
            test_only.remove(&prod);
        }
        test_only
    }

    /// Collect `use` aliases and glob imports in a scope so renamed imports
    /// (e.g. `use crate::internal::db::get_db_conn_instance as open_it;`) cannot evade detection.
    fn collect_use_tree(
        tree: &syn::UseTree,
        prefix: &str,
        aliases: &mut HashMap<String, String>,
        has_storage_backend_glob: &mut bool,
        has_rusqlite_import: &mut bool,
    ) {
        let join = |p: &str, seg: &str| {
            if p.is_empty() {
                seg.to_string()
            } else {
                format!("{p}::{seg}")
            }
        };
        match tree {
            syn::UseTree::Path(p) => {
                let next = join(prefix, &p.ident.to_string());
                if p.ident == "rusqlite" {
                    *has_rusqlite_import = true;
                }
                collect_use_tree(
                    &p.tree,
                    &next,
                    aliases,
                    has_storage_backend_glob,
                    has_rusqlite_import,
                );
            }
            syn::UseTree::Name(n) => {
                let ident = n.ident.to_string();
                if ident == "self" {
                    if let Some(last) = prefix.rsplit("::").next() {
                        aliases.insert(last.to_string(), prefix.to_string());
                    }
                } else {
                    let full = join(prefix, &ident);
                    if full.contains("rusqlite") {
                        *has_rusqlite_import = true;
                    }
                    aliases.insert(ident, full);
                }
            }
            syn::UseTree::Rename(r) => {
                let orig = r.ident.to_string();
                let alias = r.rename.to_string();
                let full = if orig == "self" {
                    prefix.to_string()
                } else {
                    join(prefix, &orig)
                };
                if full.contains("rusqlite") {
                    *has_rusqlite_import = true;
                }
                aliases.insert(alias, full);
            }
            syn::UseTree::Glob(_) => {
                if prefix.ends_with("StorageBackendKind") {
                    *has_storage_backend_glob = true;
                }
                if prefix.contains("rusqlite") {
                    *has_rusqlite_import = true;
                }
            }
            syn::UseTree::Group(g) => {
                for item in &g.items {
                    collect_use_tree(
                        item,
                        prefix,
                        aliases,
                        has_storage_backend_glob,
                        has_rusqlite_import,
                    );
                }
            }
        }
    }

    fn expand_path_with_aliases(path: &str, aliases: &HashMap<String, String>) -> String {
        let mut parts = path.splitn(2, "::");
        let Some(first) = parts.next() else {
            return path.to_string();
        };
        if let Some(target) = aliases.get(first) {
            if let Some(rest) = parts.next() {
                format!("{target}::{rest}")
            } else {
                target.clone()
            }
        } else {
            path.to_string()
        }
    }

    /// Inspect generics + where clause for type parameters bounded by `ConnectionTrait`.
    fn collect_conn_trait_generics(generics: &syn::Generics) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for param in &generics.params {
            if let syn::GenericParam::Type(tp) = param
                && bounds_mention_connection_trait(&tp.bounds)
            {
                out.insert(tp.ident.to_string());
            }
        }
        if let Some(wc) = &generics.where_clause {
            for pred in &wc.predicates {
                if let syn::WherePredicate::Type(pt) = pred
                    && bounds_mention_connection_trait(&pt.bounds)
                    && let Some(name) = self_type_name(&pt.bounded_ty)
                {
                    out.insert(name);
                }
            }
        }
        out
    }

    fn bounds_mention_connection_trait(
        bounds: &syn::punctuated::Punctuated<syn::TypeParamBound, syn::Token![+]>,
    ) -> bool {
        bounds.iter().any(|b| match b {
            syn::TypeParamBound::Trait(tb) => tb
                .path
                .segments
                .last()
                .is_some_and(|s| s.ident == "ConnectionTrait"),
            _ => false,
        })
    }

    /// True if a struct field or function parameter MANDATORILY (non-`Option`) holds a SeaORM/SQLite connection
    /// (e.g. `DatabaseConnection`, `&'a DatabaseConnection`, `DbConn`, `&'a C` where `C: ConnectionTrait`).
    /// Deliberately returns `false` for `Option<DatabaseConnection>` or enums so dual-backend
    /// context structs cannot be confused with SQLite-only connection bundles.
    fn field_mandatorily_holds_conn(
        ty: &syn::Type,
        conn_generics: &BTreeSet<String>,
        conn_holding_structs: &BTreeSet<String>,
    ) -> bool {
        match ty {
            syn::Type::Reference(r) => {
                field_mandatorily_holds_conn(&r.elem, conn_generics, conn_holding_structs)
            }
            syn::Type::Paren(p) => {
                field_mandatorily_holds_conn(&p.elem, conn_generics, conn_holding_structs)
            }
            syn::Type::Group(g) => {
                field_mandatorily_holds_conn(&g.elem, conn_generics, conn_holding_structs)
            }
            syn::Type::Path(tp) => {
                let Some(last) = tp.path.segments.last() else {
                    return false;
                };
                let ident = last.ident.to_string();
                if ident == "Option" || ident == "Vec" || ident == "Result" {
                    return false;
                }
                if CONN_TYPE_IDENTS.contains(&ident.as_str())
                    || conn_holding_structs.contains(&ident)
                    || (tp.path.segments.len() == 1 && conn_generics.contains(&ident))
                {
                    return true;
                }
                // Check single-wrapper smart pointers like `Arc<DatabaseConnection>`, `Box<...>`.
                if matches!(ident.as_str(), "Arc" | "Rc" | "Box" | "Mutex" | "RwLock")
                    && let syn::PathArguments::AngleBracketed(ab) = &last.arguments
                {
                    return ab.args.iter().any(|arg| {
                        matches!(arg, syn::GenericArgument::Type(inner) if field_mandatorily_holds_conn(inner, conn_generics, conn_holding_structs))
                    });
                }
                false
            }
            _ => false,
        }
    }

    fn type_mentions_conn_or_bundle(
        ty: &syn::Type,
        conn_generics: &BTreeSet<String>,
        conn_holding_structs: &BTreeSet<String>,
    ) -> bool {
        // Only mandatory (non-`Option`, non-`Vec`, non-`Result`) connection parameters
        // or mandatory-conn struct bundles make a function a SQLite connection-passing function.
        // Dual-backend helpers taking `Option<&DatabaseConnection>` alongside a `GitStore` are
        // audited directly on their bodies instead.
        if field_mandatorily_holds_conn(ty, conn_generics, conn_holding_structs) {
            return true;
        }
        // Also recognize `impl ConnectionTrait` and `dyn ConnectionTrait` parameter types.
        match ty {
            syn::Type::Reference(r) => {
                type_mentions_conn_or_bundle(&r.elem, conn_generics, conn_holding_structs)
            }
            syn::Type::ImplTrait(it) => bounds_mention_connection_trait(&it.bounds),
            syn::Type::TraitObject(to) => bounds_mention_connection_trait(&to.bounds),
            syn::Type::Paren(p) => {
                type_mentions_conn_or_bundle(&p.elem, conn_generics, conn_holding_structs)
            }
            syn::Type::Group(g) => {
                type_mentions_conn_or_bundle(&g.elem, conn_generics, conn_holding_structs)
            }
            _ => false,
        }
    }

    fn signature_accepts_conn(
        sig: &syn::Signature,
        impl_conn_generics: &BTreeSet<String>,
        impl_type_holds_conn: bool,
        conn_holding_structs: &BTreeSet<String>,
    ) -> bool {
        let name = sig.ident.to_string();
        if name.ends_with("_with_conn") {
            return true;
        }
        let mut conn_generics = impl_conn_generics.clone();
        conn_generics.extend(collect_conn_trait_generics(&sig.generics));

        for input in &sig.inputs {
            match input {
                syn::FnArg::Receiver(_) => {
                    if !impl_conn_generics.is_empty() || impl_type_holds_conn {
                        return true;
                    }
                }
                syn::FnArg::Typed(pt) => {
                    if type_mentions_conn_or_bundle(&pt.ty, &conn_generics, conn_holding_structs) {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn canonical_module_path_from_file(rel: &str) -> String {
        let trimmed = rel.strip_prefix("src/").unwrap_or(rel);
        let without_ext = trimmed.strip_suffix(".rs").unwrap_or(trimmed);
        if without_ext == "lib" || without_ext == "main" {
            return "crate".to_string();
        }
        let mod_part = without_ext.strip_suffix("/mod").unwrap_or(without_ext);
        format!("crate::{}", mod_part.replace('/', "::"))
    }

    fn stmt_has_cfg_attr(attrs: &[syn::Attribute]) -> bool {
        attrs.iter().any(|a| a.path().is_ident("cfg"))
    }

    fn expr_unconditionally_diverges(expr: &syn::Expr) -> bool {
        match expr {
            syn::Expr::Return(_) | syn::Expr::Break(_) | syn::Expr::Continue(_) => true,
            syn::Expr::Macro(em) => em.mac.path.segments.last().is_some_and(|s| {
                matches!(
                    s.ident.to_string().as_str(),
                    "panic" | "unreachable" | "todo" | "unimplemented"
                )
            }),
            syn::Expr::Block(eb) => block_unconditionally_diverges(&eb.block),
            syn::Expr::Group(g) => expr_unconditionally_diverges(&g.expr),
            syn::Expr::Paren(p) => expr_unconditionally_diverges(&p.expr),
            syn::Expr::If(ei) => {
                block_unconditionally_diverges(&ei.then_branch)
                    && ei
                        .else_branch
                        .as_ref()
                        .is_some_and(|(_, eb)| expr_unconditionally_diverges(eb))
            }
            syn::Expr::Match(em) => {
                let active: Vec<&syn::Arm> =
                    em.arms.iter().filter(|a| !has_cfg_test(&a.attrs)).collect();
                !active.is_empty()
                    && active
                        .iter()
                        .all(|a| expr_unconditionally_diverges(&a.body))
            }
            _ => false,
        }
    }

    fn stmt_unconditionally_diverges(stmt: &syn::Stmt) -> bool {
        match stmt {
            syn::Stmt::Expr(e, _) => expr_unconditionally_diverges(e),
            syn::Stmt::Macro(m) => m.mac.path.segments.last().is_some_and(|s| {
                matches!(
                    s.ident.to_string().as_str(),
                    "panic" | "unreachable" | "todo" | "unimplemented"
                )
            }),
            _ => false,
        }
    }

    fn block_unconditionally_diverges(block: &syn::Block) -> bool {
        let active: Vec<&syn::Stmt> = block
            .stmts
            .iter()
            .filter(|s| !has_cfg_test(syn_stmt_attrs(s)))
            .collect();
        if active.is_empty() {
            return false;
        }
        // Any unguarded statement that unconditionally diverges makes the whole block diverge.
        for stmt in &active {
            if !stmt_has_cfg_attr(syn_stmt_attrs(stmt)) && stmt_unconditionally_diverges(stmt) {
                return true;
            }
        }
        // If the block consists solely of mutually-gated `#[cfg(...)]` statements/blocks
        // (e.g., `#[cfg(feature = "storage-redb")] { return ...; } #[cfg(not(feature = "storage-redb"))] { return Err(...); }`)
        // and EVERY one of them unconditionally diverges, then the block unconditionally diverges.
        active
            .iter()
            .all(|s| stmt_has_cfg_attr(syn_stmt_attrs(s)) && stmt_unconditionally_diverges(s))
    }

    /// Returns true if `expr` is a paired `StorageBackendKind` `match` where EVERY non-`Sqlite`
    /// arm (including `StorageBackendKind::Redb`) unconditionally diverges, so statements
    /// following this `match` in the same enclosing block can ONLY execute on `Sqlite`.
    fn match_dominates_remainder_as_sqlite(
        expr: &syn::ExprMatch,
        allow_bare_variant: bool,
    ) -> bool {
        let active_arms: Vec<&syn::Arm> = expr
            .arms
            .iter()
            .filter(|arm| !has_cfg_test(&arm.attrs))
            .collect();
        let has_sbk_sqlite = active_arms.iter().any(|arm| {
            pat_matches_variant(
                &arm.pat,
                Some("StorageBackendKind"),
                "Sqlite",
                allow_bare_variant,
            )
        });
        let has_sbk_redb = active_arms.iter().any(|arm| {
            pat_matches_variant(
                &arm.pat,
                Some("StorageBackendKind"),
                "Redb",
                allow_bare_variant,
            )
        });
        if !(has_sbk_sqlite && has_sbk_redb) {
            return false;
        }
        active_arms.iter().all(|arm| {
            let is_sqlite = pat_matches_variant(
                &arm.pat,
                Some("StorageBackendKind"),
                "Sqlite",
                allow_bare_variant,
            );
            let is_redb = pat_matches_variant(
                &arm.pat,
                Some("StorageBackendKind"),
                "Redb",
                allow_bare_variant,
            );
            if is_sqlite && !is_redb {
                true
            } else {
                expr_unconditionally_diverges(&arm.body)
            }
        })
    }

    fn stmt_dominates_remainder_as_sqlite(stmt: &syn::Stmt, allow_bare_variant: bool) -> bool {
        fn extract_match(e: &syn::Expr) -> Option<&syn::ExprMatch> {
            match e {
                syn::Expr::Match(m) => Some(m),
                syn::Expr::Try(t) => match t.expr.as_ref() {
                    syn::Expr::Match(m) => Some(m),
                    _ => None,
                },
                syn::Expr::Await(a) => match a.base.as_ref() {
                    syn::Expr::Match(m) => Some(m),
                    _ => None,
                },
                _ => None,
            }
        }
        match stmt {
            syn::Stmt::Local(local) => local
                .init
                .as_ref()
                .and_then(|init| extract_match(&init.expr))
                .is_some_and(|m| match_dominates_remainder_as_sqlite(m, allow_bare_variant)),
            syn::Stmt::Expr(expr, _) => extract_match(expr)
                .is_some_and(|m| match_dominates_remainder_as_sqlite(m, allow_bare_variant)),
            _ => false,
        }
    }

    /// Whole-tree Pass 1 catalog:
    /// - All `*_with_conn` functions and connection-passing functions/methods.
    /// - All dual-backend enums (having both `Sqlite` and `Redb` variants).
    /// - All structs whose fields hold a `DatabaseConnection`/`DbConn`/`ConnectionTrait`.
    #[derive(Default)]
    struct WholeTreeCatalog {
        /// Registered `*_with_conn` function/method names.
        with_conn_fn_names: BTreeSet<String>,
        /// Registered non-`*_with_conn` connection-passing methods `(TypeName, method_name)`.
        conn_passing_methods: BTreeSet<(String, String)>,
        /// Registered non-`*_with_conn` connection-passing free functions:
        /// `fn_name -> set of (file_path, canonical_module_path)`.
        conn_passing_free_fns: BTreeMap<String, BTreeSet<(String, String)>>,
        /// Enums declaring BOTH a `Sqlite` and a `Redb` variant (e.g. `WorktreeDb`, `IntentRepoBackend`).
        dual_backend_enums: BTreeSet<String>,
        /// Structs whose fields hold a SeaORM/SQLite connection (so `&self` methods on them are connection-passing).
        conn_holding_structs: BTreeSet<String>,
    }

    struct Pass1TypeCollector<'a> {
        catalog: &'a mut WholeTreeCatalog,
    }

    impl<'ast> Visit<'ast> for Pass1TypeCollector<'_> {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            if has_cfg_test(syn_item_attrs(item)) {
                return;
            }
            syn::visit::visit_item(self, item);
        }

        fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
            let mut has_sqlite = false;
            let mut has_redb = false;
            for v in &item.variants {
                if has_cfg_test(&v.attrs) {
                    continue;
                }
                if v.ident == "Sqlite" {
                    has_sqlite = true;
                } else if v.ident == "Redb" {
                    has_redb = true;
                }
            }
            if has_sqlite && has_redb {
                self.catalog
                    .dual_backend_enums
                    .insert(item.ident.to_string());
            }
            syn::visit::visit_item_enum(self, item);
        }

        fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
            let conn_generics = collect_conn_trait_generics(&item.generics);
            let holds_conn = !conn_generics.is_empty()
                || item.fields.iter().any(|f| {
                    !has_cfg_test(&f.attrs)
                        && field_mandatorily_holds_conn(
                            &f.ty,
                            &conn_generics,
                            &self.catalog.conn_holding_structs,
                        )
                });
            if holds_conn {
                self.catalog
                    .conn_holding_structs
                    .insert(item.ident.to_string());
            }
            syn::visit::visit_item_struct(self, item);
        }
    }

    struct Pass1FnCollector<'a> {
        file_path: &'a str,
        catalog: &'a mut WholeTreeCatalog,
        current_impl_type: Option<String>,
        current_impl_conn_generics: BTreeSet<String>,
    }

    impl<'ast> Visit<'ast> for Pass1FnCollector<'_> {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            if has_cfg_test(syn_item_attrs(item)) {
                return;
            }
            syn::visit::visit_item(self, item);
        }

        fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
            let prev_type = self.current_impl_type.take();
            let prev_generics = std::mem::take(&mut self.current_impl_conn_generics);
            self.current_impl_type = self_type_name(&item.self_ty);
            self.current_impl_conn_generics = collect_conn_trait_generics(&item.generics);
            syn::visit::visit_item_impl(self, item);
            self.current_impl_type = prev_type;
            self.current_impl_conn_generics = prev_generics;
        }

        fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
            let name = item.sig.ident.to_string();
            let empty = BTreeSet::new();
            if name.ends_with("_with_conn") {
                self.catalog.with_conn_fn_names.insert(name);
            } else if signature_accepts_conn(
                &item.sig,
                &empty,
                false,
                &self.catalog.conn_holding_structs,
            ) {
                let mod_path = canonical_module_path_from_file(self.file_path);
                self.catalog
                    .conn_passing_free_fns
                    .entry(name)
                    .or_default()
                    .insert((self.file_path.to_string(), mod_path));
            }
            syn::visit::visit_item_fn(self, item);
        }

        fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
            if has_cfg_test(syn_impl_item_attrs(item)) {
                return;
            }
            syn::visit::visit_impl_item(self, item);
        }

        fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
            let name = item.sig.ident.to_string();
            let holds_conn = self
                .current_impl_type
                .as_deref()
                .is_some_and(|t| self.catalog.conn_holding_structs.contains(t));
            if name.ends_with("_with_conn") {
                self.catalog.with_conn_fn_names.insert(name);
            } else if signature_accepts_conn(
                &item.sig,
                &self.current_impl_conn_generics,
                holds_conn,
                &self.catalog.conn_holding_structs,
            ) && let Some(ty) = &self.current_impl_type
            {
                self.catalog.conn_passing_methods.insert((ty.clone(), name));
            }
            syn::visit::visit_impl_item_fn(self, item);
        }

        fn visit_trait_item(&mut self, item: &'ast syn::TraitItem) {
            if has_cfg_test(syn_trait_item_attrs(item)) {
                return;
            }
            syn::visit::visit_trait_item(self, item);
        }

        fn visit_trait_item_fn(&mut self, item: &'ast syn::TraitItemFn) {
            let name = item.sig.ident.to_string();
            if name.ends_with("_with_conn") {
                self.catalog.with_conn_fn_names.insert(name);
            }
            syn::visit::visit_trait_item_fn(self, item);
        }
    }

    fn pat_matches_variant(
        pat: &syn::Pat,
        enum_name: Option<&str>,
        variant: &str,
        allow_bare_variant: bool,
    ) -> bool {
        let path_matches = |path: &syn::Path| -> bool {
            let segs = path_segments(path);
            match segs.as_slice() {
                [.., ty, var] if var == variant => enum_name.is_none_or(|expected| ty == expected),
                [var] if allow_bare_variant && var == variant => true,
                _ => false,
            }
        };
        match pat {
            syn::Pat::Path(p) => path_matches(&p.path),
            syn::Pat::TupleStruct(ts) => {
                path_matches(&ts.path)
                    || ts
                        .elems
                        .iter()
                        .any(|e| pat_matches_variant(e, enum_name, variant, allow_bare_variant))
            }
            syn::Pat::Struct(ps) => {
                path_matches(&ps.path)
                    || ps.fields.iter().any(|f| {
                        pat_matches_variant(&f.pat, enum_name, variant, allow_bare_variant)
                    })
            }
            syn::Pat::Or(or) => or
                .cases
                .iter()
                .any(|c| pat_matches_variant(c, enum_name, variant, allow_bare_variant)),
            syn::Pat::Tuple(t) => t
                .elems
                .iter()
                .any(|e| pat_matches_variant(e, enum_name, variant, allow_bare_variant)),
            syn::Pat::Reference(r) => {
                pat_matches_variant(&r.pat, enum_name, variant, allow_bare_variant)
            }
            syn::Pat::Paren(p) => {
                pat_matches_variant(&p.pat, enum_name, variant, allow_bare_variant)
            }
            syn::Pat::Ident(id) => {
                (allow_bare_variant && id.ident == variant && id.subpat.is_none())
                    || id.subpat.as_ref().is_some_and(|(_, sub)| {
                        pat_matches_variant(sub, enum_name, variant, allow_bare_variant)
                    })
            }
            syn::Pat::Slice(s) => s
                .elems
                .iter()
                .any(|e| pat_matches_variant(e, enum_name, variant, allow_bare_variant)),
            _ => false,
        }
    }

    fn expr_tests_storage_backend_variant(
        expr: &syn::Expr,
        variant: &str,
        allow_bare_variant: bool,
    ) -> bool {
        let is_backend_path = |e: &syn::Expr| -> bool {
            if let syn::Expr::Path(ep) = e {
                let segs = path_segments(&ep.path);
                matches!(segs.as_slice(), [.., ty, var] if ty == "StorageBackendKind" && var == variant)
                    || (allow_bare_variant && matches!(segs.as_slice(), [var] if var == variant))
            } else {
                false
            }
        };
        match expr {
            syn::Expr::Binary(bin) if matches!(bin.op, syn::BinOp::Eq(_)) => {
                is_backend_path(&bin.left) || is_backend_path(&bin.right)
            }
            syn::Expr::Let(expr_let) => pat_matches_variant(
                &expr_let.pat,
                Some("StorageBackendKind"),
                variant,
                allow_bare_variant,
            ),
            syn::Expr::Macro(em) if em.mac.path.is_ident("matches") => {
                let s = em.mac.tokens.to_string();
                s.contains(&format!("StorageBackendKind :: {variant}"))
            }
            syn::Expr::Paren(p) => {
                expr_tests_storage_backend_variant(&p.expr, variant, allow_bare_variant)
            }
            syn::Expr::Group(g) => {
                expr_tests_storage_backend_variant(&g.expr, variant, allow_bare_variant)
            }
            _ => false,
        }
    }

    fn is_direct_sqlite_opener_path(
        raw_path: &str,
        expanded_path: &str,
        has_rusqlite_import: bool,
    ) -> Option<&'static str> {
        let check_one = |p: &str| -> Option<&'static str> {
            let segs: Vec<&str> = p.split("::").collect();
            let last = *segs.last()?;
            if DIRECT_OPENER_FNS.contains(&last) {
                return Some("direct-sqlite-opener");
            }
            if CAPTURE_STORE_OPENERS.contains(&last) {
                let len = segs.len();
                if (len >= 3 && segs[len - 3] == "capture" && segs[len - 2] == "store")
                    || p.contains("capture::store::")
                {
                    return Some("direct-capture-sqlite-opener");
                }
            }
            if segs.len() >= 2 {
                let prev = segs[segs.len() - 2];
                if prev == "Database" && last == "connect" {
                    return Some("direct-seaorm-database-connect");
                }
                if prev == "SqlitePool" && last.starts_with("connect") {
                    return Some("direct-sqlx-sqlite-connect");
                }
                if prev == "SqliteConnection" && last.starts_with("connect") {
                    return Some("direct-sqlx-sqlite-connect");
                }
                if SQLITE_STORE_TYPES.contains(&prev)
                    && (last.starts_with("open") || last == "create")
                {
                    return Some("direct-sqlite-store-open");
                }
                if prev == "Connection"
                    && last.starts_with("open")
                    && (p.contains("rusqlite") || has_rusqlite_import)
                {
                    return Some("direct-rusqlite-open");
                }
            }
            None
        };
        check_one(expanded_path).or_else(|| check_one(raw_path))
    }

    struct Pass2Auditor<'a> {
        file_path: &'a str,
        catalog: &'a WholeTreeCatalog,
        aliases: HashMap<String, String>,
        has_storage_backend_glob: bool,
        has_rusqlite_import: bool,
        current_impl_type: Option<String>,
        current_impl_conn_generics: BTreeSet<String>,
        fn_stack: Vec<FnContext>,
        storage_backend_sqlite_depth: usize,
        paired_enum_sqlite_depth: usize,
        violations: Vec<AuditViolation>,
    }

    #[derive(Clone)]
    struct FnContext {
        name: String,
        is_conn_passing: bool,
    }

    impl<'a> Pass2Auditor<'a> {
        fn current_fn_name(&self) -> String {
            self.fn_stack
                .last()
                .map(|c| c.name.clone())
                .unwrap_or_else(|| "<module>".to_string())
        }

        fn current_fn_is_conn_passing(&self) -> bool {
            self.fn_stack.last().is_some_and(|c| c.is_conn_passing)
        }

        fn record_violation(&mut self, line: usize, kind: &'static str, detail: String) {
            self.violations.push(AuditViolation {
                path: self.file_path.to_string(),
                line,
                function: self.current_fn_name(),
                kind,
                detail,
            });
        }

        fn check_call_path(&mut self, raw_path: &str, line: usize) {
            let expanded = expand_path_with_aliases(raw_path, &self.aliases);

            // 1. Direct SQLite/SeaORM openers ALWAYS require lexical nesting in a paired
            //    StorageBackendKind::Sqlite / StorageBackendKind::Redb dispatch.
            if let Some(opener_kind) =
                is_direct_sqlite_opener_path(raw_path, &expanded, self.has_rusqlite_import)
            {
                if self.storage_backend_sqlite_depth == 0 {
                    let kind = if self.current_fn_is_conn_passing() {
                        "with-conn-opens-db"
                    } else {
                        opener_kind
                    };
                    self.record_violation(
                        line,
                        kind,
                        format!(
                            "calls direct SQLite opener `{raw_path}` (resolved `{expanded}`) \
                             without lexical nesting inside a paired StorageBackendKind::Sqlite/Redb dispatch"
                        ),
                    );
                }
                return;
            }

            // 2. Calls to registered `*_with_conn` or connection-passing functions/methods.
            let segs: Vec<&str> = expanded.split("::").collect();
            let raw_segs: Vec<&str> = raw_path.split("::").collect();
            let last = segs.last().copied().unwrap_or("");
            let raw_last = raw_segs.last().copied().unwrap_or("");

            let is_with_conn_call = last.ends_with("_with_conn")
                || raw_last.ends_with("_with_conn")
                || self.catalog.with_conn_fn_names.contains(last)
                || CONN_DERIVED_PEER_FNS.contains(&last)
                || CONN_DERIVED_PEER_FNS.contains(&raw_last);

            let is_conn_passing_method_call = segs.len() >= 2 && {
                let ty = segs[segs.len() - 2];
                let effective_ty = if ty == "Self" {
                    self.current_impl_type.as_deref().unwrap_or(ty)
                } else {
                    ty
                };
                self.catalog
                    .conn_passing_methods
                    .contains(&(effective_ty.to_string(), last.to_string()))
            };

            let is_conn_passing_free_fn_call =
                if let Some(defs) = self.catalog.conn_passing_free_fns.get(last) {
                    let is_same_file_unqualified = !raw_path.contains("::")
                        && !self.aliases.contains_key(raw_path)
                        && defs.iter().any(|(def_file, _)| def_file == self.file_path);
                    let matches_canonical_mod = defs.iter().any(|(_, mod_path)| {
                        let full = format!("{mod_path}::{last}");
                        if expanded == full {
                            return true;
                        }
                        // Also match module-relative paths like `super::foo::fn_name` or `foo::fn_name`
                        // when the immediate parent module matches the defining module's last segment.
                        let mod_last = mod_path.rsplit("::").next().unwrap_or("");
                        segs.len() >= 2
                            && segs[segs.len() - 2] == mod_last
                            && !mod_last.is_empty()
                            && mod_last != "crate"
                    });
                    is_same_file_unqualified || matches_canonical_mod
                } else {
                    false
                };

            if is_with_conn_call || is_conn_passing_method_call || is_conn_passing_free_fn_call {
                let allowed = self.current_fn_is_conn_passing()
                    || self.storage_backend_sqlite_depth > 0
                    || self.paired_enum_sqlite_depth > 0;
                if !allowed {
                    self.record_violation(
                        line,
                        "unwired-with-conn-caller",
                        format!(
                            "calls connection-passing `{raw_path}` outside a connection-passing \
                             function or paired StorageBackendKind::Sqlite/Redb dispatch"
                        ),
                    );
                }
            }
        }

        fn check_method_call(&mut self, method_name: &str, line: usize) {
            if method_name.ends_with("_with_conn")
                || self.catalog.with_conn_fn_names.contains(method_name)
            {
                let allowed = self.current_fn_is_conn_passing()
                    || self.storage_backend_sqlite_depth > 0
                    || self.paired_enum_sqlite_depth > 0;
                if !allowed {
                    self.record_violation(
                        line,
                        "unwired-with-conn-caller",
                        format!(
                            "calls connection-passing method `.{method_name}(...)` outside a \
                             connection-passing function or paired StorageBackendKind::Sqlite/Redb dispatch"
                        ),
                    );
                }
                return;
            }
            if DIRECT_SQLITE_QUERY_METHODS.contains(&method_name) {
                let allowed = self.current_fn_is_conn_passing()
                    || self.storage_backend_sqlite_depth > 0
                    || self.paired_enum_sqlite_depth > 0;
                if !allowed {
                    self.record_violation(
                        line,
                        "direct-sqlite-query",
                        format!(
                            "calls SeaORM raw SQL method `.{method_name}(...)` outside a \
                             connection-passing function or paired StorageBackendKind::Sqlite/Redb dispatch"
                        ),
                    );
                }
            }
        }

        fn scan_macro_tokens(&mut self, tokens: proc_macro2::TokenStream) {
            let trees: Vec<proc_macro2::TokenTree> = tokens.into_iter().collect();
            let mut idx = 0;
            while idx < trees.len() {
                match &trees[idx] {
                    proc_macro2::TokenTree::Group(group) => {
                        self.scan_macro_tokens(group.stream());
                        idx += 1;
                    }
                    proc_macro2::TokenTree::Ident(ident) => {
                        let start_line = ident.span().start().line;
                        let mut segs = vec![ident.to_string()];
                        let mut cursor = idx + 1;
                        while cursor + 2 < trees.len() {
                            if let (
                                proc_macro2::TokenTree::Punct(p1),
                                proc_macro2::TokenTree::Punct(p2),
                                proc_macro2::TokenTree::Ident(next_id),
                            ) = (&trees[cursor], &trees[cursor + 1], &trees[cursor + 2])
                                && p1.as_char() == ':'
                                && p2.as_char() == ':'
                            {
                                segs.push(next_id.to_string());
                                cursor += 3;
                                continue;
                            }
                            break;
                        }
                        let is_call = matches!(
                            trees.get(cursor),
                            Some(proc_macro2::TokenTree::Group(g))
                                if g.delimiter() == proc_macro2::Delimiter::Parenthesis
                        );
                        if is_call {
                            let full = segs.join("::");
                            self.check_call_path(&full, start_line);
                        }
                        idx = cursor;
                    }
                    _ => {
                        idx += 1;
                    }
                }
            }
        }

        /// Check if a `match` expression pairs `StorageBackendKind::Sqlite` with
        /// `StorageBackendKind::Redb`, or pairs a known dual-backend enum's `Sqlite` + `Redb` arms.
        fn visit_paired_match(&mut self, expr: &syn::ExprMatch) {
            self.visit_expr(&expr.expr);

            let active_arms: Vec<&syn::Arm> = expr
                .arms
                .iter()
                .filter(|arm| !has_cfg_test(&arm.attrs))
                .collect();

            let has_sbk_sqlite = active_arms.iter().any(|arm| {
                pat_matches_variant(
                    &arm.pat,
                    Some("StorageBackendKind"),
                    "Sqlite",
                    self.has_storage_backend_glob,
                )
            });
            let has_sbk_redb = active_arms.iter().any(|arm| {
                pat_matches_variant(
                    &arm.pat,
                    Some("StorageBackendKind"),
                    "Redb",
                    self.has_storage_backend_glob,
                )
            });
            let is_paired_sbk = has_sbk_sqlite && has_sbk_redb;

            // Check for paired dual-backend enums (e.g. WorktreeDb::{Sqlite, Redb} or Self::{Sqlite, Redb}).
            let mut matched_dual_enum: Option<String> = None;
            if !is_paired_sbk {
                let mut candidates: Vec<String> =
                    self.catalog.dual_backend_enums.iter().cloned().collect();
                if self
                    .current_impl_type
                    .as_deref()
                    .is_some_and(|t| self.catalog.dual_backend_enums.contains(t))
                {
                    candidates.push("Self".to_string());
                }
                for enum_name in candidates {
                    let has_sq = active_arms.iter().any(|arm| {
                        pat_matches_variant(&arm.pat, Some(&enum_name), "Sqlite", false)
                    });
                    let has_rb = active_arms
                        .iter()
                        .any(|arm| pat_matches_variant(&arm.pat, Some(&enum_name), "Redb", false));
                    if has_sq && has_rb {
                        matched_dual_enum = Some(enum_name);
                        break;
                    }
                }
            }

            for arm in active_arms {
                let arm_is_sbk_sqlite = is_paired_sbk
                    && pat_matches_variant(
                        &arm.pat,
                        Some("StorageBackendKind"),
                        "Sqlite",
                        self.has_storage_backend_glob,
                    )
                    && !pat_matches_variant(
                        &arm.pat,
                        Some("StorageBackendKind"),
                        "Redb",
                        self.has_storage_backend_glob,
                    );
                let arm_is_dual_enum_sqlite = matched_dual_enum.as_deref().is_some_and(|en| {
                    pat_matches_variant(&arm.pat, Some(en), "Sqlite", false)
                        && !pat_matches_variant(&arm.pat, Some(en), "Redb", false)
                });

                if arm_is_sbk_sqlite {
                    self.storage_backend_sqlite_depth += 1;
                }
                if arm_is_dual_enum_sqlite {
                    self.paired_enum_sqlite_depth += 1;
                }
                syn::visit::visit_arm(self, arm);
                if arm_is_dual_enum_sqlite {
                    self.paired_enum_sqlite_depth -= 1;
                }
                if arm_is_sbk_sqlite {
                    self.storage_backend_sqlite_depth -= 1;
                }
            }
        }

        /// Check an `if / else if / else` chain for explicit paired `StorageBackendKind::Sqlite`
        /// and `StorageBackendKind::Redb` branches.
        fn visit_paired_if(&mut self, expr: &syn::ExprIf) {
            let mut branches: Vec<(Option<&syn::Expr>, &syn::Block)> = Vec::new();
            let mut tail_else_expr: Option<&syn::Expr> = None;
            let mut cursor: &syn::ExprIf = expr;
            loop {
                branches.push((Some(&cursor.cond), &cursor.then_branch));
                match &cursor.else_branch {
                    Some((_, else_expr)) => match else_expr.as_ref() {
                        syn::Expr::If(next_if) => {
                            cursor = next_if;
                        }
                        syn::Expr::Block(else_block) => {
                            branches.push((None, &else_block.block));
                            break;
                        }
                        other => {
                            tail_else_expr = Some(other);
                            break;
                        }
                    },
                    None => break,
                }
            }

            let has_explicit_sqlite = branches.iter().any(|(cond, _)| {
                cond.is_some_and(|c| {
                    expr_tests_storage_backend_variant(c, "Sqlite", self.has_storage_backend_glob)
                })
            });
            let has_explicit_redb = branches.iter().any(|(cond, _)| {
                cond.is_some_and(|c| {
                    expr_tests_storage_backend_variant(c, "Redb", self.has_storage_backend_glob)
                })
            });
            let is_paired_if = has_explicit_sqlite && has_explicit_redb;

            for (cond, block) in branches {
                if let Some(c) = cond {
                    self.visit_expr(c);
                }
                let is_sqlite_branch = is_paired_if
                    && cond.is_some_and(|c| {
                        expr_tests_storage_backend_variant(
                            c,
                            "Sqlite",
                            self.has_storage_backend_glob,
                        )
                    });
                if is_sqlite_branch {
                    self.storage_backend_sqlite_depth += 1;
                }
                self.visit_block(block);
                if is_sqlite_branch {
                    self.storage_backend_sqlite_depth -= 1;
                }
            }
            if let Some(other) = tail_else_expr {
                self.visit_expr(other);
            }
        }
    }

    impl<'ast> Visit<'ast> for Pass2Auditor<'_> {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            if has_cfg_test(syn_item_attrs(item)) {
                return;
            }
            syn::visit::visit_item(self, item);
        }

        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            collect_use_tree(
                &item.tree,
                "",
                &mut self.aliases,
                &mut self.has_storage_backend_glob,
                &mut self.has_rusqlite_import,
            );
            syn::visit::visit_item_use(self, item);
        }

        fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
            let prev_type = self.current_impl_type.take();
            let prev_generics = std::mem::take(&mut self.current_impl_conn_generics);
            self.current_impl_type = self_type_name(&item.self_ty);
            self.current_impl_conn_generics = collect_conn_trait_generics(&item.generics);
            syn::visit::visit_item_impl(self, item);
            self.current_impl_type = prev_type;
            self.current_impl_conn_generics = prev_generics;
        }

        fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
            let empty = BTreeSet::new();
            let is_conn_passing = signature_accepts_conn(
                &item.sig,
                &empty,
                false,
                &self.catalog.conn_holding_structs,
            );
            let saved_aliases = self.aliases.clone();
            let saved_glob = self.has_storage_backend_glob;
            let saved_rusqlite = self.has_rusqlite_import;
            self.fn_stack.push(FnContext {
                name: item.sig.ident.to_string(),
                is_conn_passing,
            });
            syn::visit::visit_item_fn(self, item);
            self.fn_stack.pop();
            self.aliases = saved_aliases;
            self.has_storage_backend_glob = saved_glob;
            self.has_rusqlite_import = saved_rusqlite;
        }

        fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
            if has_cfg_test(syn_impl_item_attrs(item)) {
                return;
            }
            syn::visit::visit_impl_item(self, item);
        }

        fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
            let holds_conn = self
                .current_impl_type
                .as_deref()
                .is_some_and(|t| self.catalog.conn_holding_structs.contains(t));
            let is_conn_passing = signature_accepts_conn(
                &item.sig,
                &self.current_impl_conn_generics,
                holds_conn,
                &self.catalog.conn_holding_structs,
            );
            let fn_name = match &self.current_impl_type {
                Some(ty) => format!("{ty}::{}", item.sig.ident),
                None => item.sig.ident.to_string(),
            };
            let saved_aliases = self.aliases.clone();
            let saved_glob = self.has_storage_backend_glob;
            let saved_rusqlite = self.has_rusqlite_import;
            self.fn_stack.push(FnContext {
                name: fn_name,
                is_conn_passing,
            });
            syn::visit::visit_impl_item_fn(self, item);
            self.fn_stack.pop();
            self.aliases = saved_aliases;
            self.has_storage_backend_glob = saved_glob;
            self.has_rusqlite_import = saved_rusqlite;
        }

        fn visit_trait_item(&mut self, item: &'ast syn::TraitItem) {
            if has_cfg_test(syn_trait_item_attrs(item)) {
                return;
            }
            syn::visit::visit_trait_item(self, item);
        }

        fn visit_trait_item_fn(&mut self, item: &'ast syn::TraitItemFn) {
            let empty = BTreeSet::new();
            let is_conn_passing = signature_accepts_conn(
                &item.sig,
                &empty,
                false,
                &self.catalog.conn_holding_structs,
            );
            self.fn_stack.push(FnContext {
                name: item.sig.ident.to_string(),
                is_conn_passing,
            });
            syn::visit::visit_trait_item_fn(self, item);
            self.fn_stack.pop();
        }

        fn visit_block(&mut self, block: &'ast syn::Block) {
            let mut dominated_added = 0usize;
            for stmt in &block.stmts {
                if has_cfg_test(syn_stmt_attrs(stmt)) {
                    continue;
                }
                self.visit_stmt(stmt);
                if stmt_dominates_remainder_as_sqlite(stmt, self.has_storage_backend_glob) {
                    self.storage_backend_sqlite_depth += 1;
                    dominated_added += 1;
                }
            }
            self.storage_backend_sqlite_depth -= dominated_added;
        }

        fn visit_stmt(&mut self, stmt: &'ast syn::Stmt) {
            if has_cfg_test(syn_stmt_attrs(stmt)) {
                return;
            }
            syn::visit::visit_stmt(self, stmt);
        }

        fn visit_expr(&mut self, expr: &'ast syn::Expr) {
            if has_cfg_test(syn_expr_attrs(expr)) {
                return;
            }
            match expr {
                syn::Expr::Match(m) => self.visit_paired_match(m),
                syn::Expr::If(i) => self.visit_paired_if(i),
                _ => syn::visit::visit_expr(self, expr),
            }
        }

        fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
            if let syn::Expr::Path(ep) = call.func.as_ref() {
                let raw = path_to_string(&ep.path);
                let line = ep.span().start().line;
                self.check_call_path(&raw, line);
            }
            syn::visit::visit_expr_call(self, call);
        }

        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            let method = call.method.to_string();
            let line = call.method.span().start().line;
            self.check_method_call(&method, line);
            syn::visit::visit_expr_method_call(self, call);
        }

        fn visit_macro(&mut self, mac: &'ast syn::Macro) {
            self.scan_macro_tokens(mac.tokens.clone());
            syn::visit::visit_macro(self, mac);
        }
    }

    pub(super) fn audit_sources(
        units: &[SourceUnit],
    ) -> Result<Vec<AuditViolation>, AuditParseError> {
        let mut parsed = Vec::with_capacity(units.len());
        for unit in units {
            let file = syn::parse_file(&unit.source).map_err(|err| AuditParseError {
                path: unit.path.clone(),
                line: err.span().start().line,
                message: err.to_string(),
            })?;
            parsed.push((unit.path.clone(), file));
        }

        // Exclude files that are ONLY included via `#[cfg(test)] mod ...;` in the parsed tree.
        let cfg_test_files = collect_cfg_test_out_of_line_modules(&parsed);

        // Pass 1a: Collect dual-backend enums and mandatory-connection struct bundles (to fixed point).
        let mut catalog = WholeTreeCatalog::default();
        loop {
            let before = catalog.conn_holding_structs.len() + catalog.dual_backend_enums.len();
            for (path, file) in &parsed {
                if cfg_test_files.contains(path) {
                    continue;
                }
                let mut collector = Pass1TypeCollector {
                    catalog: &mut catalog,
                };
                collector.visit_file(file);
            }
            let after = catalog.conn_holding_structs.len() + catalog.dual_backend_enums.len();
            if after == before {
                break;
            }
        }

        // Pass 1b: Build whole-tree `*_with_conn` and connection-passing function catalog.
        for (path, file) in &parsed {
            if cfg_test_files.contains(path) {
                continue;
            }
            let mut collector = Pass1FnCollector {
                file_path: path,
                catalog: &mut catalog,
                current_impl_type: None,
                current_impl_conn_generics: BTreeSet::new(),
            };
            collector.visit_file(file);
        }

        // Pass 2: Audit every callsite and caller reachability across all non-test files.
        let mut violations = Vec::new();
        for (path, file) in &parsed {
            if cfg_test_files.contains(path) {
                continue;
            }
            let mut auditor = Pass2Auditor {
                file_path: path,
                catalog: &catalog,
                aliases: HashMap::new(),
                has_storage_backend_glob: false,
                has_rusqlite_import: false,
                current_impl_type: None,
                current_impl_conn_generics: BTreeSet::new(),
                fn_stack: Vec::new(),
                storage_backend_sqlite_depth: 0,
                paired_enum_sqlite_depth: 0,
                violations: Vec::new(),
            };
            auditor.visit_file(file);
            violations.extend(auditor.violations);
        }

        violations.sort();
        Ok(violations)
    }

    pub(super) fn audit_workspace_src(
        repo_root: &Path,
    ) -> Result<Vec<AuditViolation>, AuditParseError> {
        let units = collect_src_files(repo_root);
        assert!(
            !units.is_empty(),
            "RB-29b AST audit must discover production Rust files under src/"
        );
        audit_sources(&units)
    }

    pub(super) fn rejects_partially_wired_caller_impl() {
        let source = r##"
use crate::storage::{GitStore, StorageBackendKind};
use crate::internal::db::get_db_conn_instance;

pub async fn wired_caller(backend: StorageBackendKind) {
    match backend {
        StorageBackendKind::Redb => {
            let _marker = "redb";
        }
        StorageBackendKind::Sqlite => {
            let _db = get_db_conn_instance().await;
        }
    }
}

pub fn fake_cfg_test_in_string() -> &'static str {
    "#[cfg(test)] mod tests {"
}

pub async fn unwired_helper() {
    let _db = get_db_conn_instance().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn ok_in_cfg_test() {
        let _db = get_db_conn_instance().await;
    }
}
"##;
        let units = vec![SourceUnit {
            path: "src/command/synthetic_partial.rs".to_string(),
            source: source.to_string(),
        }];
        let violations = audit_sources(&units).expect("valid Rust fixture must parse");
        let rendered = violations
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !violations.is_empty(),
            "partially wired file must be rejected by syn AST guard"
        );
        assert!(
            violations.iter().any(|v| v.function == "unwired_helper"),
            "expected violation naming exact function `unwired_helper`, got:\n{rendered}"
        );
        assert!(
            !violations.iter().any(|v| v.function == "wired_caller"),
            "properly wired `wired_caller` must NOT be flagged:\n{rendered}"
        );
        assert!(
            !violations.iter().any(|v| v.function == "ok_in_cfg_test"),
            "exact `#[cfg(test)]` module must NOT be flagged:\n{rendered}"
        );
    }

    pub(super) fn rejects_cfg_sqlite_without_runtime_dispatch_impl() {
        let source = r#"
use crate::internal::db::{get_db_conn_instance, open_database_without_migrations};
use crate::storage::StorageBackendKind;

#[cfg(feature = "storage-sqlite")]
pub async fn cfg_only_opener() {
    let _db = get_db_conn_instance().await;
}

pub async fn inline_cfg_sqlite_opener() {
    #[cfg(feature = "storage-sqlite")]
    {
        let _db = open_database_without_migrations("repo.db").await;
    }
}

pub async fn unpaired_sqlite_match_arm(backend: StorageBackendKind) {
    match backend {
        StorageBackendKind::Sqlite => {
            let _db = get_db_conn_instance().await;
        }
        _ => {}
    }
}
"#;
        let units = vec![SourceUnit {
            path: "src/command/synthetic_cfg_only.rs".to_string(),
            source: source.to_string(),
        }];
        let violations = audit_sources(&units).expect("valid Rust fixture must parse");
        let rendered = violations
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        for expected_fn in [
            "cfg_only_opener",
            "inline_cfg_sqlite_opener",
            "unpaired_sqlite_match_arm",
        ] {
            assert!(
                violations.iter().any(|v| v.function == expected_fn),
                "expected violation for `{expected_fn}`, got:\n{rendered}"
            );
        }
    }

    pub(super) fn rejects_unwired_caller_of_allowlisted_with_conn_impl() {
        let store_module = r#"
use sea_orm::DatabaseConnection;
use crate::storage::GitStore;
use crate::internal::db::get_db_conn_instance;

pub async fn foo_with_conn(_conn: &DatabaseConnection) -> Result<(), String> {
    Ok(())
}

pub async fn foo_with_store(_store: &dyn GitStore) -> Result<(), String> {
    Ok(())
}

pub async fn transitive_with_conn(conn: &DatabaseConnection) -> Result<(), String> {
    foo_with_conn(conn).await
}

pub async fn bad_self_opening_with_conn(_conn: &DatabaseConnection) -> Result<(), String> {
    let _leaked = get_db_conn_instance().await;
    Ok(())
}
"#;
        let cmd_module = r#"
use crate::storage::{GitStore, StorageBackendKind};

pub async fn wired_cmd_entry(
    backend: StorageBackendKind,
    conn: &sea_orm::DatabaseConnection,
    store: &dyn GitStore,
) -> Result<(), String> {
    match backend {
        StorageBackendKind::Redb => foo_with_store(store).await,
        StorageBackendKind::Sqlite => foo_with_conn(conn).await,
    }
}

pub async fn unwired_cmd_entry(handle: &OpaqueConn) -> Result<(), String> {
    foo_with_conn(handle).await
}
"#;
        let units = vec![
            SourceUnit {
                path: "src/internal/synthetic_store.rs".to_string(),
                source: store_module.to_string(),
            },
            SourceUnit {
                path: "src/command/synthetic_cmd.rs".to_string(),
                source: cmd_module.to_string(),
            },
        ];
        let violations = audit_sources(&units).expect("valid Rust fixture must parse");
        let rendered = violations
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            violations
                .iter()
                .any(|v| v.function == "bad_self_opening_with_conn"
                    && v.kind == "with-conn-opens-db"),
            "AC3: `*_with_conn` opening DB directly must be rejected:\n{rendered}"
        );
        assert!(
            violations
                .iter()
                .any(|v| v.function == "unwired_cmd_entry" && v.kind == "unwired-with-conn-caller"),
            "AC4/AC7: unwired caller of `foo_with_conn` must be rejected:\n{rendered}"
        );
        assert!(
            !violations
                .iter()
                .any(|v| v.function == "transitive_with_conn"),
            "transitive `*_with_conn` caller must NOT be flagged:\n{rendered}"
        );
        assert!(
            !violations.iter().any(|v| v.function == "wired_cmd_entry"),
            "wired `StorageBackendKind::Sqlite/Redb` caller must NOT be flagged:\n{rendered}"
        );
    }

    pub(super) fn fails_closed_on_malformed_rust_input_impl() {
        let syntax_error_unit = SourceUnit {
            path: "src/command/malformed_syntax.rs".to_string(),
            source: "pub fn broken( { let x = 1; }".to_string(),
        };
        let err1 = audit_sources(&[syntax_error_unit])
            .expect_err("malformed Rust syntax MUST fail closed");
        assert_eq!(err1.path, "src/command/malformed_syntax.rs");

        let unclosed_cfg_test_unit = SourceUnit {
            path: "src/command/unclosed_cfg_test.rs".to_string(),
            source: r#"
pub fn valid_prod_fn() {}

#[cfg(test)]
mod tests {
    fn x() {}
"#
            .to_string(),
        };
        let err2 = audit_sources(&[unclosed_cfg_test_unit])
            .expect_err("unclosed `#[cfg(test)] mod tests {` MUST fail closed");
        assert_eq!(err2.path, "src/command/unclosed_cfg_test.rs");
    }

    pub(super) fn per_callsite_storage_wiring_completeness_guard_impl() {
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let violations = audit_workspace_src(&repo_root)
            .unwrap_or_else(|err| panic!("RB-29b AST audit failed closed on parse error: {err}"));
        if !violations.is_empty() {
            let report = violations
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            panic!(
                "RB-29b syn 2.0 AST audit found {} unprotected SQLite / `*_with_conn` callsite(s) \
                 in production `src/`:\n{report}",
                violations.len()
            );
        }
    }
}

#[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]
#[test]
fn syn_guard_rejects_partially_wired_caller() {
    rb29b_ast_audit::rejects_partially_wired_caller_impl();
}

#[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]
#[test]
fn syn_guard_rejects_cfg_sqlite_without_runtime_dispatch() {
    rb29b_ast_audit::rejects_cfg_sqlite_without_runtime_dispatch_impl();
}

#[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]
#[test]
fn syn_guard_rejects_unwired_caller_of_allowlisted_with_conn() {
    rb29b_ast_audit::rejects_unwired_caller_of_allowlisted_with_conn_impl();
}

#[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]
#[test]
fn syn_guard_fails_closed_on_malformed_rust_input() {
    rb29b_ast_audit::fails_closed_on_malformed_rust_input_impl();
}

#[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]
#[test]
fn syn_ast_per_callsite_storage_wiring_completeness_guard() {
    rb29b_ast_audit::per_callsite_storage_wiring_completeness_guard_impl();
}

#[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]
mod packaged_release_audit {
    use std::{
        fs,
        path::{Path, PathBuf},
        process::Command,
    };

    use libra::{
        internal::db::schema::DatabaseRole,
        storage::{
            StorageBackendKind,
            config_store::{CONFIG_REDB_FILE_NAME, ConfigStore},
            manifest::{read_storage_manifest, resolve_redb_generation_paths},
            sqlite::SqliteConfigStore,
        },
    };
    use sha2::{Digest, Sha256};

    pub(super) fn sha256_file_hex(path: &Path) -> String {
        let bytes = fs::read(path)
            .unwrap_or_else(|err| panic!("failed to read '{}' for sha256: {err}", path.display()));
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        hex::encode(hasher.finalize())
    }

    pub(super) fn verify_staged_binary_with_sidecar(env_var: &str) -> PathBuf {
        let raw = std::env::var(env_var)
            .unwrap_or_else(|_| panic!("{env_var} must be set to the staged release binary path"));
        let bin = PathBuf::from(&raw);
        assert!(
            bin.is_file(),
            "{env_var} '{}' does not exist or is not a regular file",
            bin.display()
        );
        let sidecar = PathBuf::from(format!("{}.sha256", bin.display()));
        assert!(
            sidecar.is_file(),
            "expected SHA-256 sidecar '{}' beside {env_var}",
            sidecar.display()
        );
        let expected = fs::read_to_string(&sidecar)
            .unwrap_or_else(|err| panic!("failed to read '{}': {err}", sidecar.display()))
            .trim()
            .to_ascii_lowercase();
        assert_eq!(
            expected.len(),
            64,
            "sidecar '{}' must contain a 64-hex SHA-256 digest, got '{expected}'",
            sidecar.display()
        );
        let actual = sha256_file_hex(&bin);
        assert_eq!(
            actual,
            expected,
            "SHA-256 mismatch for staged {env_var} '{}'",
            bin.display()
        );
        bin
    }

    pub(super) fn run_cmd(
        bin: &Path,
        cwd: &Path,
        home: &Path,
        global_db: &Path,
        system_db: &Path,
        args: &[&str],
    ) -> String {
        let mut cmd = Command::new(bin);
        cmd.current_dir(cwd)
            .args(args)
            .env("HOME", home)
            .env("USERPROFILE", home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("LIBRA_CONFIG_GLOBAL_DB", global_db)
            .env("LIBRA_CONFIG_SYSTEM_DB", system_db)
            .env("LIBRA_TEST", "1")
            .env_remove("LIBRA_STORAGE_BACKEND")
            .env_remove("LIBRA_CONFIG_BACKEND");
        let out = cmd
            .output()
            .unwrap_or_else(|err| panic!("failed to execute {} {:?}: {err}", bin.display(), args));
        assert!(
            out.status.success(),
            "{} {:?} failed (status {:?})\nstdout:\n{}\nstderr:\n{}",
            bin.display(),
            args,
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    pub(super) fn pre_cutover_dual_backend_coherence_impl() {
        let libra_bin = verify_staged_binary_with_sidecar("LIBRA_PACKAGED_BIN");
        let migrate_bin = verify_staged_binary_with_sidecar("LIBRA_MIGRATE_PACKAGED_BIN");

        let tmp = tempfile::tempdir().expect("create RB-32b tempdir");
        let home = tmp.path().join("home");
        let global_dir = tmp.path().join("global-cfg");
        let system_dir = tmp.path().join("system-cfg");
        let repo_dir = tmp.path().join("repo");
        fs::create_dir_all(home.join(".config")).expect("mkdir home/.config");
        fs::create_dir_all(&global_dir).expect("mkdir global-cfg");
        fs::create_dir_all(&system_dir).expect("mkdir system-cfg");
        fs::create_dir_all(&repo_dir).expect("mkdir repo");

        let global_db = global_dir.join("config.db");
        let system_db = system_dir.join("config.db");

        // AC3 & AC4: Unset LIBRA_STORAGE_BACKEND -> `libra config set --global` creates
        // SQLite config.db and NEVER creates config.redb.
        run_cmd(
            &libra_bin,
            &repo_dir,
            &home,
            &global_db,
            &system_db,
            &["config", "set", "--global", "user.name", "PreCutoverUser"],
        );
        run_cmd(
            &libra_bin,
            &repo_dir,
            &home,
            &global_db,
            &system_db,
            &[
                "config",
                "set",
                "--global",
                "user.email",
                "precutover@example.com",
            ],
        );
        assert!(
            global_db.is_file(),
            "AC3: pre-cutover `libra config set --global` must create SQLite config.db at {}",
            global_db.display()
        );
        assert!(
            !global_dir.join(CONFIG_REDB_FILE_NAME).exists(),
            "AC4: pre-cutover `libra config set --global` must NOT create config.redb"
        );
        assert_eq!(
            run_cmd(
                &libra_bin,
                &repo_dir,
                &home,
                &global_db,
                &system_db,
                &["config", "get", "--global", "user.name"],
            )
            .trim(),
            "PreCutoverUser"
        );
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build runtime");
        rt.block_on(async {
            let sqlite_cfg =
                SqliteConfigStore::open_read_only(&global_db, DatabaseRole::GlobalConfig)
                    .await
                    .expect("open SQLite global config.db");
            assert_eq!(
                sqlite_cfg
                    .get("user.name")
                    .await
                    .expect("read user.name from SQLite config.db")
                    .map(|e| e.value),
                Some("PreCutoverUser".to_string())
            );
        });

        // AC2: Unset LIBRA_STORAGE_BACKEND -> `libra init` creates SQLite `.libra/libra.db`.
        run_cmd(
            &libra_bin,
            &repo_dir,
            &home,
            &global_db,
            &system_db,
            &["init", "--vault", "false"],
        );
        let dot_libra = repo_dir.join(".libra");
        let sqlite_git_path = dot_libra.join("libra.db");
        assert!(
            sqlite_git_path.is_file(),
            "AC2: pre-cutover `libra init` must default to SQLite libra.db"
        );
        assert!(
            !dot_libra.join("storage-format.json").exists(),
            "AC2: pre-cutover SQLite `libra init` must not write Redb storage-format.json"
        );
        assert!(
            !dot_libra.join("redb").exists(),
            "AC2: pre-cutover SQLite `libra init` must not create .libra/redb"
        );

        // AC1: Full bidirectional interop between staged `libra` (with keyring)
        // and staged `libra-migrate` (without keyring):
        // SQLite repo -> migrate --to redb -> libra read/write/commit -> migrate --to sqlite -> libra readback.
        fs::write(repo_dir.join("seed.txt"), "seeded-on-sqlite\n").expect("write seed.txt");
        run_cmd(
            &libra_bin,
            &repo_dir,
            &home,
            &global_db,
            &system_db,
            &["add", "seed.txt"],
        );
        run_cmd(
            &libra_bin,
            &repo_dir,
            &home,
            &global_db,
            &system_db,
            &["config", "rb32b.origin", "sqlite-before-cutover"],
        );
        run_cmd(
            &libra_bin,
            &repo_dir,
            &home,
            &global_db,
            &system_db,
            &["commit", "-m", "rb32b seed sqlite commit", "--no-verify"],
        );
        let seed_oid = run_cmd(
            &libra_bin,
            &repo_dir,
            &home,
            &global_db,
            &system_db,
            &["rev-parse", "HEAD"],
        )
        .trim()
        .to_string();

        let repo_arg = repo_dir.to_str().expect("utf-8 repo path");
        let fwd_out = run_cmd(
            &migrate_bin,
            &repo_dir,
            &home,
            &global_db,
            &system_db,
            &["--to", "redb", "--repo", repo_arg],
        );
        assert!(
            fwd_out.contains("Migrated repository to redb"),
            "unexpected forward migrate output: {fwd_out}"
        );
        let fwd_manifest = read_storage_manifest(&dot_libra)
            .expect("read manifest after --to redb")
            .expect("manifest must exist after --to redb");
        assert_eq!(fwd_manifest.backend, StorageBackendKind::Redb);
        assert!(fwd_manifest.finalized);
        let redb_paths = resolve_redb_generation_paths(&dot_libra)
            .expect("resolve redb paths")
            .expect("redb paths must exist");
        assert!(redb_paths.git_db_path.is_file());
        assert!(redb_paths.capture_db_path.is_file());

        // Read & write with packaged `libra` while repository is on Redb.
        assert_eq!(
            run_cmd(
                &libra_bin,
                &repo_dir,
                &home,
                &global_db,
                &system_db,
                &["config", "get", "rb32b.origin"],
            )
            .trim(),
            "sqlite-before-cutover"
        );
        assert_eq!(
            run_cmd(
                &libra_bin,
                &repo_dir,
                &home,
                &global_db,
                &system_db,
                &["rev-parse", "HEAD"],
            )
            .trim(),
            seed_oid
        );
        run_cmd(
            &libra_bin,
            &repo_dir,
            &home,
            &global_db,
            &system_db,
            &["config", "rb32b.redb_write", "written-on-redb"],
        );
        fs::write(repo_dir.join("redb_step.txt"), "written-on-redb\n")
            .expect("write redb_step.txt");
        run_cmd(
            &libra_bin,
            &repo_dir,
            &home,
            &global_db,
            &system_db,
            &["add", "redb_step.txt"],
        );
        run_cmd(
            &libra_bin,
            &repo_dir,
            &home,
            &global_db,
            &system_db,
            &["commit", "-m", "rb32b redb commit", "--no-verify"],
        );
        let redb_oid = run_cmd(
            &libra_bin,
            &repo_dir,
            &home,
            &global_db,
            &system_db,
            &["rev-parse", "HEAD"],
        )
        .trim()
        .to_string();
        assert_ne!(redb_oid, seed_oid);

        // Reverse migrate back to SQLite via staged `libra-migrate` (no keyring).
        let rev_out = run_cmd(
            &migrate_bin,
            &repo_dir,
            &home,
            &global_db,
            &system_db,
            &["--to", "sqlite", "--repo", repo_arg],
        );
        assert!(
            rev_out.contains("Migrated repository to sqlite"),
            "unexpected reverse migrate output: {rev_out}"
        );
        let rev_manifest = read_storage_manifest(&dot_libra)
            .expect("read manifest after --to sqlite")
            .expect("manifest must exist after --to sqlite");
        assert_eq!(rev_manifest.backend, StorageBackendKind::Sqlite);
        assert!(rev_manifest.finalized);
        assert!(sqlite_git_path.is_file());

        // Read 100% back via packaged `libra` on restored SQLite repo.
        assert_eq!(
            run_cmd(
                &libra_bin,
                &repo_dir,
                &home,
                &global_db,
                &system_db,
                &["rev-parse", "HEAD"],
            )
            .trim(),
            redb_oid
        );
        assert_eq!(
            run_cmd(
                &libra_bin,
                &repo_dir,
                &home,
                &global_db,
                &system_db,
                &["config", "get", "rb32b.origin"],
            )
            .trim(),
            "sqlite-before-cutover"
        );
        assert_eq!(
            run_cmd(
                &libra_bin,
                &repo_dir,
                &home,
                &global_db,
                &system_db,
                &["config", "get", "rb32b.redb_write"],
            )
            .trim(),
            "written-on-redb"
        );
        let commit_obj = run_cmd(
            &libra_bin,
            &repo_dir,
            &home,
            &global_db,
            &system_db,
            &["cat-file", "-p", "HEAD"],
        );
        assert!(
            commit_obj.contains("rb32b redb commit") && commit_obj.contains(&seed_oid),
            "restored SQLite HEAD commit must retain message and parent OID: {commit_obj}"
        );
    }
}

#[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]
#[test]
fn packaged_release_dual_backend_libra_and_migrate_coherence_pre_cutover() {
    packaged_release_audit::pre_cutover_dual_backend_coherence_impl();
}
