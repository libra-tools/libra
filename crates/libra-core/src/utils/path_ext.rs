//! Path extension helpers for converting between workdir and absolute paths, string conversions, and containment checks across path lists.

use std::path::{Path, PathBuf};

use crate::utils::util;

pub trait PathExt {
    fn to_workdir(&self) -> PathBuf;
    fn to_string_or_panic(&self) -> String;
    fn workdir_to_absolute(&self) -> PathBuf;
    fn sub_of_paths<P, U>(&self, paths: U) -> bool
    where
        P: AsRef<Path>,
        U: IntoIterator<Item = P>;
}

impl PathExt for PathBuf {
    fn to_workdir(&self) -> PathBuf {
        util::to_workdir_path(self)
    }

    /// `PathBuf` to `String`, panics on non-UTF-8 paths.
    /// - aka: `into_os_string().into_string().expect("non-UTF-8 path")`
    ///
    /// Callers that may encounter non-UTF-8 paths should use
    /// `Path::to_string_lossy()` or `try_working_dir_string()` in
    /// `src/utils/util.rs` instead.
    fn to_string_or_panic(&self) -> String {
        // INVARIANT: the function's name documents that the input must be
        // UTF-8. Callers that pass non-UTF-8 OsString contents are violating
        // the API contract.
        self.clone()
            .into_os_string()
            .into_string()
            .expect("PathExt::to_string_or_panic called on non-UTF-8 path")
    }

    fn workdir_to_absolute(&self) -> PathBuf {
        util::workdir_to_absolute(self)
    }

    fn sub_of_paths<P, U>(&self, paths: U) -> bool
    where
        P: AsRef<Path>,
        U: IntoIterator<Item = P>,
    {
        // TODO 接口都改成 to workdir好了
        util::is_sub_of_paths(self, paths)
    }
}
