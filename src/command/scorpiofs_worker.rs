//! Libra-owned resident ScorpioFS worker.
//!
//! The public Libra CLI is intentionally short-lived, while a FUSE session
//! must remain alive after `worktree scorpiofs attach` returns. Libra therefore
//! starts this hidden worker from its own executable. The worker links the
//! ScorpioFS crate directly and exposes only a loopback control endpoint.
//! Durable desired state remains in Libra; the embedded ScorpioFS service is
//! explicitly configured not to persist or recover state itself.

use std::{net::SocketAddr, path::PathBuf};

use clap::Parser;

use crate::utils::error::{CliError, CliResult, StableErrorCode};

#[derive(Parser, Debug)]
pub struct ScorpioFsWorkerArgs {
    #[arg(long)]
    pub config_path: PathBuf,
    #[arg(long)]
    pub bind: SocketAddr,
    #[arg(long)]
    pub upper_root: PathBuf,
    #[arg(long)]
    pub cl_root: PathBuf,
    #[arg(long)]
    pub mount_root: PathBuf,
    #[arg(long)]
    pub runtime_state_file: PathBuf,
}

#[cfg(all(target_os = "linux", feature = "scorpiofs-direct"))]
pub async fn execute_safe(args: ScorpioFsWorkerArgs) -> CliResult<()> {
    use std::sync::Arc;

    use scorpiofs::{
        cli,
        daemon::antares::{AntaresDaemon, AntaresServiceImpl},
        util::config,
    };

    let config_path = args.config_path.to_str().ok_or_else(|| {
        CliError::fatal("ScorpioFS config path is not valid UTF-8")
            .with_stable_code(StableErrorCode::CliInvalidTarget)
    })?;
    let overrides = cli::antares_overrides(
        Some(args.upper_root),
        Some(args.cl_root),
        Some(args.mount_root),
        Some(args.runtime_state_file),
    );
    config::init_config_with(config_path, overrides).map_err(|error| {
        CliError::fatal(format!("failed to initialize embedded ScorpioFS: {error}"))
            .with_stable_code(StableErrorCode::RepoStateInvalid)
    })?;

    let service = Arc::new(AntaresServiceImpl::new_external_state(None).await);
    AntaresDaemon::new(service)
        .serve(args.bind)
        .await
        .map_err(|error| {
            CliError::fatal(format!("Libra ScorpioFS worker failed: {error}"))
                .with_stable_code(StableErrorCode::IoWriteFailed)
        })
}

#[cfg(not(all(target_os = "linux", feature = "scorpiofs-direct")))]
pub async fn execute_safe(_args: ScorpioFsWorkerArgs) -> CliResult<()> {
    Err(
        CliError::fatal("the direct ScorpioFS worker requires Linux and scorpiofs-direct")
            .with_stable_code(StableErrorCode::Unsupported),
    )
}
