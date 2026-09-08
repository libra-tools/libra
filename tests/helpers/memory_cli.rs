use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use serde_json::Value;

pub(crate) fn command(args: &[&str], repository: &Path) -> Command {
    let home = repository.join(".memory-test-home");
    let config_home = home.join(".config");
    let global_db = home.join(".libra").join("config.db");
    fs::create_dir_all(&config_home).expect("create isolated config home");

    let mut command = Command::new(env!("CARGO_BIN_EXE_libra"));
    command
        .args(args)
        .current_dir(repository)
        .env_clear()
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("LIBRA_CONFIG_GLOBAL_DB", &global_db)
        .env("LIBRA_TEST", "1")
        .env("LANG", "C")
        .env("LC_ALL", "C");
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    command
}

pub(crate) fn run(args: &[&str], repository: &Path) -> Output {
    command(args, repository)
        .output()
        .expect("execute Libra command")
}

pub(crate) fn init(repository: &Path) {
    let output = run(&["init"], repository);
    assert_success(&output, "initialize Memory test repository");
}

pub(crate) fn assert_success(output: &Output, operation: &str) {
    assert!(
        output.status.success(),
        "failed to {operation}: {}",
        String::from_utf8_lossy(&output.stderr),
    );
}

pub(crate) fn stdout_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("decode command JSON stdout")
}

pub(crate) fn stderr_json(output: &Output) -> Value {
    let text = String::from_utf8_lossy(&output.stderr);
    let trimmed = text.trim_end();
    if let Ok(value) = serde_json::from_str(trimmed) {
        return value;
    }
    let start = trimmed
        .rfind("\n{")
        .map(|index| index + 1)
        .or_else(|| trimmed.find('{'))
        .expect("structured command error should contain JSON");
    serde_json::from_str(&trimmed[start..]).expect("decode command JSON stderr")
}
