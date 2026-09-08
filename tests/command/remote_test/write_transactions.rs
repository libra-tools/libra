use std::time::Duration;

use libra::{
    command::remote::{self, RemoteCmds},
    internal::{branch::Branch, config::ConfigKv, db::establish_connection},
    utils::{output::OutputConfig, path},
};
use sea_orm::{ConnectionTrait, DatabaseTransaction, TransactionTrait};
use serial_test::serial;
use tempfile::tempdir;

async fn hold_independent_write_lock() -> DatabaseTransaction {
    let db_path = path::database();
    let holder = establish_connection(
        db_path
            .to_str()
            .expect("temporary repository database path should be UTF-8"),
    )
    .await
    .expect("open independent database connection");
    let transaction = holder.begin().await.expect("begin holder transaction");
    transaction
        .execute_unprepared("UPDATE `config_kv` SET `id` = `id` WHERE 0")
        .await
        .expect("acquire SQLite write lock");
    transaction
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(cwd)]
async fn test_remote_read_then_write_transactions_wait_for_existing_writer() {
    let repo = tempdir().expect("create temporary repository");
    libra::utils::test::setup_with_new_libra_in(repo.path()).await;
    let _cwd = libra::utils::test::ChangeDirGuard::new(repo.path());

    remote::execute_safe(
        RemoteCmds::Add {
            name: "origin".into(),
            url: "https://example.com/repo.git".into(),
            fetch: false,
            track: vec![],
            master: None,
            tags: false,
            no_tags: false,
            mirror: false,
        },
        &OutputConfig::default(),
    )
    .await
    .expect("add remote");

    let holder = hold_independent_write_lock().await;
    let mut set_branches = tokio::spawn(async {
        remote::execute_safe(
            RemoteCmds::SetBranches {
                add: false,
                name: "origin".into(),
                branches: vec!["main".into()],
            },
            &OutputConfig::default(),
        )
        .await
    });
    if let Ok(result) = tokio::time::timeout(Duration::from_millis(100), &mut set_branches).await {
        panic!("set-branches completed while another connection held the write lock: {result:?}");
    }
    holder.commit().await.expect("release write lock");
    set_branches
        .await
        .expect("join set-branches task")
        .expect("set-branches should succeed after the lock is released");
    let refspecs = ConfigKv::get_all("remote.origin.fetch")
        .await
        .expect("read rewritten fetch refspecs");
    assert_eq!(refspecs.len(), 1);
    assert_eq!(
        refspecs[0].value,
        "+refs/heads/main:refs/remotes/origin/main"
    );
    Branch::update_branch(
        "refs/remotes/origin/main",
        "0000000000000000000000000000000000000000",
        Some("origin"),
    )
    .await
    .expect("create remote-tracking branch");

    let holder = hold_independent_write_lock().await;
    let mut rename = tokio::spawn(async {
        remote::execute_safe(
            RemoteCmds::Rename {
                old: "origin".into(),
                new: "upstream".into(),
            },
            &OutputConfig::default(),
        )
        .await
    });
    if let Ok(result) = tokio::time::timeout(Duration::from_millis(100), &mut rename).await {
        panic!("rename completed while another connection held the write lock: {result:?}");
    }
    holder.commit().await.expect("release write lock");
    rename
        .await
        .expect("join rename task")
        .expect("rename should succeed after the lock is released");

    assert!(
        ConfigKv::remote_config("origin")
            .await
            .expect("read old remote namespace")
            .is_none()
    );
    let renamed = ConfigKv::remote_config("upstream")
        .await
        .expect("read renamed remote namespace")
        .expect("renamed remote should exist");
    assert_eq!(renamed.url, "https://example.com/repo.git");

    let holder = hold_independent_write_lock().await;
    let mut set_head = tokio::spawn(async {
        remote::execute_safe(
            RemoteCmds::SetHead {
                auto: false,
                delete: false,
                name: "upstream".into(),
                branch: Some("main".into()),
            },
            &OutputConfig::default(),
        )
        .await
    });
    if let Ok(result) = tokio::time::timeout(Duration::from_millis(100), &mut set_head).await {
        panic!("set-head completed while another connection held the write lock: {result:?}");
    }
    holder.commit().await.expect("release write lock");
    set_head
        .await
        .expect("join set-head task")
        .expect("set-head should succeed after the lock is released");
}
