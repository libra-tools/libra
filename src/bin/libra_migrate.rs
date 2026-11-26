//! Offline bidirectional storage migration CLI (`libra-migrate`).
//!
//! Supports repository (`--repo`), global configuration (`--global`), and system
//! configuration (`--system`) migrations between `sqlite` and `redb`, read-only
//! `--dry-run` inspection, re-entry idempotency, and compile-time pre-integration
//! cutover gating bound strictly to [`libra::storage::backend::REDB_RUNTIME_UNLOCKED`]
//! (`ADR-RB-01` #3, `ADR-RB-07`, `RB-11c`, `RB-11t`).

use std::{
    env,
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::{Parser, ValueEnum};
use libra::{
    internal::{config::global_config_resolution, db::schema::DatabaseRole},
    storage::{
        backend::{REDB_RUNTIME_UNLOCKED, StorageBackendKind},
        config_store::{
            LIBRA_CONFIG_GLOBAL_DB_ENV, LIBRA_CONFIG_SYSTEM_DB_ENV, config_scope_dir_from_db_path,
            read_config_format_manifest,
        },
        error::{StorageBackendError, StorageResult},
        manifest::{
            INITIAL_STORAGE_GENERATION, StorageAuthority, detect_storage_authority,
            read_storage_manifest,
        },
        migrate::{
            MigrationDryRunReport, dry_run_config_scope_migration, dry_run_repository_migration,
            execute_forward_migration, execute_reverse_migration, migrate_config_scope,
            resume_config_scope_migration, resume_forward_migration, resume_reverse_migration,
        },
    },
};

/// Target storage engine backend for `libra-migrate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum TargetBackendArg {
    Redb,
    Sqlite,
}

impl From<TargetBackendArg> for StorageBackendKind {
    fn from(value: TargetBackendArg) -> Self {
        match value {
            TargetBackendArg::Redb => Self::Redb,
            TargetBackendArg::Sqlite => Self::Sqlite,
        }
    }
}

/// Offline bidirectional SQLite <-> redb storage migration tool for Libra.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "libra-migrate",
    version,
    about = "Offline bidirectional storage migration tool for Libra repositories and config scopes"
)]
struct MigrateCliArgs {
    /// Target storage backend (`redb` or `sqlite`).
    #[arg(long = "to", value_enum)]
    to: TargetBackendArg,

    /// Repository working tree or `.libra` storage directory (defaults to discovering `.libra` from cwd).
    #[arg(long = "repo", value_name = "PATH", conflicts_with_all = ["global", "system"])]
    repo: Option<PathBuf>,

    /// Migrate the GlobalConfig scope (`LIBRA_CONFIG_GLOBAL_DB` or XDG config dir).
    #[arg(long = "global", conflicts_with_all = ["system", "repo"])]
    global: bool,

    /// Migrate the SystemConfig scope (`LIBRA_CONFIG_SYSTEM_DB` or `/etc/libra/config.db`).
    #[arg(long = "system", conflicts_with_all = ["global", "repo"])]
    system: bool,

    /// Strictly read-only preview: report per-table row counts and estimated size with zero disk writes.
    #[arg(long = "dry-run")]
    dry_run: bool,

    /// Force verification/re-entry even if the target backend is already finalized.
    #[arg(long = "force")]
    force: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MigrationScope {
    Repository(PathBuf),
    Config {
        role: DatabaseRole,
        scope_dir: PathBuf,
    },
}

fn check_live_cutover_gate(unlocked: bool) -> StorageResult<()> {
    if !unlocked {
        return Err(StorageBackendError::PreReleaseGateLocked {
            detail: "compile-time constant `REDB_RUNTIME_UNLOCKED` is `false`; live `libra-migrate` cutover is disabled until integration cutover (`RB-29a`). Use `--dry-run` for a read-only preview".to_string(),
        });
    }
    Ok(())
}

fn resolve_repository_storage_dir(explicit: Option<&Path>) -> StorageResult<PathBuf> {
    if let Some(path) = explicit {
        let dot_libra = path.join(".libra");
        if dot_libra.is_dir() {
            return Ok(dot_libra);
        }
        if path.file_name().is_some_and(|name| name == ".libra")
            || path.join("libra.db").exists()
            || path.join("storage-format.json").exists()
        {
            return Ok(path.to_path_buf());
        }
        return Ok(dot_libra);
    }

    let cwd = env::current_dir()
        .map_err(|err| StorageBackendError::io("resolve current working directory", err))?;
    let mut cursor = Some(cwd.as_path());
    while let Some(dir) = cursor {
        let candidate = dir.join(".libra");
        if candidate.is_dir() {
            return Ok(candidate);
        }
        cursor = dir.parent();
    }
    Err(StorageBackendError::NotFound {
        resource: "Libra repository (.libra directory)".to_string(),
    })
}

fn resolve_global_config_scope_dir() -> StorageResult<PathBuf> {
    if let Some(raw) = env::var_os(LIBRA_CONFIG_GLOBAL_DB_ENV)
        && !raw.is_empty()
    {
        return config_scope_dir_from_db_path(&PathBuf::from(raw));
    }
    let resolution = global_config_resolution().ok_or_else(|| StorageBackendError::NotFound {
        resource: "GlobalConfig directory (set LIBRA_CONFIG_GLOBAL_DB)".to_string(),
    })?;
    config_scope_dir_from_db_path(&resolution.path)
}

fn resolve_system_config_scope_dir() -> StorageResult<PathBuf> {
    if let Some(raw) = env::var_os(LIBRA_CONFIG_SYSTEM_DB_ENV)
        && !raw.is_empty()
    {
        return config_scope_dir_from_db_path(&PathBuf::from(raw));
    }
    config_scope_dir_from_db_path(Path::new("/etc/libra/config.db"))
}

fn resolve_migration_scope(args: &MigrateCliArgs) -> StorageResult<MigrationScope> {
    if args.global {
        Ok(MigrationScope::Config {
            role: DatabaseRole::GlobalConfig,
            scope_dir: resolve_global_config_scope_dir()?,
        })
    } else if args.system {
        Ok(MigrationScope::Config {
            role: DatabaseRole::SystemConfig,
            scope_dir: resolve_system_config_scope_dir()?,
        })
    } else {
        Ok(MigrationScope::Repository(resolve_repository_storage_dir(
            args.repo.as_deref(),
        )?))
    }
}

fn format_dry_run_report(report: &MigrationDryRunReport, out: &mut impl Write) -> io::Result<()> {
    writeln!(
        out,
        "dry-run: scope={} current={} target={} already_on_target={} pending={}",
        report.scope,
        report
            .current_backend
            .map_or("none", StorageBackendKind::as_str),
        report.target_backend.as_str(),
        report.already_on_target,
        report.manifest_pending,
    )?;
    if report.already_on_target {
        writeln!(
            out,
            "Target backend `{}` is already active; no migration required.",
            report.target_backend.as_str()
        )?;
    } else if report.manifest_pending {
        writeln!(
            out,
            "An unfinalized migration manifest is present; running without `--dry-run` will resume recovery."
        )?;
    }
    for stat in &report.tables {
        writeln!(
            out,
            "  table {}.{} rows={}",
            stat.database, stat.table, stat.row_count
        )?;
    }
    writeln!(
        out,
        "summary: tables={} total_rows={} source_bytes={} estimated_target_bytes={}",
        report.tables.len(),
        report.total_rows,
        report.source_bytes,
        report.estimated_target_bytes,
    )?;
    Ok(())
}

async fn run_cli_with_gate(
    args: MigrateCliArgs,
    unlocked: bool,
    stdout: &mut impl Write,
) -> StorageResult<()> {
    let target = StorageBackendKind::from(args.to);

    // Strictly enforce the compile-time lock gate BEFORE touching any path or
    // database when running a live cutover (non-dry-run). Zero env-var bypasses.
    if !args.dry_run {
        check_live_cutover_gate(unlocked)?;
    }

    let scope = resolve_migration_scope(&args)?;

    if args.dry_run {
        let report = match &scope {
            MigrationScope::Repository(common_storage) => {
                dry_run_repository_migration(common_storage, target).await?
            }
            MigrationScope::Config { role, scope_dir } => {
                dry_run_config_scope_migration(*role, scope_dir, target).await?
            }
        };
        format_dry_run_report(&report, stdout)
            .map_err(|err| StorageBackendError::io("write dry-run report to stdout", err))?;
        return Ok(());
    }

    match scope {
        MigrationScope::Repository(common_storage) => {
            if let Some(manifest) = read_storage_manifest(&common_storage)? {
                if !manifest.finalized {
                    if manifest.backend != target {
                        return Err(StorageBackendError::ManifestNotFinalized {
                            detail: format!(
                                "unfinalized migration targets `{}` but `--to {}` was requested; finish recovery with `--to {}` first",
                                manifest.backend.as_str(),
                                target.as_str(),
                                manifest.backend.as_str(),
                            ),
                        });
                    }
                    match target {
                        StorageBackendKind::Redb => {
                            let report = resume_forward_migration(&common_storage).await?;
                            writeln!(
                                stdout,
                                "Resumed and finalized repository migration to redb (generation={}).",
                                report.finalized.generation
                            )
                            .map_err(|err| {
                                StorageBackendError::io("write migration output", err)
                            })?;
                        }
                        StorageBackendKind::Sqlite => {
                            let report = resume_reverse_migration(&common_storage).await?;
                            writeln!(
                                stdout,
                                "Resumed and finalized repository migration to sqlite (generation={}).",
                                report.generation
                            )
                            .map_err(|err| {
                                StorageBackendError::io("write migration output", err)
                            })?;
                        }
                    }
                    return Ok(());
                }

                if manifest.backend == target {
                    if !args.force {
                        writeln!(
                            stdout,
                            "Repository is already migrated to `{}` (generation={}); skipping without overwriting (pass --force to re-verify).",
                            target.as_str(),
                            manifest.generation
                        )
                        .map_err(|err| StorageBackendError::io("write migration output", err))?;
                        return Ok(());
                    }
                    let report = dry_run_repository_migration(&common_storage, target).await?;
                    writeln!(
                        stdout,
                        "Repository is already finalized on `{}` (generation={}); verified existing layout (already_on_target={}).",
                        target.as_str(),
                        manifest.generation,
                        report.already_on_target
                    )
                    .map_err(|err| StorageBackendError::io("write migration output", err))?;
                    return Ok(());
                }

                match target {
                    StorageBackendKind::Redb => {
                        let next_gen = manifest.generation.saturating_add(1);
                        let report = execute_forward_migration(&common_storage, next_gen).await?;
                        writeln!(
                            stdout,
                            "Migrated repository to redb (generation={}).",
                            report.generation
                        )
                        .map_err(|err| StorageBackendError::io("write migration output", err))?;
                    }
                    StorageBackendKind::Sqlite => {
                        let report = execute_reverse_migration(&common_storage).await?;
                        writeln!(
                            stdout,
                            "Migrated repository to sqlite (generation={}).",
                            report.generation
                        )
                        .map_err(|err| StorageBackendError::io("write migration output", err))?;
                    }
                }
                return Ok(());
            }

            // No storage-format.json yet: inspect authority.
            match detect_storage_authority(&common_storage)? {
                StorageAuthority::LegacySqlite { .. } => match target {
                    StorageBackendKind::Sqlite => {
                        if !args.force {
                            writeln!(
                                stdout,
                                "Repository is already using sqlite; skipping without overwriting."
                            )
                            .map_err(|err| {
                                StorageBackendError::io("write migration output", err)
                            })?;
                            return Ok(());
                        }
                        writeln!(
                            stdout,
                            "Repository is already using legacy sqlite; nothing to reverse-migrate."
                        )
                        .map_err(|err| StorageBackendError::io("write migration output", err))?;
                        Ok(())
                    }
                    StorageBackendKind::Redb => {
                        let report =
                            execute_forward_migration(&common_storage, INITIAL_STORAGE_GENERATION)
                                .await?;
                        writeln!(
                            stdout,
                            "Migrated repository to redb (generation={}).",
                            report.generation
                        )
                        .map_err(|err| StorageBackendError::io("write migration output", err))?;
                        Ok(())
                    }
                },
                StorageAuthority::FreshEmpty => Err(StorageBackendError::NotFound {
                    resource: "repository storage database (libra.db or storage-format.json)"
                        .to_string(),
                }),
                StorageAuthority::Manifest(_) => Err(StorageBackendError::BackendInternal {
                    detail: "manifest unexpectedly appeared during authority check".to_string(),
                }),
            }
        }
        MigrationScope::Config { role, scope_dir } => {
            if let Some(manifest) = read_config_format_manifest(&scope_dir, role)? {
                if !manifest.finalized {
                    if manifest.backend != target {
                        return Err(StorageBackendError::ManifestNotFinalized {
                            detail: format!(
                                "unfinalized {role} config migration targets `{}` but `--to {}` was requested",
                                manifest.backend.as_str(),
                                target.as_str(),
                            ),
                        });
                    }
                    let report = resume_config_scope_migration(role, &scope_dir).await?;
                    writeln!(
                        stdout,
                        "Resumed and finalized {role} migration to `{}` (generation={}).",
                        target.as_str(),
                        report.generation
                    )
                    .map_err(|err| StorageBackendError::io("write migration output", err))?;
                    return Ok(());
                }

                if manifest.backend == target && !args.force {
                    writeln!(
                        stdout,
                        "{role} is already migrated to `{}` (generation={}); skipping without overwriting.",
                        target.as_str(),
                        manifest.generation
                    )
                    .map_err(|err| StorageBackendError::io("write migration output", err))?;
                    return Ok(());
                }
            }

            let report = migrate_config_scope(role, &scope_dir, target).await?;
            if report.already_at_target {
                writeln!(
                    stdout,
                    "{role} is already migrated to `{}` (generation={}); skipping without overwriting.",
                    target.as_str(),
                    report.generation
                )
                .map_err(|err| StorageBackendError::io("write migration output", err))?;
            } else {
                writeln!(
                    stdout,
                    "Migrated {role} to `{}` (generation={}, config_rows={}, config_kv_rows={}, schema_rows={}).",
                    target.as_str(),
                    report.generation,
                    report.config_rows_migrated,
                    report.config_kv_rows_migrated,
                    report.schema_version_rows_migrated,
                )
                .map_err(|err| StorageBackendError::io("write migration output", err))?;
            }
            Ok(())
        }
    }
}

fn format_cli_error(err: &StorageBackendError) -> String {
    format!("error [{}]: {err}", err.stable_code_str())
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = MigrateCliArgs::parse();
    let mut stdout = io::stdout().lock();
    // Strictly bind production `main()` to compile-time `REDB_RUNTIME_UNLOCKED`
    // with zero environment-variable bypasses (`ADR-RB-01` #3, `RB-11c`, `RB-11t`).
    match run_cli_with_gate(args, REDB_RUNTIME_UNLOCKED, &mut stdout).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let _ = writeln!(io::stderr(), "{}", format_cli_error(&err));
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use libra::{
        internal::db::schema::create_database_for_role,
        storage::{
            StorageBackendKind,
            config_store::{
                config_format_manifest_path, config_redb_path_in_scope_dir,
                config_sqlite_path_in_scope_dir, is_config_sqlite_fenced_tombstone,
            },
            manifest::{canonical_git_redb_rel_path, storage_manifest_path},
        },
    };
    use sea_orm::ConnectionTrait;
    use tempfile::tempdir;

    use super::*;

    /// Test-only seam driving the full `libra-migrate` CLI with explicit gate state.
    async fn run_cli_for_test(argv: &[&str], unlocked: bool) -> (StorageResult<()>, String) {
        let parsed = MigrateCliArgs::try_parse_from(argv).expect("parse libra-migrate test argv");
        let mut buf = Vec::new();
        let res = run_cli_with_gate(parsed, unlocked, &mut buf).await;
        (res, String::from_utf8(buf).expect("valid utf-8 stdout"))
    }

    async fn create_repo_sqlite_fixture(root: &Path) -> PathBuf {
        let dot_libra = root.join(".libra");
        fs::create_dir_all(&dot_libra).expect("create .libra");
        let db_path = dot_libra.join("libra.db");
        let db = create_database_for_role(
            db_path.to_str().expect("utf8 db path"),
            DatabaseRole::Repository,
        )
        .await
        .expect("create repository sqlite fixture");
        db.execute_unprepared(
            "INSERT INTO reference (name, kind, \"commit\", remote) VALUES ('main', 'Branch', '1111111111111111111111111111111111111111', NULL);",
        )
        .await
        .expect("seed reference row");
        db.execute_unprepared(
            "INSERT INTO config_kv (key, value, encrypted) VALUES ('user.name', 'cli-migrate-tester', 0);",
        )
        .await
        .expect("seed config_kv row");
        db.close().await.expect("close repo db");
        dot_libra
    }

    async fn create_config_sqlite_fixture(scope_dir: &Path, role: DatabaseRole) -> PathBuf {
        fs::create_dir_all(scope_dir).expect("create config scope dir");
        let db_path = config_sqlite_path_in_scope_dir(scope_dir);
        let db = create_database_for_role(db_path.to_str().expect("utf8 config db path"), role)
            .await
            .expect("create config sqlite fixture");
        db.execute_unprepared(
            "INSERT INTO config_kv (key, value, encrypted) VALUES ('core.editor', 'vim', 0);",
        )
        .await
        .expect("seed config_kv");
        db.close().await.expect("close config db");
        db_path
    }

    /// `RB-11c` AC4: `run_cli_with_gate(..., /*unlocked=*/false)` unconditionally
    /// rejects live cutover with `LBR-CLI-003` and zero disk writes even when
    /// `LIBRA_TEST=1` is set in the process environment.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cli_migrate_gate_rejects_when_locked_even_with_libra_test_env() {
        let tmp = tempdir().expect("tempdir");
        let dot_libra = tmp.path().join(".libra");
        fs::create_dir_all(&dot_libra).expect("mkdir .libra");
        fs::write(dot_libra.join("libra.db"), b"").expect("touch libra.db");
        let global_dir = tmp.path().join("global");
        let system_dir = tmp.path().join("system");
        fs::create_dir_all(&global_dir).expect("mkdir global");
        fs::create_dir_all(&system_dir).expect("mkdir system");
        let global_db = global_dir.join("config.db");
        let system_db = system_dir.join("config.db");
        fs::write(&global_db, b"").expect("touch global config.db");
        fs::write(&system_db, b"").expect("touch system config.db");

        // Explicitly verify that `LIBRA_TEST=1` does NOT bypass the locked gate.
        let prev_test = env::var_os("LIBRA_TEST");
        let prev_global = env::var_os(LIBRA_CONFIG_GLOBAL_DB_ENV);
        let prev_system = env::var_os(LIBRA_CONFIG_SYSTEM_DB_ENV);
        // SAFETY: isolated test process under nextest / single-threaded test run.
        unsafe {
            env::set_var("LIBRA_TEST", "1");
            env::set_var(LIBRA_CONFIG_GLOBAL_DB_ENV, &global_db);
            env::set_var(LIBRA_CONFIG_SYSTEM_DB_ENV, &system_db);
        }

        let repo_str = tmp.path().to_str().expect("utf8 path");
        let cases: &[&[&str]] = &[
            &["libra-migrate", "--repo", repo_str, "--to", "redb"],
            &["libra-migrate", "--repo", repo_str, "--to", "sqlite"],
            &["libra-migrate", "--global", "--to", "redb"],
            &["libra-migrate", "--system", "--to", "redb"],
        ];

        for argv in cases {
            let (res, out) = run_cli_for_test(argv, /* unlocked */ false).await;
            let err = res.expect_err("locked gate must reject live cutover");
            assert_eq!(
                err.stable_code_str(),
                "LBR-CLI-003",
                "argv {argv:?} must return LBR-CLI-003, got {err:?}"
            );
            assert!(
                matches!(err, StorageBackendError::PreReleaseGateLocked { .. }),
                "expected PreReleaseGateLocked for {argv:?}, got {err:?}"
            );
            assert!(
                out.is_empty(),
                "locked rejection must emit no stdout for {argv:?}"
            );
            let formatted = format_cli_error(&err);
            assert!(
                formatted.contains("LBR-CLI-003"),
                "CLI error formatting must expose LBR-CLI-003: {formatted}"
            );
        }

        // Zero disk writes across all scopes.
        assert!(!storage_manifest_path(&dot_libra).exists());
        assert!(!config_redb_path_in_scope_dir(&global_dir).exists());
        assert!(!config_format_manifest_path(&global_dir).exists());
        assert!(!config_redb_path_in_scope_dir(&system_dir).exists());
        assert!(!config_format_manifest_path(&system_dir).exists());

        // Restore env.
        unsafe {
            match prev_test {
                Some(v) => env::set_var("LIBRA_TEST", v),
                None => env::remove_var("LIBRA_TEST"),
            }
            match prev_global {
                Some(v) => env::set_var(LIBRA_CONFIG_GLOBAL_DB_ENV, v),
                None => env::remove_var(LIBRA_CONFIG_GLOBAL_DB_ENV),
            }
            match prev_system {
                Some(v) => env::set_var(LIBRA_CONFIG_SYSTEM_DB_ENV, v),
                None => env::remove_var(LIBRA_CONFIG_SYSTEM_DB_ENV),
            }
        }
    }

    /// `RB-11c` AC3 & AC5: `--dry-run` works read-only with zero disk writes
    /// (even when locked), re-entry without `--force` exits 0 without overwriting,
    /// and unlocked bidirectional roundtrip succeeds for repo + global/system config.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cli_migrate_test_seam_dry_run_reentry_and_bidirectional_roundtrip() {
        let tmp = tempdir().expect("tempdir");
        let repo_root = tmp.path().join("repo");
        let dot_libra = create_repo_sqlite_fixture(&repo_root).await;
        let repo_str = repo_root.to_str().expect("utf8 path");

        // 1. `--dry-run` works even when `unlocked == false` and writes zero files.
        let before_entries: Vec<_> = fs::read_dir(&dot_libra)
            .expect("readdir before dry-run")
            .map(|e| e.expect("entry").file_name())
            .collect();
        let (dry_res, dry_out) = run_cli_for_test(
            &[
                "libra-migrate",
                "--repo",
                repo_str,
                "--to",
                "redb",
                "--dry-run",
            ],
            /* unlocked */ false,
        )
        .await;
        dry_res.expect("--dry-run must succeed even when locked");
        assert!(dry_out.contains("dry-run: scope=repository current=sqlite target=redb"));
        assert!(dry_out.contains("table git.reference rows=1"));
        assert!(dry_out.contains("table git.config_kv rows=1"));
        assert!(dry_out.contains("estimated_target_bytes="));
        let after_entries: Vec<_> = fs::read_dir(&dot_libra)
            .expect("readdir after dry-run")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert_eq!(
            before_entries, after_entries,
            "--dry-run must produce zero disk writes"
        );
        assert!(!storage_manifest_path(&dot_libra).exists());

        // 2. Unlocked forward migration (`sqlite -> redb`).
        let (fwd_res, fwd_out) = run_cli_for_test(
            &["libra-migrate", "--repo", repo_str, "--to", "redb"],
            /* unlocked */ true,
        )
        .await;
        fwd_res.expect("forward repo migration via test seam");
        assert!(fwd_out.contains("Migrated repository to redb"));
        let manifest_after_fwd = read_storage_manifest(&dot_libra)
            .expect("read manifest")
            .expect("manifest exists");
        assert_eq!(manifest_after_fwd.backend, StorageBackendKind::Redb);
        assert!(manifest_after_fwd.finalized);

        // 3. Re-entry without `--force` exits 0 with clear message and preserves target unchanged.
        let manifest_bytes_before =
            fs::read(storage_manifest_path(&dot_libra)).expect("read manifest bytes");
        let git_redb_path =
            dot_libra.join(canonical_git_redb_rel_path(manifest_after_fwd.generation));
        let git_redb_mtime_before = fs::metadata(&git_redb_path)
            .expect("stat git.redb")
            .modified()
            .expect("mtime");

        let (reentry_res, reentry_out) = run_cli_for_test(
            &["libra-migrate", "--repo", repo_str, "--to", "redb"],
            /* unlocked */ true,
        )
        .await;
        reentry_res.expect("re-entry on already-migrated redb repo must exit 0");
        assert!(
            reentry_out.contains("already migrated to `redb`"),
            "unexpected re-entry message: {reentry_out}"
        );
        assert_eq!(
            manifest_bytes_before,
            fs::read(storage_manifest_path(&dot_libra)).expect("read manifest bytes after")
        );
        assert_eq!(
            git_redb_mtime_before,
            fs::metadata(&git_redb_path)
                .expect("stat git.redb after")
                .modified()
                .expect("mtime after")
        );

        // 4. Reverse `--dry-run` works read-only on redb repo.
        let (rev_dry_res, rev_dry_out) = run_cli_for_test(
            &[
                "libra-migrate",
                "--repo",
                repo_str,
                "--to",
                "sqlite",
                "--dry-run",
            ],
            /* unlocked */ false,
        )
        .await;
        rev_dry_res.expect("reverse --dry-run must succeed");
        assert!(rev_dry_out.contains("dry-run: scope=repository current=redb target=sqlite"));
        assert!(rev_dry_out.contains("estimated_target_bytes="));

        // 5. Unlocked reverse migration (`redb -> sqlite`) and re-entry idempotency.
        let (rev_res, rev_out) = run_cli_for_test(
            &["libra-migrate", "--repo", repo_str, "--to", "sqlite"],
            /* unlocked */ true,
        )
        .await;
        rev_res.expect("reverse repo migration via test seam");
        assert!(rev_out.contains("Migrated repository to sqlite"));
        let manifest_after_rev = read_storage_manifest(&dot_libra)
            .expect("read manifest after reverse")
            .expect("manifest exists");
        assert_eq!(manifest_after_rev.backend, StorageBackendKind::Sqlite);
        assert!(manifest_after_rev.finalized);

        let (rev_reentry_res, rev_reentry_out) = run_cli_for_test(
            &["libra-migrate", "--repo", repo_str, "--to", "sqlite"],
            /* unlocked */ true,
        )
        .await;
        rev_reentry_res.expect("reverse re-entry must exit 0");
        assert!(rev_reentry_out.contains("already migrated to `sqlite`"));

        // 6. GlobalConfig bidirectional roundtrip + dry-run + re-entry.
        let global_dir = tmp.path().join("global-scope");
        let global_db = create_config_sqlite_fixture(&global_dir, DatabaseRole::GlobalConfig).await;
        let prev_global = env::var_os(LIBRA_CONFIG_GLOBAL_DB_ENV);
        unsafe {
            env::set_var(LIBRA_CONFIG_GLOBAL_DB_ENV, &global_db);
        }

        let (g_dry_res, g_dry_out) = run_cli_for_test(
            &["libra-migrate", "--global", "--to", "redb", "--dry-run"],
            /* unlocked */ false,
        )
        .await;
        g_dry_res.expect("global --dry-run must succeed");
        assert!(g_dry_out.contains("dry-run: scope=global_config current=sqlite target=redb"));
        assert!(g_dry_out.contains("table global_config.config_kv rows=1"));
        assert!(!config_redb_path_in_scope_dir(&global_dir).exists());

        let (g_fwd_res, _) = run_cli_for_test(
            &["libra-migrate", "--global", "--to", "redb"],
            /* unlocked */ true,
        )
        .await;
        g_fwd_res.expect("global forward migration");
        assert!(config_redb_path_in_scope_dir(&global_dir).exists());
        assert!(
            is_config_sqlite_fenced_tombstone(&global_db).expect("check global fenced tombstone")
        );

        let (g_reentry_res, g_reentry_out) = run_cli_for_test(
            &["libra-migrate", "--global", "--to", "redb"],
            /* unlocked */ true,
        )
        .await;
        g_reentry_res.expect("global re-entry must exit 0");
        assert!(g_reentry_out.contains("already migrated to `redb`"));

        let (g_rev_res, _) = run_cli_for_test(
            &["libra-migrate", "--global", "--to", "sqlite"],
            /* unlocked */ true,
        )
        .await;
        g_rev_res.expect("global reverse migration");
        assert!(!is_config_sqlite_fenced_tombstone(&global_db).expect("global config.db restored"));

        unsafe {
            match prev_global {
                Some(v) => env::set_var(LIBRA_CONFIG_GLOBAL_DB_ENV, v),
                None => env::remove_var(LIBRA_CONFIG_GLOBAL_DB_ENV),
            }
        }
    }
}
