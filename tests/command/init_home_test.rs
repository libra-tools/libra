//! Regression coverage for the per-user storage boundary (#472).

use super::*;

#[test]
fn init_refuses_absent_default_home_storage() {
    // Given a fresh HOME with no .libra directory (including aliased temp paths).
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    fs::create_dir(&home).unwrap();
    let envs = [
        ("HOME", home.to_str().unwrap()),
        ("USERPROFILE", home.to_str().unwrap()),
    ];

    // When init uses its default '.' target, it must refuse before creating it.
    let output =
        run_libra_command_with_env(&["init", "--vault", "false", "-b", "main"], &home, &envs);

    // Then the user state directory is not converted into repository storage.
    assert!(
        !output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("Libra home"));
    assert!(!home.join(".libra").exists());
}

#[test]
fn init_refuses_absent_relative_libra_home_for_bare_repository() {
    // Given an override naming a state directory that has not been created.
    let temp = tempdir().unwrap();

    // When the same relative path is supplied as a bare repository target.
    let output = run_libra_command_with_env(
        &["init", "--bare", ".state", "--vault", "false", "-b", "main"],
        temp.path(),
        &[("LIBRA_HOME", ".state")],
    );

    // Then path spelling cannot bypass the storage boundary.
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Libra home"));
    assert!(!temp.path().join(".state").exists());
}

#[tokio::test]
async fn explicit_upgrade_home_does_not_make_global_config_directory_a_repository() {
    // Given separate upgrade state and global config directories.
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let config_dir = home.join(".libra");
    fs::create_dir_all(&config_dir).unwrap();
    super::seed_libra_home_artifact_db(&config_dir.join("libra.db")).await;
    let state = temp.path().join("state");

    // When discovery runs from HOME with the upgrade home overridden.
    let output = run_libra_command_with_env(
        &["rev-parse", "--git-dir"],
        &home,
        &[
            ("HOME", home.to_str().unwrap()),
            ("USERPROFILE", home.to_str().unwrap()),
            ("LIBRA_HOME", state.to_str().unwrap()),
        ],
    );

    // Then the actual global config directory remains reserved too.
    assert!(
        !output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("global Libra home"));
    assert_eq!(output.status.code(), Some(128));
    assert!(String::from_utf8_lossy(&output.stderr).contains("LBR-REPO-001"));
    assert!(!config_dir.join("objects").exists());
}

#[tokio::test]
async fn discovery_refuses_commondir_pointing_at_libra_home() {
    // Given a linked gitdir whose common storage is the user's state directory.
    let temp = tempdir().unwrap();
    let home = temp.path().join("state");
    fs::create_dir(&home).unwrap();
    super::seed_libra_home_artifact_db(&home.join("libra.db")).await;
    let before = fs::read(home.join("libra.db")).unwrap();
    let repo = temp.path().join("repo");
    let gitdir = repo.join(".libra");
    fs::create_dir_all(&gitdir).unwrap();
    fs::write(gitdir.join("commondir"), home.to_str().unwrap()).unwrap();
    fs::write(gitdir.join("worktree_id"), "1").unwrap();

    // When repository discovery follows the linked worktree pointer.
    let output = run_libra_command_with_env(
        &["rev-parse", "--git-common-dir"],
        &repo,
        &[("LIBRA_HOME", home.to_str().unwrap())],
    );

    // Then it refuses the storage boundary violation without opening the DB.
    assert!(
        !output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("commondir"), "{stderr}");
    assert_eq!(fs::read(home.join("libra.db")).unwrap(), before);
}
