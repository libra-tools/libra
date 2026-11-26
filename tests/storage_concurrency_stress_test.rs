//! Multi-process concurrency stress tests, dual-feature migration roundtrip,
//! and pre-unlock `libra-migrate` binary gate integration tests (`RB-12`).

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
    use std::{fs, path::PathBuf, process::Command};

    use libra::{
        internal::db::schema::{DatabaseRole, create_database_for_role},
        storage::{
            AgentSessionRecord, CAPTURE_QUEUE_PAYLOAD_SCHEMA_VERSION, CaptureAgentKind,
            CaptureBatchPayload, CaptureQueueLane, CaptureScopeState, CaptureSessionState,
            CaptureStore as _, FRAME_FLAG_NONE, FRAME_FLAG_PRIORITY, GitStore as _,
            INITIAL_STORAGE_GENERATION, LIBRA_CONFIG_GLOBAL_DB_ENV, LIBRA_CONFIG_SYSTEM_DB_ENV,
            RedbCaptureStore, RedbGitStore, RedbRepairContext, RefCasOutcome, RefKey, RefTarget,
            SqliteCaptureStore, SqliteGitStore, StorageBackendKind, append_postcard_frame,
            config_format_manifest_path, config_redb_path_in_scope_dir, drain_capture_queue,
            execute_forward_migration, execute_reverse_migration, init_capture_queue,
            read_storage_manifest, resolve_redb_generation_paths, storage_manifest_path,
        },
    };
    use sea_orm::ConnectionTrait;
    use tempfile::tempdir;

    /// `RB-12` AC4 & AC6: SQLite -> redb forward migration, dual-lane capture queue
    /// + concurrent MVCC read/CAS writes, then redb -> SQLite reverse migration with
    /// 100% data integrity verification across both `libra.db` and `capture.db`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn dual_feature_migration_and_stress_roundtrip() {
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

    /// `RB-12` AC2: Real compiled `libra-migrate` binary invoked with `LIBRA_TEST=1`
    /// must unconditionally fail closed with `LBR-CLI-003` and zero disk writes prior
    /// to `RB-29a` unlock.
    #[test]
    fn pre_unlock_libra_migrate_binary_rejects_cutover_with_libra_test_env() {
        let tmp = tempdir().expect("create pre-unlock gate tempdir");
        let dot_libra = tmp.path().join(".libra");
        let global_dir = tmp.path().join("g");
        let system_dir = tmp.path().join("s");
        fs::create_dir_all(&dot_libra).expect("mkdir .libra");
        fs::create_dir_all(&global_dir).expect("mkdir g");
        fs::create_dir_all(&system_dir).expect("mkdir s");
        fs::write(dot_libra.join("libra.db"), b"").expect("touch libra.db");
        let global_db: PathBuf = global_dir.join("config.db");
        let system_db: PathBuf = system_dir.join("config.db");
        fs::write(&global_db, b"").expect("touch g/config.db");
        fs::write(&system_db, b"").expect("touch s/config.db");

        let bin = env!("CARGO_BIN_EXE_libra-migrate");
        let repo_arg = tmp.path().to_str().expect("utf8 repo path");
        let arg_cases: &[&[&str]] = &[
            &["--repo", repo_arg, "--to", "redb"],
            &["--repo", repo_arg, "--to", "sqlite"],
            &["--global", "--to", "redb"],
            &["--system", "--to", "redb"],
        ];

        for args in arg_cases {
            let output = Command::new(bin)
                .args(*args)
                .env("LIBRA_TEST", "1")
                .env(LIBRA_CONFIG_GLOBAL_DB_ENV, &global_db)
                .env(LIBRA_CONFIG_SYSTEM_DB_ENV, &system_db)
                .output()
                .unwrap_or_else(|err| panic!("spawn libra-migrate {args:?}: {err}"));
            assert!(
                !output.status.success(),
                "pre-unlock libra-migrate {args:?} must exit non-zero even with LIBRA_TEST=1"
            );
            let combined = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                combined.contains("LBR-CLI-003"),
                "pre-unlock libra-migrate {args:?} must emit LBR-CLI-003, got: {combined}"
            );
        }

        // Verify zero disk writes across all scopes.
        assert!(!storage_manifest_path(&dot_libra).exists());
        assert!(!config_redb_path_in_scope_dir(&global_dir).exists());
        assert!(!config_format_manifest_path(&global_dir).exists());
        assert!(!config_redb_path_in_scope_dir(&system_dir).exists());
        assert!(!config_format_manifest_path(&system_dir).exists());
    }
}
