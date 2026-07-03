# Operation Command Integration Change Log

## Scope

This change integrates the operation-log wrapper with the CLI command layer for
the B-side command integration requirement.

## Implemented

- Added `libra op` CLI dispatch so parsed `Commands::Op` requests execute
  through `command::op::execute_safe`.
- Added `src/command/op.rs` with `op log`, `op show`, and `op restore` command
  handlers using the current operation service APIs.
- Integrated `branch create` with `with_operation_log`.
- Moved the `branch create` branch write into the transaction supplied by
  `with_operation_log` by calling `Branch::update_branch_with_conn(txn, ...)`.
- Added operation schema bootstrap DDL for fresh repositories.
- Added idempotent operation schema creation during database connection setup so
  existing repositories can use operation logging after upgrade.
- Added an integration test proving `libra branch feature` records an operation
  and exposes a restore graph.

## Key Files

- `src/cli.rs`
- `src/command/op.rs`
- `src/command/branch.rs`
- `src/command/mod.rs`
- `src/internal/db.rs`
- `sql/sqlite_20260309_init.sql`
- `tests/command/branch_test.rs`

## Remote Verification

Verification was run on the requested bayesdl remote host under:

`/root/bayes-tmp/libra-codex-test/libra`

The Rust target directory used the existing remote cache:

`/root/libra-codex-target`

Environment used for Rust checks:

```sh
export RUSTUP_HOME=/root/bayes-tmp/rustup
export CARGO_HOME=/root/bayes-tmp/cargo
export CARGO_TARGET_DIR=/root/libra-codex-target
export PATH="$CARGO_HOME/bin:$PATH"
export OPENSSL_NO_VENDOR=1
unset OPENSSL_DIR OPENSSL_LIB_DIR OPENSSL_INCLUDE_DIR
export PKG_CONFIG_PATH=/usr/lib/x86_64-linux-gnu/pkgconfig
export LIBCLANG_PATH=/usr/lib/llvm-14/lib
export CLANG_PATH=/usr/bin/clang
export LIBRA_SKIP_WEB_BUILD=1
```

Passed checks:

```sh
rustfmt --edition 2024 --check \
  src/internal/db.rs \
  src/command/branch.rs \
  src/command/op.rs \
  src/cli.rs \
  src/command/mod.rs \
  tests/command/branch_test.rs

cargo check

cargo test --test command_test test_branch_create_records_operation_log -- --nocapture

cargo test --test operation_service_test --test operation_wrapper_test
```

Current re-verification on the bayesdl host also passed:

```sh
cargo check
cargo test --test command_test test_branch_create_records_operation_log -- --nocapture
rustfmt --edition 2024 --check \
  src/internal/db.rs \
  src/command/branch.rs \
  src/command/op.rs \
  src/cli.rs \
  src/command/mod.rs \
  tests/command/branch_test.rs
cargo test --test operation_service_test --test operation_wrapper_test
```

The command smoke test also passed:

```sh
libra init
libra config user.name RemoteTester
libra config user.email remote@example.com
libra add tracked.txt
libra commit -m base --no-verify
libra branch feature
libra op log --json
```

The resulting operation log contained a succeeded `branch` operation.

## Review Notes

- The B-side command integration requirement is satisfied by `branch create`.
- The operation tables are now available for both fresh repositories and existing
  repositories opened after this change.
- `op log --command` now applies filtering before pagination and reports the
  filtered `total`, so page boundaries remain stable for command-specific views.
- The earlier compile-shape issues in `op.rs` are resolved: no dependency on
  `util::repo_id`, no `Head::Commit`, and restore graph loading handles the
  service's `Option<OperationGraphRecord>` return shape.

## Known Limitations

- `op restore` currently restores HEAD and branch refs present in the captured
  operation view. It does not prune refs that exist in the current repository but
  are absent from the target view.
