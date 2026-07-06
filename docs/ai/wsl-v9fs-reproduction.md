# WSL/v9fs Reproduction Guide

This document shows how to reproduce the original `creation time is not available for the filesystem` panic and how to verify the fix.

The bug only appears when the Libra worktree is on a filesystem where Rust `std::fs::Metadata::created()` returns `Unsupported`. In WSL this is most likely on a Windows-mounted or 9p/v9fs-backed path, not on the normal Linux ext4 home directory.

## 0. Repositories and Fixed Commits

Use these repository URLs and commit ids for the commands below:

```bash
LIBRA_URL="https://github.com/Ivanbeethoven/libra.git"
GIT_INTERNAL_URL="https://github.com/Ivanbeethoven/git-internal.git"

# Pre-fix Libra commit used to reproduce the panic.
LIBRA_VULNERABLE_COMMIT="1b4665f0"

# Fixed git-internal commit.
GIT_INTERNAL_FIX_COMMIT="20d2810"

# Libra commit that adds the local git-internal override.
LIBRA_OVERRIDE_COMMIT="4d3e11d7"
```

The fixed local validation layout must keep both repositories as siblings:

```text
libra-v9fs-repro/
├── git-internal/
└── libra-fixed/
```

That sibling layout matters because `libra/Cargo.toml` contains:

```toml
[patch.crates-io]
git-internal = { path = "../git-internal" }
```

## 1. Pick a Reproduction Directory

Run these commands inside WSL:

```bash
uname -a
mount | grep -E '9p|drvfs' || true
```

Choose a path on the mounted/shared filesystem that previously triggered the bug. Examples:

```bash
cd /mnt/c/Users/$USER
mkdir -p libra-v9fs-repro
cd libra-v9fs-repro
```

Confirm the path is not the normal Linux home filesystem:

```bash
df -T "$PWD"
stat -c 'file=%n type=%F mtime=%y ctime=%z birth=%w' .
```

If `birth` is `-`, creation/birth time is unavailable there, which is the condition that exposed the old panic.

For a direct Rust-level probe:

```bash
cat >/tmp/check-created.rs <<'RS'
use std::{env, fs};

fn main() {
    let path = env::args().nth(1).expect("usage: check-created <path>");
    let meta = fs::symlink_metadata(&path).expect("metadata");
    println!("created = {:?}", meta.created());
    println!("modified = {:?}", meta.modified());
}
RS

rustc /tmp/check-created.rs -o /tmp/check-created
touch created-probe.txt
/tmp/check-created created-probe.txt
```

The vulnerable environment prints something like:

```text
created = Err(Os { code: ..., kind: Unsupported, message: "creation time is not available for the filesystem" })
modified = Ok(...)
```

If `created = Ok(...)`, this exact WSL path may not reproduce the original issue. Try the path where the panic was first observed.

## 2. Reproduce the Old Panic

Use the pre-fix `libra` commit, before the local `git-internal` override was added:

```bash
git clone "$LIBRA_URL" libra-vulnerable
cd libra-vulnerable
git checkout "$LIBRA_VULNERABLE_COMMIT"
```

Build only the Rust binary. `LIBRA_SKIP_WEB_BUILD=1` avoids requiring `pnpm` for this reproduction:

```bash
LIBRA_SKIP_WEB_BUILD=1 cargo build --bin libra
LIBRA_BIN="$PWD/target/debug/libra"
```

Create a separate test repository on the same WSL/v9fs-backed filesystem:

```bash
cd ..
rm -rf vulnerable-worktree
"$LIBRA_BIN" init --vault false vulnerable-worktree
cd vulnerable-worktree
printf 'hello from v9fs\n' > tracked.txt
```

Trigger the index path:

```bash
RUST_BACKTRACE=1 "$LIBRA_BIN" add tracked.txt
```

Expected old failure:

```text
called `Result::unwrap()` on an `Err` value: Error { kind: Unsupported, message: "creation time is not available for the filesystem" }
fatal: CLI thread panicked
```

The backtrace or panic path should reference the crates.io copy of `git-internal-0.8.1`, for example:

```text
.../.cargo/registry/src/.../git-internal-0.8.1/src/internal/index.rs
```

## 3. Verify the Fixed Version

Clone both repositories as siblings on the same WSL filesystem:

```bash
cd ..
git clone "$GIT_INTERNAL_URL" git-internal
git clone "$LIBRA_URL" libra-fixed
```

The fixed commits are:

```bash
cd git-internal
git merge-base --is-ancestor "$GIT_INTERNAL_FIX_COMMIT" HEAD && echo "git-internal fix is present"

cd ../libra-fixed
git merge-base --is-ancestor "$LIBRA_OVERRIDE_COMMIT" HEAD && echo "libra local override is present"
```

Confirm `libra` is patched to use the sibling `git-internal` checkout:

```bash
grep -n -A2 '\[patch.crates-io\]' Cargo.toml
```

Expected:

```toml
[patch.crates-io]
git-internal = { path = "../git-internal" }
```

Build/check with the local patched crate:

```bash
LIBRA_SKIP_WEB_BUILD=1 cargo check --locked --bin libra
```

During the check, Cargo should show:

```text
Checking git-internal v0.8.1 (.../git-internal)
```

Build the fixed binary:

```bash
LIBRA_SKIP_WEB_BUILD=1 cargo build --locked --bin libra
LIBRA_BIN="$PWD/target/debug/libra"
```

Run the same add flow on the v9fs-backed filesystem:

```bash
cd ..
rm -rf fixed-worktree
"$LIBRA_BIN" init --vault false fixed-worktree
cd fixed-worktree
printf 'hello from fixed v9fs\n' > tracked.txt
"$LIBRA_BIN" add tracked.txt
"$LIBRA_BIN" status --short
```

Expected fixed behavior:

- `libra add tracked.txt` exits successfully.
- There is no panic about creation time.
- The panic path no longer references `.cargo/registry/src/.../git-internal-0.8.1/...`.

## 4. Troubleshooting

If the fixed run still panics and the path includes `.cargo/registry/src`, then `libra` is not using the local patched crate. Check:

```bash
pwd
ls -d ../git-internal
grep -n -A2 '\[patch.crates-io\]' Cargo.toml
cargo tree -i git-internal
```

If `cargo check --locked --bin libra` fails before Rust checking with a `pnpm install` error, set:

```bash
export LIBRA_SKIP_WEB_BUILD=1
```

If `libra init --vault false` fails because the home directory is not writable, set `HOME` to a writable WSL directory:

```bash
export HOME="$PWD/.home"
mkdir -p "$HOME"
```

If the old version does not panic, verify that the test repository itself is on the filesystem where `created()` is unsupported. Building Libra on ext4 and testing a worktree on v9fs is fine; the important part is where `tracked.txt` lives.
