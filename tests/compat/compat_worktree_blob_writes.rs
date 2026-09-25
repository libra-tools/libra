//! plan issues/470 FM-02 compat guard: worktree blob writers must use the
//! shared `utils::worktree_blob` primitive, and retired direct-write / fixed-
//! mode helpers must stay retired, including in new merge submodules.
//!
//! The guard pins two things:
//! 1. Known blob-writing modules reference `write_worktree_blob` (the routing
//!    contract from ADR-FM-02/03). For merge, locate the actual blob writer
//!    across the facade and recursively discovered submodules, then require a
//!    call to the primitive inside that function body.
//! 2. The old direct-write / fixed-chmod helpers and their call shapes do not
//!    come back in any scanned module. Anything else is an unclassified bypass.
//!
//! Test-only code (`#[cfg(test)]` bodies) may legitimately use `fs::write` for
//! fixtures, so the deny-list names the retired production shapes explicitly
//! instead of banning `fs::write` outright.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use syn::{
    Expr, ExprCall, Item,
    visit::{self, Visit},
};

/// Modules migrated by FM-02 (plus FM-01's restore, which shares the guard).
/// Merge is scanned as a directory below because its writer may move between
/// the facade and a submodule without changing the routing contract.
const BLOB_WRITING_MODULES: &[&str] = &[
    "src/command/cherry_pick.rs",
    "src/command/hydrate.rs",
    "src/command/rebase.rs",
    "src/command/reset.rs",
    "src/command/restore.rs",
    "src/command/revert.rs",
    "src/command/stash.rs",
];

const MERGE_BLOB_WRITER: &str = "write_workdir_file_with_mode";

/// Retired production shapes. Each is a direct blob write or a fixed-mode chmod
/// that FM-01/FM-02 replaced with the primitive.
const FORBIDDEN_SHAPES: &[&str] = &[
    "fs::write(&file_path, content)",
    "fs::write(path, content)",
    "fs::write(&target_path, &blob.data)",
    "fs::write(&full, &blob.data)",
    "util::write_file(&blob.data",
    "apply_worktree_blob_mode",
    "apply_file_mode",
    "set_executable_workdir_mode",
];

fn collect_rs_files(dir: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_rs_files(&path, files)?;
        } else if file_type.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
    Ok(())
}

#[derive(Default)]
struct SharedBlobCall {
    found: bool,
}

impl<'ast> Visit<'ast> for SharedBlobCall {
    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if let Expr::Path(function) = &*call.func {
            self.found |= function
                .path
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "write_worktree_blob");
        }
        visit::visit_expr_call(self, call);
    }
}

/// Parse the module, then inspect only the writer's body. A helper import,
/// comment, or unrelated function cannot satisfy this routing assertion.
fn merge_blob_writer_calls(source: &str) -> syn::Result<Vec<bool>> {
    let file = syn::parse_file(source)?;
    Ok(file
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Fn(function) if function.sig.ident == MERGE_BLOB_WRITER => Some(function),
            _ => None,
        })
        .map(|function| {
            let mut call = SharedBlobCall::default();
            call.visit_block(&function.block);
            call.found
        })
        .collect())
}

#[test]
fn blob_writers_route_through_the_primitive_and_merge_submodules_stay_guarded() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut problems = Vec::new();
    let mut modules: Vec<PathBuf> = BLOB_WRITING_MODULES
        .iter()
        .map(|module| root.join(module))
        .collect();
    modules.push(root.join("src/command/merge.rs"));
    collect_rs_files(&root.join("src/command/merge"), &mut modules)
        .expect("discover merge submodules");
    modules.sort();
    let mut merge_blob_writers = Vec::new();

    for module in modules {
        let relative = module
            .strip_prefix(&root)
            .expect("source under project root");
        let source = fs::read_to_string(&module)
            .unwrap_or_else(|error| panic!("read {}: {error}", relative.display()));
        let is_merge_module = relative.starts_with("src/command/merge/")
            || relative == Path::new("src/command/merge.rs");
        if is_merge_module {
            match merge_blob_writer_calls(&source) {
                Ok(writers) => {
                    for calls_shared_primitive in writers {
                        merge_blob_writers.push(relative.to_path_buf());
                        if !calls_shared_primitive {
                            problems.push(format!(
                                "{}: merge blob writer does not call the shared primitive inside its body",
                                relative.display()
                            ));
                        }
                    }
                }
                Err(error) => problems.push(format!(
                    "{}: cannot parse merge module: {error}",
                    relative.display()
                )),
            }
        } else if !source.contains("write_worktree_blob") {
            problems.push(format!(
                "{}: blob writer does not use the shared primitive",
                relative.display()
            ));
        }
        for (index, line) in source.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            for shape in FORBIDDEN_SHAPES {
                if line.contains(shape) {
                    problems.push(format!(
                        "{}:{}: retired shape `{shape}`",
                        relative.display(),
                        index + 1
                    ));
                }
            }
        }
    }

    if merge_blob_writers.len() != 1 {
        problems.push(format!(
            "expected one merge blob writer `{MERGE_BLOB_WRITER}` across merge.rs and merge/**/*.rs, found {merge_blob_writers:?}"
        ));
    }

    assert!(
        problems.is_empty(),
        "worktree blob writes must go through utils::worktree_blob:\n{}",
        problems.join("\n")
    );
}

#[test]
fn merge_blob_writer_call_must_be_in_its_own_body() {
    let missing_call = r#"
        use crate::utils::worktree_blob::write_worktree_blob;
        // The import and the unrelated call must not satisfy the writer contract.
        fn write_workdir_file_with_mode() {
            let _ = "write_worktree_blob()";
        }
        fn unrelated() { write_worktree_blob(); }
    "#;
    assert_eq!(merge_blob_writer_calls(missing_call).unwrap(), vec![false]);

    let direct_call = "fn write_workdir_file_with_mode() { worktree_blob::write_worktree_blob(); }";
    assert_eq!(merge_blob_writer_calls(direct_call).unwrap(), vec![true]);
}
