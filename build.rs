//! RC-20: `cargo build` no longer runs the Next.js export. The `web/`
//! directory was deleted after plan-20260920 (RC-20 kept only the version
//! face; the user removed the whole tree — version faces are now three).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
}
