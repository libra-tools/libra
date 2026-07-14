//! Sandboxed `.libra/hooks` lifecycle contract for plan-20260708 P1-10.

#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Output},
};

use tempfile::{TempDir, tempdir};

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempdir().expect("create hook fixture");
        let root = temp.path().to_path_buf();
        let home = root.join("home");
        fs::create_dir_all(&home).expect("create isolated home");
        Self {
            _temp: temp,
            root,
            home,
        }
    }

    fn command(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_libra"));
        command
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("LIBRA_CONFIG_GLOBAL_DB", self.home.join(".libra/config.db"))
            .env("LIBRA_TEST", "1")
            .env("LANG", "C")
            .env("LC_ALL", "C");
        if let Some(profile_file) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", profile_file);
        }
        command
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> Output {
        self.command(cwd, args).output().expect("spawn libra")
    }

    fn success(&self, cwd: &Path, args: &[&str]) -> Output {
        let output = self.run(cwd, args);
        assert!(
            output.status.success(),
            "{} failed\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn init_repo(&self, name: &str) -> PathBuf {
        let repo = self.root.join(name);
        fs::create_dir_all(&repo).expect("create repository");
        let repo_text = repo.to_str().expect("utf8 repository path");
        self.success(&self.root, &["init", repo_text]);
        self.success(&repo, &["config", "set", "user.name", "Hook Test"]);
        self.success(&repo, &["config", "set", "user.email", "hooks@example.com"]);
        repo
    }

    fn stage(&self, repo: &Path, path: &str, contents: &str) {
        fs::write(repo.join(path), contents).expect("write staged fixture");
        self.success(repo, &["add", path]);
    }

    fn oid(&self, repo: &Path, revision: &str) -> String {
        String::from_utf8(self.success(repo, &["rev-parse", revision]).stdout)
            .expect("revision output is utf8")
            .trim()
            .to_string()
    }
}

fn write_hook(repo: &Path, name: &str, body: &str) -> PathBuf {
    let path = repo.join(".libra/hooks").join(name);
    fs::write(&path, body).expect("write repository hook");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
        .expect("make repository hook executable");
    path
}

#[test]
fn commit_hooks_run_in_order_modify_the_message_and_honor_escape_valves() {
    let fixture = Fixture::new();
    let repo = fixture.init_repo("commit-lifecycle");
    fixture.stage(&repo, "one.txt", "one\n");

    write_hook(
        &repo,
        "pre-commit",
        "#!/bin/sh\nprintf 'pre-commit\\n' >> \"$LIBRA_WORK_TREE/hook.log\"\n",
    );
    write_hook(
        &repo,
        "prepare-commit-msg",
        "#!/bin/sh\nprintf 'prepare:%s:%s:%s\\n' \"$1\" \"$2\" \"$3\" >> \"$LIBRA_WORK_TREE/hook.log\"\nprintf '\\nPrepared-by: prepare-commit-msg\\n' >> \"$1\"\n",
    );
    write_hook(
        &repo,
        "commit-msg",
        "#!/bin/sh\nprintf 'commit-msg:%s\\n' \"$1\" >> \"$LIBRA_WORK_TREE/hook.log\"\nprintf '\\nHooked-by: commit-msg\\n' >> \"$1\"\n",
    );
    write_hook(
        &repo,
        "post-commit",
        "#!/bin/sh\nprintf 'post-commit\\n' >> \"$LIBRA_WORK_TREE/hook.log\"\n",
    );

    fixture.success(&repo, &["commit", "--no-gpg-sign", "-m", "subject"]);
    let message_path = repo.join(".libra/COMMIT_EDITMSG");
    let expected_log = format!(
        "pre-commit\nprepare:{}:message:\ncommit-msg:{}\npost-commit\n",
        message_path.display(),
        message_path.display()
    );
    assert_eq!(
        fs::read_to_string(repo.join("hook.log")).expect("read hook order"),
        expected_log
    );
    let message = String::from_utf8(fixture.success(&repo, &["log", "-1", "--format=%B"]).stdout)
        .expect("commit message is utf8");
    assert!(
        message.contains("Prepared-by: prepare-commit-msg"),
        "{message}"
    );
    assert!(message.contains("Hooked-by: commit-msg"), "{message}");

    fixture.stage(&repo, "two.txt", "two\n");
    write_hook(&repo, "commit-msg", "#!/bin/sh\nexit 43\n");
    let head_before_block = fixture.oid(&repo, "HEAD");
    let blocked = fixture.run(&repo, &["commit", "--no-gpg-sign", "-m", "blocked"]);
    assert!(!blocked.status.success());
    assert_eq!(fixture.oid(&repo, "HEAD"), head_before_block);
    assert!(
        String::from_utf8_lossy(&blocked.stderr).contains("commit-msg hook failed"),
        "{}",
        String::from_utf8_lossy(&blocked.stderr)
    );
    let before_no_verify = fs::read_to_string(repo.join("hook.log")).expect("read hook log");
    fixture.success(
        &repo,
        &["commit", "--no-gpg-sign", "--no-verify", "-m", "no hooks"],
    );
    assert_eq!(
        fs::read_to_string(repo.join("hook.log")).expect("read no-verify hook log"),
        before_no_verify,
        "--no-verify must be a complete arbitrary-code escape valve"
    );

    write_hook(
        &repo,
        "commit-msg",
        "#!/bin/sh\nprintf 'commit-msg:%s\\n' \"$1\" >> \"$LIBRA_WORK_TREE/hook.log\"\nprintf '\\nHooked-by: commit-msg\\n' >> \"$1\"\n",
    );
    fixture.stage(&repo, "three.txt", "three\n");
    fixture.success(
        &repo,
        &[
            "commit",
            "--no-gpg-sign",
            "--disable-pre",
            "-m",
            "skip pre only",
        ],
    );
    let tail = fs::read_to_string(repo.join("hook.log")).expect("read disable-pre hook log");
    assert!(
        tail.ends_with(&format!(
            "prepare:{}:message:\ncommit-msg:{}\npost-commit\n",
            message_path.display(),
            message_path.display()
        )),
        "--disable-pre must skip only pre-commit: {tail}"
    );
}

#[test]
fn amend_post_rewrite_receives_stdin_and_failure_is_advisory() {
    let fixture = Fixture::new();
    let repo = fixture.init_repo("post-rewrite");
    fixture.stage(&repo, "base.txt", "base\n");
    fixture.success(
        &repo,
        &["commit", "--no-gpg-sign", "--no-verify", "-m", "base"],
    );
    let old = fixture.oid(&repo, "HEAD");

    write_hook(
        &repo,
        "post-rewrite",
        "#!/bin/sh\nread -r old new\nprintf 'rewrite:%s:%s:%s\\n' \"$1\" \"$old\" \"$new\" >> \"$LIBRA_WORK_TREE/hook.log\"\nprintf 'rewrite-stderr\\n' >&2\nexit 17\n",
    );
    write_hook(
        &repo,
        "post-commit",
        "#!/bin/sh\nprintf 'post-commit\\n' >> \"$LIBRA_WORK_TREE/hook.log\"\n",
    );

    let amended = fixture.run(
        &repo,
        &[
            "commit",
            "--amend",
            "--no-edit",
            "--allow-empty",
            "--no-gpg-sign",
        ],
    );
    assert!(
        amended.status.success(),
        "advisory hook failure must not roll back the amended commit: {}",
        String::from_utf8_lossy(&amended.stderr)
    );
    let new = fixture.oid(&repo, "HEAD");
    assert_ne!(new, old);
    assert_eq!(
        fs::read_to_string(repo.join("hook.log")).expect("read rewrite log"),
        format!("post-commit\nrewrite:amend:{old}:{new}\n")
    );
    let stderr = String::from_utf8_lossy(&amended.stderr);
    assert!(stderr.contains("rewrite-stderr"), "{stderr}");
    assert!(stderr.contains("post-rewrite hook"), "{stderr}");
    assert!(stderr.contains("code 17"), "{stderr}");

    let before_warning_exit = fixture.oid(&repo, "HEAD");
    let warning_exit = fixture.run(
        &repo,
        &[
            "--exit-code-on-warning",
            "commit",
            "--amend",
            "--no-edit",
            "--allow-empty",
            "--no-gpg-sign",
        ],
    );
    assert_eq!(
        warning_exit.status.code(),
        Some(9),
        "advisory hook warning must honor the global warning-exit contract"
    );
    assert_ne!(
        fixture.oid(&repo, "HEAD"),
        before_warning_exit,
        "warning exit 9 must not imply the completed amend was rolled back"
    );
}

#[test]
fn hooks_fail_closed_on_unsafe_files_and_cannot_escape_or_rewrite_metadata() {
    let fixture = Fixture::new();
    let repo = fixture.init_repo("hook-safety");
    fixture.stage(&repo, "base.txt", "base\n");
    fixture.success(
        &repo,
        &["commit", "--no-gpg-sign", "--no-verify", "-m", "base"],
    );

    fixture.stage(&repo, "blocked.txt", "blocked\n");
    let canonical = repo.join(".libra/hooks/pre-commit");
    fs::write(&canonical, "#!/bin/sh\nexit 0\n").expect("write non-executable canonical hook");
    fs::set_permissions(&canonical, fs::Permissions::from_mode(0o644))
        .expect("keep canonical hook non-executable");
    write_hook(
        &repo,
        "pre-commit.sh",
        "#!/bin/sh\ntouch \"$LIBRA_WORK_TREE/legacy-ran\"\n",
    );
    let before = fixture.oid(&repo, "HEAD");
    let rejected = fixture.run(&repo, &["commit", "--no-gpg-sign", "-m", "blocked"]);
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("not executable"),
        "{}",
        String::from_utf8_lossy(&rejected.stderr)
    );
    assert_eq!(fixture.oid(&repo, "HEAD"), before);
    assert!(!repo.join("legacy-ran").exists());

    fs::remove_file(&canonical).expect("remove non-executable canonical hook");
    fs::remove_file(repo.join(".libra/hooks/pre-commit.sh")).expect("remove legacy hook");
    write_hook(
        &repo,
        "pre-commit",
        "#!/bin/sh\ntest -z \"${LIBRA_HOOK_SECRET_TEST+x}\" || exit 41\ntest -n \"$PATH\" || exit 42\ntouch \"$LIBRA_WORK_TREE/inside\"\ntouch \"$LIBRA_WORK_TREE/../outside\"\n",
    );
    let sandboxed = fixture
        .command(&repo, &["commit", "--no-gpg-sign", "-m", "sandboxed"])
        .env("LIBRA_HOOK_SECRET_TEST", "must-not-reach-repository-code")
        .output()
        .expect("run commit with a secret-bearing caller environment");
    assert!(
        sandboxed.status.success(),
        "hook must receive the safe environment allowlist but not caller secrets: {}",
        String::from_utf8_lossy(&sandboxed.stderr)
    );
    assert!(repo.join("inside").exists());
    assert!(
        !fixture.root.join("outside").exists(),
        "the hook sandbox must not mutate the host outside its worktree"
    );
    for absent_metadata in [".git", ".codex", ".agents"] {
        assert!(
            !repo.join(absent_metadata).exists(),
            "sandbox mount setup must not create absent {absent_metadata} metadata"
        );
    }

    fixture.stage(&repo, "metadata.txt", "metadata\n");
    let index = repo.join(".libra/index");
    let index_before = fs::read(&index).expect("read index before metadata attack");
    write_hook(
        &repo,
        "pre-commit",
        "#!/bin/sh\nprintf 'corrupt' > \"$LIBRA_DIR/index\"\n",
    );
    let head_before = fixture.oid(&repo, "HEAD");
    let rejected = fixture.run(&repo, &["commit", "--no-gpg-sign", "-m", "metadata attack"]);
    assert!(!rejected.status.success());
    assert_eq!(fixture.oid(&repo, "HEAD"), head_before);
    assert_eq!(
        fs::read(&index).expect("read protected index"),
        index_before
    );

    fs::remove_file(repo.join(".libra/hooks/pre-commit")).expect("remove metadata hook");
    let target = fixture.root.join("symlink-target");
    fs::write(&target, "#!/bin/sh\nexit 0\n").expect("write symlink hook target");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o755))
        .expect("make symlink target executable");
    symlink(&target, repo.join(".libra/hooks/pre-commit")).expect("create symlink hook");
    let rejected = fixture.run(&repo, &["commit", "--no-gpg-sign", "-m", "symlink attack"]);
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("must be a regular file"),
        "{}",
        String::from_utf8_lossy(&rejected.stderr)
    );
    assert_eq!(fixture.oid(&repo, "HEAD"), head_before);
}

#[test]
fn post_checkout_reports_old_new_and_branch_or_path_mode() {
    let fixture = Fixture::new();
    let repo = fixture.init_repo("post-checkout");
    fixture.stage(&repo, "tracked.txt", "base\n");
    fixture.success(
        &repo,
        &["commit", "--no-gpg-sign", "--no-verify", "-m", "base"],
    );
    fixture.success(&repo, &["branch", "feature"]);
    fixture.success(&repo, &["switch", "feature"]);
    fixture.stage(&repo, "feature.txt", "feature\n");
    fixture.success(
        &repo,
        &["commit", "--no-gpg-sign", "--no-verify", "-m", "feature"],
    );
    let feature = fixture.oid(&repo, "HEAD");
    fixture.success(&repo, &["switch", "main"]);
    let main = fixture.oid(&repo, "HEAD");

    write_hook(
        &repo,
        "post-checkout",
        "#!/bin/sh\nprintf '%s:%s:%s\\n' \"$1\" \"$2\" \"$3\" >> \"$LIBRA_WORK_TREE/hook.log\"\n",
    );
    fixture.success(&repo, &["switch", "main"]);
    fixture.success(&repo, &["checkout", "main"]);
    assert!(
        !repo.join("hook.log").exists(),
        "already-on checkout/switch operations must not invoke post-checkout"
    );
    fixture.success(&repo, &["switch", "feature"]);
    fixture.success(&repo, &["checkout", "main"]);
    fs::write(repo.join("tracked.txt"), "dirty\n").expect("modify tracked path");
    fixture.success(&repo, &["checkout", "--", "tracked.txt"]);

    assert_eq!(
        fs::read_to_string(repo.join("hook.log")).expect("read checkout hook log"),
        format!("{main}:{feature}:1\n{feature}:{main}:1\n{main}:{main}:0\n")
    );

    let before_show = fs::read_to_string(repo.join("hook.log")).expect("read hook log");
    fixture.success(&repo, &["checkout"]);
    assert_eq!(
        fs::read_to_string(repo.join("hook.log")).expect("read show-current hook log"),
        before_show,
        "checkout with no state transition must not invoke post-checkout"
    );

    let skipped = fixture
        .command(&repo, &["switch", "feature"])
        .env("LIBRA_NO_HOOKS", "1")
        .output()
        .expect("switch with global hook escape");
    assert!(skipped.status.success());
    assert_eq!(
        fs::read_to_string(repo.join("hook.log")).expect("read escaped hook log"),
        before_show,
        "LIBRA_NO_HOOKS must disable post-operation hooks"
    );
}

#[test]
fn merge_hooks_block_before_ref_update_and_post_merge_is_advisory() {
    let fixture = Fixture::new();
    let repo = fixture.init_repo("merge-lifecycle");
    fixture.stage(&repo, "base.txt", "base\n");
    fixture.success(
        &repo,
        &["commit", "--no-gpg-sign", "--no-verify", "-m", "base"],
    );
    fixture.success(&repo, &["branch", "feature"]);
    fixture.success(&repo, &["switch", "feature"]);
    fixture.stage(&repo, "feature.txt", "feature\n");
    fixture.success(
        &repo,
        &["commit", "--no-gpg-sign", "--no-verify", "-m", "feature"],
    );
    fixture.success(&repo, &["switch", "main"]);
    fixture.stage(&repo, "main.txt", "main\n");
    fixture.success(
        &repo,
        &["commit", "--no-gpg-sign", "--no-verify", "-m", "main"],
    );

    write_hook(
        &repo,
        "pre-merge-commit",
        "#!/bin/sh\nprintf 'pre-collision\\n' >> \"$LIBRA_WORK_TREE/hook.log\"\nprintf 'hook-owned\\n' > \"$LIBRA_WORK_TREE/feature.txt\"\n",
    );
    write_hook(
        &repo,
        "post-merge",
        "#!/bin/sh\nprintf 'post-merge:%s\\n' \"$1\" >> \"$LIBRA_WORK_TREE/hook.log\"\nexit 19\n",
    );
    let before_collision = fixture.oid(&repo, "HEAD");
    let collision = fixture.run(&repo, &["merge", "--no-ff", "feature"]);
    assert!(
        !collision.status.success(),
        "a hook-created untracked collision must abort before ref update"
    );
    assert_eq!(fixture.oid(&repo, "HEAD"), before_collision);
    assert_eq!(
        fs::read_to_string(repo.join("feature.txt")).expect("read hook-created file"),
        "hook-owned\n"
    );
    fs::remove_file(repo.join("feature.txt")).expect("remove collision fixture");
    write_hook(
        &repo,
        "pre-merge-commit",
        "#!/bin/sh\nprintf 'pre-merge\\n' >> \"$LIBRA_WORK_TREE/hook.log\"\n",
    );
    write_hook(
        &repo,
        "prepare-commit-msg",
        "#!/bin/sh\nprintf 'prepare:%s\\n' \"$2\" >> \"$LIBRA_WORK_TREE/hook.log\"\nprintf '\\nPrepared-merge: yes\\n' >> \"$1\"\n",
    );
    write_hook(
        &repo,
        "commit-msg",
        "#!/bin/sh\nprintf 'commit-msg\\n' >> \"$LIBRA_WORK_TREE/hook.log\"\nprintf '\\nChecked-merge: yes\\n' >> \"$1\"\n",
    );
    write_hook(
        &repo,
        "post-commit",
        "#!/bin/sh\nprintf 'post-commit\\n' >> \"$LIBRA_WORK_TREE/hook.log\"\n",
    );
    let merged = fixture.run(&repo, &["merge", "--no-ff", "feature"]);
    assert!(
        merged.status.success(),
        "post-merge failure is advisory: {}",
        String::from_utf8_lossy(&merged.stderr)
    );
    assert_eq!(
        fs::read_to_string(repo.join("hook.log")).expect("read merge hook log"),
        "pre-collision\npre-merge\nprepare:merge\ncommit-msg\npost-commit\npost-merge:0\n"
    );
    let merge_message =
        String::from_utf8(fixture.success(&repo, &["log", "-1", "--format=%B"]).stdout)
            .expect("merge commit message is utf8");
    assert!(
        merge_message.contains("Prepared-merge: yes"),
        "{merge_message}"
    );
    assert!(
        merge_message.contains("Checked-merge: yes"),
        "{merge_message}"
    );
    assert!(
        String::from_utf8_lossy(&merged.stderr).contains("post-merge hook"),
        "{}",
        String::from_utf8_lossy(&merged.stderr)
    );

    fixture.success(&repo, &["branch", "topic"]);
    fixture.success(&repo, &["switch", "topic"]);
    fixture.stage(&repo, "topic.txt", "topic\n");
    fixture.success(
        &repo,
        &["commit", "--no-gpg-sign", "--no-verify", "-m", "topic"],
    );
    fixture.success(&repo, &["switch", "main"]);
    fixture.stage(&repo, "later.txt", "later\n");
    fixture.success(
        &repo,
        &["commit", "--no-gpg-sign", "--no-verify", "-m", "later"],
    );
    write_hook(&repo, "pre-merge-commit", "#!/bin/sh\nexit 0\n");
    write_hook(&repo, "commit-msg", "#!/bin/sh\nexit 31\n");
    let before_message_block = fixture.oid(&repo, "HEAD");
    let message_blocked = fixture.run(&repo, &["merge", "--no-ff", "topic"]);
    assert!(!message_blocked.status.success());
    assert_eq!(fixture.oid(&repo, "HEAD"), before_message_block);
    assert!(
        String::from_utf8_lossy(&message_blocked.stderr).contains("commit-msg hook failed"),
        "{}",
        String::from_utf8_lossy(&message_blocked.stderr)
    );
    write_hook(
        &repo,
        "pre-merge-commit",
        "#!/bin/sh\nprintf 'pre-block\\n' >> \"$LIBRA_WORK_TREE/hook.log\"\nexit 9\n",
    );
    let before = fixture.oid(&repo, "HEAD");
    let blocked = fixture.run(&repo, &["merge", "--no-ff", "topic"]);
    assert!(!blocked.status.success());
    assert_eq!(fixture.oid(&repo, "HEAD"), before);
    assert!(
        String::from_utf8_lossy(&blocked.stderr).contains("pre-merge-commit hook failed"),
        "{}",
        String::from_utf8_lossy(&blocked.stderr)
    );
    let before_bypass = fs::read_to_string(repo.join("hook.log")).expect("read blocked hook log");
    fixture.success(&repo, &["merge", "--no-ff", "--no-verify", "topic"]);
    assert_eq!(
        fs::read_to_string(repo.join("hook.log")).expect("read bypassed hook log"),
        before_bypass,
        "merge --no-verify must skip both blocking and advisory hooks"
    );
}

#[test]
fn rebase_hooks_receive_upstream_and_complete_rewrite_map() {
    let fixture = Fixture::new();
    let repo = fixture.init_repo("rebase-lifecycle");
    fixture.stage(&repo, "base.txt", "base\n");
    fixture.success(
        &repo,
        &["commit", "--no-gpg-sign", "--no-verify", "-m", "base"],
    );
    fixture.success(&repo, &["branch", "feature"]);
    fixture.success(&repo, &["switch", "feature"]);
    fixture.stage(&repo, "feature.txt", "feature\n");
    fixture.success(
        &repo,
        &["commit", "--no-gpg-sign", "--no-verify", "-m", "feature"],
    );
    let old_feature = fixture.oid(&repo, "HEAD");
    fixture.success(&repo, &["switch", "main"]);
    fixture.stage(&repo, "main.txt", "main\n");
    fixture.success(
        &repo,
        &["commit", "--no-gpg-sign", "--no-verify", "-m", "main"],
    );
    fixture.success(&repo, &["switch", "feature"]);

    write_hook(
        &repo,
        "pre-rebase",
        "#!/bin/sh\nprintf 'pre-rebase:%s:%s\\n' \"$1\" \"$2\" >> \"$LIBRA_WORK_TREE/hook.log\"\n",
    );
    write_hook(
        &repo,
        "post-rewrite",
        "#!/bin/sh\nwhile read -r old new; do printf 'post-rewrite:%s:%s:%s\\n' \"$1\" \"$old\" \"$new\" >> \"$LIBRA_WORK_TREE/hook.log\"; done\nexit 21\n",
    );
    let rebased = fixture.run(&repo, &["rebase", "main"]);
    assert!(
        rebased.status.success(),
        "post-rewrite failure is advisory: {}",
        String::from_utf8_lossy(&rebased.stderr)
    );
    let new_feature = fixture.oid(&repo, "HEAD");
    assert_ne!(new_feature, old_feature);
    assert_eq!(
        fs::read_to_string(repo.join("hook.log")).expect("read rebase hook log"),
        format!("pre-rebase:main:\npost-rewrite:rebase:{old_feature}:{new_feature}\n")
    );
    assert!(
        String::from_utf8_lossy(&rebased.stderr).contains("post-rewrite hook"),
        "{}",
        String::from_utf8_lossy(&rebased.stderr)
    );

    fixture.success(&repo, &["branch", "topic"]);
    fixture.success(&repo, &["switch", "topic"]);
    fixture.stage(&repo, "topic.txt", "topic\n");
    fixture.success(
        &repo,
        &["commit", "--no-gpg-sign", "--no-verify", "-m", "topic"],
    );
    fixture.success(&repo, &["switch", "feature"]);
    fixture.stage(&repo, "feature-later.txt", "later\n");
    fixture.success(
        &repo,
        &[
            "commit",
            "--no-gpg-sign",
            "--no-verify",
            "-m",
            "feature later",
        ],
    );
    fixture.success(&repo, &["switch", "topic"]);
    write_hook(&repo, "pre-rebase", "#!/bin/sh\nexit 7\n");
    let before = fixture.oid(&repo, "HEAD");
    let blocked = fixture.run(&repo, &["rebase", "feature"]);
    assert!(!blocked.status.success());
    assert_eq!(fixture.oid(&repo, "HEAD"), before);

    let bypassed = fixture
        .command(&repo, &["rebase", "feature"])
        .env("LIBRA_NO_HOOKS", "1")
        .output()
        .expect("run rebase with hook escape");
    assert!(
        bypassed.status.success(),
        "LIBRA_NO_HOOKS must bypass pre-rebase: {}",
        String::from_utf8_lossy(&bypassed.stderr)
    );
}
