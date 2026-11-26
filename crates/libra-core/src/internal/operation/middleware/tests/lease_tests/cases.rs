//! Production acquisition, error, and cancellation cases inside a supervised child.

use std::{fs, future::Future, task::Poll};

use super::{
    ScopeLease, operations,
    support::{Fixture, REPO_ID, busy_error, lock_path, refused},
};

pub(super) async fn run(name: &str) {
    #[cfg(unix)]
    if name == "shared_group_permissions" {
        // SAFETY: this case runs alone in its dedicated supervised child, and
        // the process exits after the one selected test completes.
        unsafe { libc::umask(0o077) };
    }
    let fixture = Fixture::open(matches!(
        name,
        "same_scope" | "different_scope" | "cancelled_operation" | "shared_group_permissions"
    ))
    .await;
    match name {
        "same_scope" => operations::same_scope(&fixture).await,
        "different_scope" => operations::different_scope(&fixture).await,
        "cancelled_operation" => operations::cancelled_operation(&fixture).await,
        "external_holder" => {
            fixture.ready();
            let error = refused(ScopeLease::acquire(&fixture.main, REPO_ID).await);
            busy_error(&error, &fixture.main);
        }
        "cancelled_waiter" => cancelled_waiter(&fixture).await,
        "released" => {
            // Given a real held lease; when its guard is dropped, another handle may acquire.
            let first = ScopeLease::acquire(&fixture.main, REPO_ID)
                .await
                .expect("first lease");
            fixture.ready();
            drop(first);
            let second = ScopeLease::acquire(&fixture.main, REPO_ID)
                .await
                .expect("released lease");
            drop(second);
            assert!(
                lock_path(&fixture.main).is_file(),
                "persistent lock file disappeared"
            );
        }
        "open_error" => {
            // Given a non-directory parent, opening cannot possibly be mere lock contention.
            let info = fixture.main.gitdir.join("info");
            fs::remove_dir(&info).expect("empty info directory");
            fs::write(&info, b"not a directory").expect("invalid parent");
            let cause = fs::create_dir_all(&info)
                .expect_err("real OS failure")
                .to_string();
            fixture.ready();
            let error = refused(ScopeLease::acquire(&fixture.main, REPO_ID).await);
            let message = error.to_string();
            // Then the real cause and affected directory survive error translation.
            assert!(message.contains(&info.display().to_string()), "{message}");
            assert!(
                message.contains(&cause)
                    || message.to_ascii_lowercase().contains("not a directory")
                    || message.contains("must be a directory"),
                "{message}"
            );
            assert!(!message.contains("already held"), "{message}");
        }
        "unchanged_contents" => {
            // Given existing diagnostic contents, acquiring a lock need not mutate them.
            let path = lock_path(&fixture.main);
            fs::write(&path, b"existing lock payload\n").expect("existing lock payload");
            fixture.ready();
            for _ in 0..3 {
                drop(
                    ScopeLease::acquire(&fixture.main, REPO_ID)
                        .await
                        .expect("lease"),
                );
            }
            assert_eq!(
                fs::read(&path).expect("lock payload"),
                b"existing lock payload\n"
            );
        }
        #[cfg(unix)]
        "shared_group_permissions" => shared_group_permissions(&fixture).await,
        #[cfg(unix)]
        "symlink_leaf" => symlink_leaf(&fixture).await,
        #[cfg(unix)]
        "symlink_parent" => symlink_parent(&fixture).await,
        #[cfg(unix)]
        "fifo_leaf" => {
            // Given a FIFO with no reader, the ordinary open path itself must not block.
            let path = lock_path(&fixture.main);
            assert!(
                std::process::Command::new("mkfifo")
                    .arg(&path)
                    .status()
                    .expect("mkfifo")
                    .success()
            );
            fixture.ready();
            let error = refused(ScopeLease::acquire(&fixture.main, REPO_ID).await);
            assert!(
                error.to_string().contains(&path.display().to_string()),
                "{error}"
            );
        }
        _ => panic!("unknown supervised lease case: {name}"),
    }
}

#[cfg(unix)]
async fn shared_group_permissions(fixture: &Fixture) {
    use std::os::unix::fs::PermissionsExt;

    use crate::internal::{config::ConfigKv, db::get_db_conn_instance_for_path};

    let connection = get_db_conn_instance_for_path(&fixture.main.storage.join("libra.db"))
        .await
        .expect("repository connection");
    ConfigKv::set_with_conn(&connection, "core.sharedRepository", "group", false)
        .await
        .expect("shared repository config");
    fixture.ready();

    let result = crate::internal::operation::run_with_operation(
        &fixture.main,
        crate::internal::operation::OperationMetaV2::default(),
        crate::internal::operation::MutationClass::WorkspaceMutation,
        |_| async {
            Err::<(), _>(crate::internal::operation::OperationError::Mutation(
                "permission probe completed".to_string(),
            ))
        },
    )
    .await;
    assert!(
        matches!(
            result,
            Err(crate::internal::operation::OperationError::Mutation(ref reason))
                if reason == "permission probe completed"
        ),
        "the production operation did not reach its callback: {result:?}"
    );

    let mode = fs::metadata(lock_path(&fixture.main))
        .expect("operation scope lock metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o660,
        "Git-compatible shared=group must add group read/write despite umask 077"
    );
}

async fn cancelled_waiter(fixture: &Fixture) {
    // Given an external holder and a runtime with only one blocking worker.
    fixture.ready();
    let mut acquisition = Box::pin(ScopeLease::acquire(&fixture.main, REPO_ID));
    // Poll once to start acquisition without assuming whether its first poll is Ready.
    let observed =
        std::future::poll_fn(|context| Poll::Ready(acquisition.as_mut().poll(context))).await;
    drop(acquisition);
    match observed {
        Poll::Ready(result) => busy_error(&refused(result), &fixture.main),
        Poll::Pending => {}
    }
    // When the waiting future is cancelled, unrelated blocking work must still complete.
    // We do not drop/shutdown the runtime to make queued work disappear.
    let sentinel = tokio::task::spawn_blocking(|| "blocking pool is live")
        .await
        .expect("blocking sentinel");
    assert_eq!(sentinel, "blocking pool is live");
}

#[cfg(unix)]
async fn symlink_leaf(fixture: &Fixture) {
    let target = fixture
        .root
        .parent()
        .expect("sandbox")
        .join("external-payload");
    fs::write(&target, b"must remain unchanged").expect("external payload");
    let path = lock_path(&fixture.main);
    std::os::unix::fs::symlink(&target, &path).expect("symlink lock");
    fixture.ready();
    let result = ScopeLease::acquire(&fixture.main, REPO_ID).await;
    // Then neither successful nor failed acquisition may append through the symlink.
    assert_eq!(
        fs::read(&target).expect("external payload"),
        b"must remain unchanged"
    );
    let error = refused(result);
    assert!(
        error.to_string().contains(&path.display().to_string()),
        "{error}"
    );
}

#[cfg(unix)]
async fn symlink_parent(fixture: &Fixture) {
    let target = fixture
        .root
        .parent()
        .expect("sandbox")
        .join("external-info");
    fs::create_dir(&target).expect("external directory");
    let info = fixture.main.gitdir.join("info");
    fs::remove_dir(&info).expect("empty info");
    std::os::unix::fs::symlink(&target, &info).expect("symlink info");
    fixture.ready();
    let result = ScopeLease::acquire(&fixture.main, REPO_ID).await;
    assert!(
        !target.join("operation-v2.lock").exists(),
        "created a lock outside the private gitdir"
    );
    let error = refused(result);
    assert!(
        error.to_string().contains(&info.display().to_string()),
        "{error}"
    );
}
