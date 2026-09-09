//! Snapshot-only deadline boundary around the shared ignore policy.

use std::{
    marker::PhantomData,
    path::{Path, PathBuf},
    rc::Rc,
    time::Instant,
};

use git_internal::internal::index::Index;

use super::{IgnorePolicy, should_ignore_with_matcher};
use crate::{
    command::status_probe::with_io_deadline_bounded,
    internal::layer::ExclusionSnapshot,
    utils::util::{self, SecureIgnoreWalkGuard},
};

/// One caller-owned ignore epoch with immutable worktree/layer context.
pub(crate) struct BoundedIgnoreWalk {
    workdir: PathBuf,
    layers: ExclusionSnapshot,
    epoch: u64,
    _walk: SecureIgnoreWalkGuard,
    // The guard must restore the same caller thread's current epoch on drop.
    _caller_thread: PhantomData<Rc<()>>,
}

impl BoundedIgnoreWalk {
    pub(crate) fn new(workdir: &Path, layers: ExclusionSnapshot) -> Self {
        // Config discovery belongs to the calling request, not a pooled worker.
        util::prewarm_ignore_config(workdir);
        let walk = util::begin_secure_ignore_walk();
        Self {
            workdir: workdir.to_path_buf(),
            layers,
            epoch: util::secure_ignore_walk_epoch(),
            _walk: walk,
            _caller_thread: PhantomData,
        }
    }

    /// `None` is unknown, never an empty rule set or a visible candidate.
    pub(crate) fn should_ignore(
        &self,
        path: &Path,
        policy: IgnorePolicy,
        index: &Index,
        is_dir: bool,
        deadline: Instant,
    ) -> Option<bool> {
        if Instant::now() >= deadline || util::ignore_read_failed_for_walk(self.epoch) {
            return None;
        }
        let answer = should_ignore_with_matcher(path, policy, index, &self.layers, || {
            let workdir = self.workdir.clone();
            let target = if path.is_absolute() {
                path.to_path_buf()
            } else {
                workdir.join(path)
            };
            let layers = self.layers.clone();
            let epoch = self.epoch;
            with_io_deadline_bounded(
                deadline.saturating_duration_since(Instant::now()),
                move || {
                    util::check_gitignore_with_layers_as_dir_for_walk(
                        &workdir, &target, &layers, is_dir, epoch,
                    )
                },
            )
        });
        if Instant::now() >= deadline || util::ignore_read_failed_for_walk(self.epoch) {
            return None;
        }
        answer.ok()
    }
}
