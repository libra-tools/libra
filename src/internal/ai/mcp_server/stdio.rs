//! Newline-delimited JSON-RPC transport for `libra agent mcp --stdio`.
//!
//! stdout receives one JSON-RPC message per line. Diagnostics stay off stdout.

use std::{collections::VecDeque, io::ErrorKind, path::Path};

use serde_json::Value;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt},
    sync::mpsc,
    task::JoinHandle,
};

use super::protocol::{self, InboundLine, MAX_LINE_BYTES, Session};

/// Read the repository identifier (`libra.repoid`) from a backend-agnostic
/// [`crate::storage::ConfigStore`], falling back to `"unavailable"` when
/// absent, encrypted, empty, or sentinel.
pub async fn repository_id_with_store(store: &dyn crate::storage::ConfigStore) -> String {
    let Ok(Some(entry)) = store.get("libra.repoid").await else {
        return "unavailable".to_string();
    };
    if entry.encrypted || entry.value.trim().is_empty() || entry.value == "unknown-repo" {
        "unavailable".to_string()
    } else {
        entry.value
    }
}

async fn repository_id_for_storage_inner(storage: &Path) -> crate::storage::StorageResult<String> {
    match crate::storage::runtime::detect_repo_backend(storage)? {
        crate::storage::StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                let store =
                    crate::storage::runtime::open_repo_config_store_read_only(storage).await?;
                Ok(repository_id_with_store(store.as_ref()).await)
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                Ok("unavailable".to_string())
            }
        }
        crate::storage::StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
                let db_path = storage.join(crate::utils::util::DATABASE);
                let Ok(conn) =
                    crate::internal::db::open_database_without_migrations(&db_path).await
                else {
                    return Ok("unavailable".to_string());
                };
                use sea_orm::{ConnectionTrait, Statement};
                let Ok(Some(row)) = conn
                    .query_one_raw(Statement::from_sql_and_values(
                        conn.get_database_backend(),
                        "SELECT value, encrypted FROM config_kv WHERE key = ? ORDER BY id DESC LIMIT 1",
                        ["libra.repoid".into()],
                    ))
                    .await
                else {
                    return Ok("unavailable".to_string());
                };
                let Ok(value) = row.try_get_by_index::<String>(0) else {
                    return Ok("unavailable".to_string());
                };
                let encrypted = row.try_get_by_index::<bool>(1).unwrap_or(true);
                if encrypted || value.trim().is_empty() || value == "unknown-repo" {
                    Ok("unavailable".to_string())
                } else {
                    Ok(value)
                }
            }
            #[cfg(not(feature = "storage-sqlite"))]
            {
                Ok("unavailable".to_string())
            }
        }
    }
}

/// Resolve the repository identifier (`libra.repoid`) for `storage` across
/// both SQLite and `redb` repositories without mutating schema state.
pub async fn repository_id_for_storage(storage: &Path) -> String {
    repository_id_for_storage_inner(storage)
        .await
        .unwrap_or_else(|_| "unavailable".to_string())
}

async fn repository_id() -> String {
    let Ok(storage) = crate::utils::util::try_get_storage_path(None) else {
        return "unavailable".to_string();
    };
    repository_id_for_storage(&storage).await
}

pub async fn serve<R, W>(reader: R, mut writer: W) -> std::io::Result<()>
where
    R: AsyncBufRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    let mut reader = reader;
    let mut session = Session::with_repository_id(repository_id().await);
    let (sender, mut incoming) = mpsc::channel(8);
    tokio::spawn(async move {
        loop {
            match read_line_capped(&mut reader, MAX_LINE_BYTES).await {
                Ok(None) => break,
                Ok(Some(line)) => {
                    if sender.send(Ok(line)).await.is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = sender.send(Err(error)).await;
                    break;
                }
            }
        }
    });
    let mut inflight: VecDeque<JoinHandle<Option<Value>>> = VecDeque::new();
    loop {
        if inflight.len() >= 4 {
            write_finished(&mut writer, &mut inflight).await?;
            continue;
        }
        tokio::select! {
            biased;
            finished = join_front(inflight.front_mut()), if !inflight.is_empty() => {
                inflight.pop_front();
                if let Some(response) = finished? {
                    let response = super::output::finalize_tool_result(response);
                    write_message(&mut writer, &response).await?;
                }
            }
            next = incoming.recv() => {
                let Some(next) = next else {
                    break;
                };
                let line = next?;
                if super::exec::line_starts_read_tool(&line) {
                    let mut local = session.duplicate();
                    inflight.push_back(tokio::task::spawn_blocking(move || {
                        protocol::handle_message(&mut local, &line)
                    }));
                } else {
                    while !inflight.is_empty() {
                        write_finished(&mut writer, &mut inflight).await?;
                    }
                    if let Some(response) = protocol::handle_message(&mut session, &line) {
                        let response = super::output::finalize_tool_result(response);
                        write_message(&mut writer, &response).await?;
                    }
                }
            }
        }
    }
    while !inflight.is_empty() {
        write_finished(&mut writer, &mut inflight).await?;
    }
    writer.flush().await?;
    Ok(())
}

async fn join_front(
    handle: Option<&mut JoinHandle<Option<Value>>>,
) -> std::io::Result<Option<Value>> {
    match handle {
        Some(handle) => handle.await.map_err(|_| {
            std::io::Error::other("MCP read tool task failed before it produced a response")
        }),
        None => Ok(None),
    }
}

async fn write_finished<W: AsyncWrite + Unpin>(
    writer: &mut W,
    inflight: &mut VecDeque<JoinHandle<Option<Value>>>,
) -> std::io::Result<()> {
    let Some(handle) = inflight.pop_front() else {
        return Ok(());
    };
    let response = handle.await.map_err(|_| {
        std::io::Error::other("MCP read tool task failed before it produced a response")
    })?;
    if let Some(response) = response {
        let response = super::output::finalize_tool_result(response);
        write_message(writer, &response).await?;
    }
    Ok(())
}

async fn write_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &Value,
) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(message).map_err(|error| {
        std::io::Error::new(
            ErrorKind::InvalidData,
            format!("failed to encode MCP response: {error}"),
        )
    })?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

/// Read one logical line. `Ok(None)` is EOF with no pending bytes.
///
/// A line whose payload exceeds `cap` is discarded through the next newline
/// so the following request can still be served.
async fn read_line_capped<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    cap: usize,
) -> std::io::Result<Option<InboundLine>> {
    let mut buf = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if buf.is_empty() {
                return Ok(None);
            }
            return Ok(Some(if buf.len() > cap {
                InboundLine::TooLarge
            } else {
                InboundLine::Message(buf)
            }));
        }
        if let Some(index) = available.iter().position(|byte| *byte == b'\n') {
            if buf.len() + index > cap {
                reader.consume(index + 1);
                return Ok(Some(InboundLine::TooLarge));
            }
            buf.extend_from_slice(&available[..index]);
            reader.consume(index + 1);
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
            if buf.is_empty() {
                return Ok(Some(InboundLine::Empty));
            }
            return Ok(Some(InboundLine::Message(buf)));
        }
        let length = available.len();
        if buf.len() + length > cap {
            reader.consume(length);
            discard_until_newline(reader).await?;
            return Ok(Some(InboundLine::TooLarge));
        }
        buf.extend_from_slice(available);
        reader.consume(length);
    }
}

async fn discard_until_newline<R: AsyncBufRead + Unpin>(reader: &mut R) -> std::io::Result<()> {
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(());
        }
        if let Some(index) = available.iter().position(|byte| *byte == b'\n') {
            reader.consume(index + 1);
            return Ok(());
        }
        let length = available.len();
        reader.consume(length);
    }
}
