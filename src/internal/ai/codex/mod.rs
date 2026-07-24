//! # Codex Provider Agent（Codex 提供者代理）
//!
//! 本模块实现了 `libra code --provider=codex` 命令背后的 Codex 代理。
//! It implements the Codex provider agent used by `libra code --provider=codex`.
//!
//! ## 功能概述 / Overview
//!
//! Codex 代理通过 WebSocket 连接到本地运行的 Codex app-server，与其进行双向通信，
//! 从而驱动一个交互式编码会话（Interactive Coding Session）。
//! The agent connects to a locally running Codex app-server via WebSocket and drives
//! an interactive coding session by exchanging JSON-RPC-like messages.
//!
//! ## 架构 / Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────────────┐
//! │                        execute() — 主循环 / main loop                │
//! │                                                                      │
//! │  ┌─────────────────┐    mpsc::channel    ┌──────────────────────┐   │
//! │  │  reader task    │ ─── approval_tx ──► │  approval flow loop  │   │
//! │  │  (WebSocket RX) │                     │  (stdin prompt /     │   │
//! │  │                 │ ─── tx (outgoing) ─►│   auto-accept)       │   │
//! │  └─────────────────┘                     └──────────────────────┘   │
//! │           │                                                          │
//! │           ▼  mpsc::channel (outgoing messages)                       │
//! │  ┌─────────────────┐                                                 │
//! │  │  writer task    │ ──► WebSocket TX ──► Codex app-server           │
//! │  │  (WebSocket TX) │                                                 │
//! │  └─────────────────┘                                                 │
//! └──────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! - **reader task**：后台异步任务，持续从 WebSocket 读取 Codex 服务器推送的通知与响应，
//!   解析并更新共享的 `CodexSession` 状态，同时触发审批流（approval flow）。
//!   Background task that reads server-sent notifications/responses from the WebSocket,
//!   updates the shared `CodexSession`, and forwards approval requests to the main loop.
//!
//! - **writer task**：后台异步任务，从 `mpsc` 通道接收待发送的 JSON 字符串，
//!   通过 WebSocket 写入 Codex 服务器。
//!   Background task that dequeues outgoing JSON strings from an `mpsc` channel
//!   and sends them over the WebSocket.
//!
//! - **main loop / approval flow**：主循环处理用户输入（stdin）并驱动审批流，
//!   支持三种模式：`ask`（每次询问用户）、`accept`（自动接受）、`decline`（自动拒绝）。
//!   The main loop reads user input from stdin and handles approval requests;
//!   it supports `ask`, `accept`, and `decline` modes.
//!
//! ## 数据持久化 / Data Persistence
//!
//! 所有 AI 对象（Run、Task、Plan、PatchSet、Evidence 等）均通过 `LibraMcpServer`
//! 序列化为 JSON 并以 content-addressable 方式写入 `.libra/objects/` 目录，
//! 使用与 Git 对象存储相同的哈希寻址机制（`ObjectHash`）。
//! All AI objects (Run, Task, Plan, PatchSet, Evidence, etc.) are serialized to JSON
//! and written to `.libra/objects/` via `LibraMcpServer` using the same
//! content-addressable storage as Git objects (`ObjectHash`).
//!
//! 历史索引（history index）记录每个对象类型与 ID 对应的最新哈希，
//! 存储于 `.libra/libra.db`（SQLite）中，由 `HistoryManager` 管理。
//! A history index recording the latest hash per (object_type, object_id) is kept
//! in `.libra/libra.db` (SQLite) and managed by `HistoryManager`.
//!
//! ## 会话恢复 / Session Recovery
//!
//! 启动时，`HistoryReader::rebuild_view()` 从已持久化的对象重建 `CodexSession` 状态，
//! 包括线程（Thread）、运行（Run）、计划（Plan）、任务（Task）、补丁集（PatchSet）等。
//! On startup, `HistoryReader::rebuild_view()` reconstructs the `CodexSession` state
//! from persisted objects, including Thread, Run, Plan, Task, and PatchSet data.

pub mod history;
pub mod model;
pub mod protocol;
pub mod schema_v2;
pub mod schema_v2_generated;
pub mod types;
pub mod view;

use std::{
    collections::{BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, OnceLock},
};

use anyhow::anyhow;
use chrono::Utc;
use clap::Parser;
use diffy::create_patch;
use futures_util::{SinkExt, StreamExt};
use git_internal::hash::ObjectHash;
use history::{EventKind, HistoryReader, HistoryRecorder, HistoryWriter};
use model::{
    ContextFrameEvent, ContextSnapshot, DecisionEvent, EvidenceEvent, IntentEvent, IntentSnapshot,
    PatchSetSnapshot, PlanSnapshot, PlanStepEvent, PlanStepSnapshot, ProvenanceSnapshot, RunEvent,
    RunSnapshot, RunUsage, TaskEvent, TaskSnapshot, ToolInvocationEvent,
};
use protocol::MethodKind;
use schema_v2::*;
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tokio_tungstenite::{connect_async, tungstenite::Message};
pub use types::*;
use walkdir::WalkDir;

use crate::{
    internal::{
        ai::{
            history::HistoryManager,
            mcp::server::LibraMcpServer,
            runtime::PlanningPromptBuilder,
            web::code_ui::{
                CodeUiApplyToFuture, CodeUiCapabilities, CodeUiCommandAdapter, CodeUiEventType,
                CodeUiInitialController, CodeUiInteractionKind, CodeUiInteractionOption,
                CodeUiInteractionRequest, CodeUiInteractionResponse, CodeUiInteractionStatus,
                CodeUiPatchChange, CodeUiPatchsetSnapshot, CodeUiPlanSnapshot, CodeUiPlanStep,
                CodeUiProviderAdapter, CodeUiProviderInfo, CodeUiReadModel, CodeUiRuntimeHandle,
                CodeUiSession, CodeUiSessionSnapshot, CodeUiSessionStatus, CodeUiTaskSnapshot,
                CodeUiToolCallSnapshot, CodeUiTranscriptEntry, CodeUiTranscriptEntryKind,
                initial_snapshot,
            },
        },
        db,
    },
    utils::{
        client_storage::ClientStorage, storage::Storage, storage_ext::StorageExt,
        util::try_get_storage_path,
    },
};

// ---------------------------------------------------------------------------
// 常量 / Constants
// ---------------------------------------------------------------------------

/// Codex app-server 的默认 WebSocket 连接地址。
/// Default WebSocket URL of the locally running Codex app-server.
const CODEX_WS_URL: &str = "ws://127.0.0.1:8080";

/// 全局异步互斥锁，用于保证向历史索引追加哈希时的串行化，防止并发写入产生重复条目。
/// A process-wide async mutex that serialises history-index appends so that
/// concurrent `store_to_mcp` calls never produce duplicate index entries.
static HISTORY_APPEND_LOCK: OnceLock<AsyncMutex<()>> = OnceLock::new();

/// 工作区快照中单个文件内容的最大字节数（256 KiB）。超出此限制的文件将被跳过。
/// Maximum byte size of a single file included in a workspace snapshot (256 KiB).
/// Files larger than this are skipped during `capture_workspace_snapshot`.
const COMMAND_DIFF_MAX_FILE_SIZE: u64 = 256 * 1024;

/// 工作区快照中最多捕获的文件数量（512 个）。超出此限制后停止遍历。
/// Maximum number of files captured in a single workspace snapshot (512).
/// Traversal stops after this many files have been added to the snapshot.
const COMMAND_DIFF_MAX_FILES: usize = 512;

// ---------------------------------------------------------------------------
// 工具函数 / Helper Functions
// ---------------------------------------------------------------------------

/// Returns `true` for high-frequency Codex notifications whose only effect is
/// to append a streaming delta to in-memory state.
///
/// Delta events fire at every streamed token, so publishing a fresh
/// `CodeUiSession` snapshot for each one causes a deep clone of the entire
/// `CodexSession` per token — a major source of latency under fast-streaming
/// models. Skipping the publish for these methods lets the next non-delta
/// event (item completion, turn completion, approval request) flush the
/// accumulated state to subscribers in one shot.
fn is_streaming_delta_method(method: MethodKind) -> bool {
    matches!(
        method,
        MethodKind::AgentMessageDelta
            | MethodKind::CommandExecutionOutputDelta
            | MethodKind::FileChangeOutputDelta
            | MethodKind::PlanDelta
    )
}

/// Truncates a string for safe inclusion in tracing logs.
///
/// Codex emits long reasoning chunks, agent messages, file diffs, and command
/// output that would otherwise dominate the log file and slow down tracing-fmt
/// formatting. This helper bounds each entry to `max_chars` Unicode scalar
/// values and replaces non-display-friendly whitespace (newlines, carriage
/// returns) with literal `\n` / `\r` so the entry stays on a single log line.
fn truncate_for_log(input: &str, max_chars: usize) -> String {
    let escaped: String = input
        .chars()
        .map(|c| match c {
            '\n' => "\\n".to_string(),
            '\r' => "\\r".to_string(),
            '\t' => "\\t".to_string(),
            other => other.to_string(),
        })
        .collect();
    let mut iter = escaped.char_indices();
    let cutoff = iter.nth(max_chars).map(|(idx, _)| idx);
    match cutoff {
        Some(idx) => format!("{}…(truncated)", &escaped[..idx]),
        None => escaped,
    }
}

/// 安全地获取 `Mutex` 锁，若锁已中毒（poisoned）则打印警告并返回 `None`。
///
/// 标准库的 `Mutex::lock()` 在持有锁的线程 panic 后会返回 `PoisonError`。
/// 本函数将该错误转化为控制台警告，避免调用方 unwrap() 导致连锁 panic。
///
/// Safe mutex locking wrapper: returns `None` and prints a warning when the
/// mutex is poisoned (i.e., the previous lock-holder panicked), preventing
/// cascading panics from unwrap() calls throughout the codebase.
///
/// # Arguments
///
/// - `mutex`   — 需要加锁的 `Arc<Mutex<T>>`。The mutex to lock.
/// - `context` — 用于日志的上下文描述字符串。Human-readable context string for
///   the warning.
fn lock_or_warn<'a, T>(mutex: &'a Arc<Mutex<T>>, context: &str) -> Option<MutexGuard<'a, T>> {
    match mutex.lock() {
        Ok(guard) => Some(guard),
        Err(e) => {
            tracing::warn!(
                target: "libra::internal::ai::codex",
                context,
                error = %e,
                "failed to lock mutex"
            );
            None
        }
    }
}

/// 返回 `HISTORY_APPEND_LOCK` 的全局单例引用。
///
/// 使用 `OnceLock` 实现惰性初始化，确保进程生命周期内只存在一个异步互斥锁实例。
/// Returns a reference to the global `HISTORY_APPEND_LOCK` singleton,
/// initialising it lazily via `OnceLock` on first call.
fn history_append_lock() -> &'static AsyncMutex<()> {
    HISTORY_APPEND_LOCK.get_or_init(|| AsyncMutex::new(()))
}

/// 将流式文件变更（streaming changes）合并到已完成的补丁集变更（completed changes）中。
///
/// Codex 服务器在流式传输期间（streaming）可能会先推送包含 `diff` 内容的文件变更，
/// 但在最终的 `completed` 事件中只给出涉及的文件路径而省略 `diff` 文本。
/// 本函数将二者合并，确保最终补丁集始终包含 diff 内容：
/// - 若 `completed_changes` 中存在非空 diff，则直接使用 `completed_changes`；
/// - 否则将 `existing_changes`（流式 diff）中未被 `completed_changes` 覆盖的条目追加进去。
///
/// Merges streaming file changes into the final completed-patchset changes.
/// If the completed payload omits diff text (only lists touched paths), the
/// previously captured streaming diffs are preserved so the stored PatchSet
/// always contains actual patch content.
///
/// # Arguments
/// * `existing_changes` — 流式阶段已捕获的文件变更列表（含 diff）。
/// * `completed_changes` — 最终 `completed` 事件中携带的文件变更列表。
fn merge_patchset_changes(
    existing_changes: &[FileChange],
    completed_changes: &[FileChange],
) -> Vec<FileChange> {
    if completed_changes.is_empty() {
        return existing_changes.to_vec();
    }

    let mut merged = completed_changes.to_vec();

    // Preserve any previously captured streaming diff if the completed payload
    // only summarizes touched files and omits the actual patch text.
    let has_completed_diff = merged.iter().any(|change| !change.diff.is_empty());
    if !has_completed_diff {
        for existing_change in existing_changes {
            if merged
                .iter()
                .all(|change| change.path != existing_change.path)
            {
                merged.push(existing_change.clone());
            }
        }
    }

    merged
}

/// 将来自 Codex 服务器的补丁状态字符串映射为 `PatchStatus` 枚举值。
///
/// Maps a raw patch-status string received from the Codex server to the
/// typed `PatchStatus` enum. Unrecognised strings default to `Pending`.
fn patch_status_from_str(status: &str) -> PatchStatus {
    match status {
        "in_progress" | "inProgress" | "started" => PatchStatus::InProgress,
        "completed" => PatchStatus::Completed,
        "failed" => PatchStatus::Failed,
        "declined" => PatchStatus::Declined,
        _ => PatchStatus::Pending,
    }
}

/// 从 JSON 数组（`[{path, diff, change_type}, ...]`）解析文件变更列表。
///
/// 当 Codex 服务器以数组格式（Array）返回文件变更时使用此函数。
/// 兼容多种字段命名：`change_type`、`changeType`、`kind.type`。
///
/// Parses a list of `FileChange` from a JSON array where each element is an
/// object with `path`, `diff`, and a change-type field (supports multiple
/// naming conventions: `change_type`, `changeType`, `kind.type`).
fn parse_patchset_changes_from_array(changes: Option<&serde_json::Value>) -> Vec<FileChange> {
    changes
        .and_then(|value| value.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|change| {
                    let path = change.get("path")?.as_str()?.to_string();
                    let diff = change
                        .get("diff")
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .to_string();
                    let change_type = change
                        .get("change_type")
                        .or_else(|| change.get("changeType"))
                        .or_else(|| change.get("kind").and_then(|k| k.get("type")))
                        .and_then(|c| c.as_str())
                        .unwrap_or("update")
                        .to_string();
                    Some(FileChange {
                        path,
                        diff,
                        change_type,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 从 JSON 对象（`{path: {unified_diff, type}, ...}`）解析文件变更列表。
///
/// 当 Codex 服务器以 Map 格式（Object）返回文件变更时使用此函数，
/// 键为文件路径，值为包含 diff 内容的对象。
/// 兼容多种 diff 字段命名：`unified_diff`、`unifiedDiff`、`diff`、`content`。
///
/// Parses a list of `FileChange` from a JSON object where keys are file paths
/// and values are objects containing diff content. Supports multiple diff-field
/// names: `unified_diff`, `unifiedDiff`, `diff`, `content`.
fn parse_patchset_changes_from_map(changes: Option<&serde_json::Value>) -> Vec<FileChange> {
    changes
        .and_then(|value| value.as_object())
        .map(|map| {
            map.iter()
                .map(|(path, change)| FileChange {
                    path: path.clone(),
                    diff: change
                        .get("unified_diff")
                        .or_else(|| change.get("unifiedDiff"))
                        .or_else(|| change.get("diff"))
                        .or_else(|| change.get("content"))
                        .and_then(|value| value.as_str())
                        .unwrap_or("")
                        .to_string(),
                    change_type: change
                        .get("type")
                        .and_then(|value| value.as_str())
                        .unwrap_or("update")
                        .to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 将补丁集快照（PatchSetSnapshot）及其证据事件（EvidenceEvent）异步写入 MCP 存储。
///
/// 本函数在后台 `tokio::spawn` 中同时完成以下四项工作：
/// 1. 将 `PatchSet` 对象本身存入 `.libra/objects/`（通过 `store_to_mcp`）；
/// 2. 向 `HistoryRecorder` 追加 `ToolInvocationStatus` 事件；
/// 3. 将 `PatchSetSnapshot`（含文件变更列表）写入历史记录；
/// 4. 将 `EvidenceEvent`（记录受影响文件数量）写入历史记录。
///
/// Asynchronously persists a `PatchSet` and its associated evidence to MCP storage.
/// Spawns a background task that:
/// 1. Stores the raw `PatchSet` object to `.libra/objects/`.
/// 2. Appends a `ToolInvocationStatus` event to the `HistoryRecorder`.
/// 3. Writes a `PatchSetSnapshot` (with file-change list) to history.
/// 4. Writes an `EvidenceEvent` (recording touched-file counts) to history.
///
/// # Arguments
/// * `mcp_server`     — MCP 存储服务实例（`Arc` 共享）。Shared MCP server.
/// * `history`        — 历史事件记录器。History event recorder.
/// * `history_writer` — 历史对象写入器。History object writer.
/// * `patchset`       — 需要持久化的补丁集。The patchset to persist.
/// * `status`         — 补丁集当前状态字符串（如 `"completed"`）。Status string.
/// * `debug_mode`     — 是否输出调试日志。Whether to emit debug logs.
fn persist_patchset_snapshot_and_evidence(
    mcp_server: Arc<LibraMcpServer>,
    history: Arc<HistoryRecorder>,
    history_writer: Arc<HistoryWriter>,
    patchset: PatchSet,
    status: String,
    debug_mode: bool,
) {
    let patchset_id = patchset.id.clone();
    let files = patchset.changes.len();
    let touched_files: Vec<String> = patchset
        .changes
        .iter()
        .map(|change| change.path.clone())
        .collect();
    let patchset_snapshot = PatchSetSnapshot {
        id: patchset_id.clone(),
        run_id: patchset.run_id.clone(),
        thread_id: patchset.thread_id.clone(),
        created_at: Utc::now(),
        status: patchset.status.clone(),
        changes: patchset.changes.clone(),
    };
    let evidence = EvidenceEvent {
        id: format!("evidence_{}", patchset_id),
        run_id: patchset.run_id.clone(),
        patchset_id: Some(patchset_id.clone()),
        at: Utc::now(),
        kind: "patchset".to_string(),
        data: serde_json::json!({
            "files": files,
            "touched_files": touched_files,
        }),
    };

    ClientStorage::spawn_background_index_work(async move {
        store_to_mcp(&mcp_server, "patchset", &patchset_id, &patchset, debug_mode).await;
        history
            .event(
                history::EventKind::ToolInvocationStatus,
                &patchset_id,
                status,
                serde_json::json!({ "files": files }),
            )
            .await;
        history_writer
            .write("patchset_snapshot", &patchset_id, &patchset_snapshot)
            .await;
        history_writer
            .write("evidence", &evidence.id, &evidence)
            .await;
    });
}

/// 判断给定的相对路径是否应跳过（不纳入工作区快照）。
///
/// 跳过条件：路径的任意一级组件名称属于以下目录之一：
/// `.git`、`.libra`、`node_modules`、`target`、`dist`、`build`。
///
/// Returns `true` if the path should be excluded from workspace snapshots.
/// Skips paths whose components include well-known non-source directories:
/// `.git`, `.libra`, `node_modules`, `target`, `dist`, `build`.
fn should_skip_diff_path(relative_path: &Path) -> bool {
    relative_path.components().any(|component| {
        let name = component.as_os_str().to_string_lossy();
        matches!(
            name.as_ref(),
            ".git" | ".libra" | "node_modules" | "target" | "dist" | "build"
        )
    })
}

/// 快速判断字节序列是否可能是文本文件（UTF-8 且不含空字节）。
///
/// 用于在 `capture_workspace_snapshot` 中过滤二进制文件，避免在 diff 中处理无意义的内容。
///
/// Heuristic check: returns `true` when the byte slice contains no null bytes
/// and is valid UTF-8, indicating it is likely a text file rather than binary.
fn is_probably_text(bytes: &[u8]) -> bool {
    !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok()
}

/// 对工作目录进行文件系统快照，返回 `{相对路径 -> 文件内容}` 映射。
///
/// 遍历 `cwd` 下的所有文件，跳过以下情况：
/// - 目录属于排除列表（`.git`、`node_modules` 等，见 `should_skip_diff_path`）；
/// - 文件大小超过 `COMMAND_DIFF_MAX_FILE_SIZE`（256 KiB）；
/// - 文件内容为二进制（见 `is_probably_text`）；
/// - 快照文件数量已达 `COMMAND_DIFF_MAX_FILES`（512）上限。
///
/// 配合 `build_file_changes_from_snapshots` 使用，可以在 AI 命令执行前后各取一次快照，
/// 再对比两次快照生成 unified diff，作为补丁集（PatchSet）的 diff 内容来源。
///
/// Captures a before/after workspace snapshot as a `{relative_path -> content}`
/// map. Used together with `build_file_changes_from_snapshots` to compute diffs
/// when Codex does not emit structured `fileChange` events (e.g., applies patches
/// via shell commands).
fn capture_workspace_snapshot(cwd: &Path) -> HashMap<String, String> {
    let mut snapshot = HashMap::new();
    if !cwd.exists() || !cwd.is_dir() {
        return snapshot;
    }

    for entry in WalkDir::new(cwd).into_iter().filter_map(Result::ok) {
        let path = entry.path();
        if !entry.file_type().is_file() {
            continue;
        }

        let Ok(relative_path) = path.strip_prefix(cwd) else {
            continue;
        };
        if relative_path.as_os_str().is_empty() || should_skip_diff_path(relative_path) {
            continue;
        }

        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.len() > COMMAND_DIFF_MAX_FILE_SIZE {
            continue;
        }

        let Ok(bytes) = fs::read(path) else {
            continue;
        };
        if !is_probably_text(&bytes) {
            continue;
        }

        let Ok(content) = String::from_utf8(bytes) else {
            continue;
        };
        let relative_key = relative_path.to_string_lossy().replace('\\', "/");
        snapshot.insert(relative_key, content);

        if snapshot.len() >= COMMAND_DIFF_MAX_FILES {
            break;
        }
    }

    snapshot
}

/// 为单个文件生成 unified diff 字符串（before → after）。
///
/// 使用 `diffy::create_patch` 生成 unified format diff，
/// 空字符串表示文件不存在（用于新建/删除场景）。
///
/// Generates a unified-format diff string for a single file transition.
/// An empty `before` or `after` string represents a non-existent file
/// (used for file creation and deletion cases).
fn render_snapshot_diff(before: &str, after: &str) -> String {
    create_patch(before, after).to_string()
}

/// 通过比较前后两次工作区快照，构建文件变更列表（`Vec<FileChange>`）。
///
/// 遍历两次快照中出现的所有路径，按以下规则分类：
/// - 仅出现在 `after`：`change_type = "add"`；
/// - 仅出现在 `before`：`change_type = "delete"`；
/// - 两次均出现但内容不同：`change_type = "update"`；
/// - 两次内容相同：跳过（无变更）。
///
/// Builds a `Vec<FileChange>` by diffing two workspace snapshots.
/// Files present only in `after` are additions, files only in `before` are
/// deletions, and files present in both with differing content are updates.
/// Unchanged files are omitted.
///
/// # Arguments
/// * `before` — AI 命令执行前的快照。Snapshot taken before execution.
/// * `after`  — AI 命令执行后的快照。Snapshot taken after execution.
fn build_file_changes_from_snapshots(
    before: &HashMap<String, String>,
    after: &HashMap<String, String>,
) -> Vec<FileChange> {
    let all_paths: BTreeSet<String> = before.keys().chain(after.keys()).cloned().collect();

    let mut changes = Vec::new();
    for path in all_paths {
        match (before.get(&path), after.get(&path)) {
            (None, Some(after_content)) => changes.push(FileChange {
                path,
                diff: render_snapshot_diff("", after_content),
                change_type: "add".to_string(),
            }),
            (Some(before_content), None) => changes.push(FileChange {
                path,
                diff: render_snapshot_diff(before_content, ""),
                change_type: "delete".to_string(),
            }),
            (Some(before_content), Some(after_content)) if before_content != after_content => {
                changes.push(FileChange {
                    path,
                    diff: render_snapshot_diff(before_content, after_content),
                    change_type: "update".to_string(),
                });
            }
            _ => {}
        }
    }

    changes
}

/// 获取指定线程（thread）中创建时间最晚的 Intent ID。
///
/// 用于在新 turn 开始时关联当前最新的用户意图（Intent），
/// 可通过 `excluding_id` 排除某个特定的 Intent ID（避免自引用）。
///
/// Returns the ID of the most recently created `Intent` in the given thread,
/// optionally excluding a specific ID (to avoid self-referential linking).
///
/// # Arguments
/// * `session`      — 当前会话状态。Current session state.
/// * `thread_id`    — 目标线程 ID。Target thread ID.
/// * `excluding_id` — 需要排除的 Intent ID（可选）。Optional ID to exclude.
fn latest_thread_intent_id(
    session: &CodexSession,
    thread_id: &str,
    excluding_id: Option<&str>,
) -> Option<String> {
    session
        .intents
        .iter()
        .filter(|intent| {
            intent.thread_id == thread_id
                && excluding_id.is_none_or(|exclude_id| intent.id != exclude_id)
        })
        .max_by_key(|intent| intent.created_at)
        .map(|intent| intent.id.clone())
}

/// 将 `ToolInvocation` 转换为可持久化的 `ToolInvocationEvent` 对象。
///
/// `ToolInvocationEvent` 用于历史记录，包含工具名称、服务器名称、
/// 调用状态、参数、结果、错误信息及执行耗时（毫秒）。
///
/// Converts a `ToolInvocation` (in-memory session state) into a
/// `ToolInvocationEvent` suitable for writing to the history store.
fn build_tool_invocation_event(invocation: &ToolInvocation) -> ToolInvocationEvent {
    ToolInvocationEvent {
        id: invocation.id.clone(),
        run_id: invocation.run_id.clone(),
        thread_id: invocation.thread_id.clone(),
        tool: invocation.tool_name.clone(),
        server: invocation.server.clone(),
        status: invocation.status.to_string(),
        at: Utc::now(),
        payload: serde_json::json!({
            "arguments": invocation.arguments.clone(),
            "result": invocation.result.clone(),
            "error": invocation.error.clone(),
            "duration_ms": invocation.duration_ms,
        }),
    }
}

/// 生成工具调用事件对象的唯一 ID。
///
/// 格式为 `tool_invocation_event_{invocation_id}_{status}_{timestamp_ms}`，
/// 其中时间戳（毫秒）确保同一调用在不同状态下产生不同的存储键，
/// 避免覆盖先前写入的事件对象。
///
/// Generates a unique object ID for a `ToolInvocationEvent`. The millisecond
/// timestamp suffix ensures that events for the same invocation at different
/// status transitions produce distinct storage keys.
fn next_tool_invocation_event_object_id(invocation_id: &str, status: &str) -> String {
    format!(
        "tool_invocation_event_{}_{}_{}",
        invocation_id,
        status,
        Utc::now().timestamp_millis()
    )
}

// ---------------------------------------------------------------------------
// 状态映射函数 / Status Mapping Functions
// ---------------------------------------------------------------------------

/// 将来自 Codex 事件的计划状态字符串映射为 `PlanStatus` 枚举值。
///
/// 支持 `"completed"`、`"in_progress"`（含 camelCase `"inProgress"`）。
/// 其余字符串统一映射为 `PlanStatus::Pending`。
///
/// Maps a raw plan-status string from a Codex event to the typed `PlanStatus`
/// enum. Unrecognised strings default to `Pending`.
fn plan_status_from_event(status: &str) -> PlanStatus {
    match status {
        "completed" => PlanStatus::Completed,
        "in_progress" | "inProgress" => PlanStatus::InProgress,
        _ => PlanStatus::Pending,
    }
}

/// 将来自 Codex 事件的任务状态字符串映射为 `TaskStatus` 枚举值。
///
/// 支持 `"completed"`、`"failed"`、`"in_progress"`。
/// 其余字符串统一映射为 `TaskStatus::Pending`。
///
/// Maps a raw task-status string from a Codex event to the typed `TaskStatus`
/// enum. Unrecognised strings default to `Pending`.
fn task_status_from_event(status: &str) -> TaskStatus {
    match status {
        "completed" => TaskStatus::Completed,
        "failed" => TaskStatus::Failed,
        "in_progress" => TaskStatus::InProgress,
        _ => TaskStatus::Pending,
    }
}

/// 将来自 Codex 事件的运行状态字符串映射为 `RunStatus` 枚举值。
///
/// 支持 `"completed"`、`"failed"`、`"in_progress"`。
/// 其余字符串统一映射为 `RunStatus::Pending`。
///
/// Maps a raw run-status string from a Codex event to the typed `RunStatus`
/// enum. Unrecognised strings default to `Pending`.
fn run_status_from_event(status: &str) -> RunStatus {
    match status {
        "completed" => RunStatus::Completed,
        "failed" => RunStatus::Failed,
        "in_progress" => RunStatus::InProgress,
        _ => RunStatus::Pending,
    }
}

// ---------------------------------------------------------------------------
// 历史索引管理 / History Index Management
// ---------------------------------------------------------------------------

/// 在历史索引中追加对象哈希（仅当哈希与已存储值不同时追加，实现去重写入）。
///
/// 流程：
/// 1. 若 `mcp_server` 未配置 `intent_history_manager`，直接返回 `Ok(())`；
/// 2. 获取全局 `HISTORY_APPEND_LOCK`，防止并发条件竞争；
/// 3. 查询历史索引中 `(object_type, object_id)` 的现有哈希；
/// 4. 仅当新哈希与现有哈希不同（或不存在）时，才追加新哈希。
///
/// Appends an object hash to the history index only when the hash differs from
/// the currently stored value, providing idempotent (deduplicated) writes.
///
/// Steps:
/// 1. Return `Ok(())` early if no `intent_history_manager` is configured.
/// 2. Acquire `HISTORY_APPEND_LOCK` to prevent concurrent race conditions.
/// 3. Look up the existing hash for `(object_type, object_id)`.
/// 4. Append the new hash only when it differs from (or is absent from) the index.
///
/// # Arguments
/// * `mcp_server`   — MCP 服务器实例（提供 `intent_history_manager`）。
/// * `object_type`  — 对象类型名称（如 `"patchset"`、`"run"`）。
/// * `object_id`    — 对象的业务唯一 ID。
/// * `hash`         — 对象序列化后的内容哈希（`ObjectHash`）。
async fn append_history_hash_if_changed(
    mcp_server: &Arc<LibraMcpServer>,
    object_type: &str,
    object_id: &str,
    hash: ObjectHash,
) -> Result<(), String> {
    let Some(history) = &mcp_server.intent_history_manager else {
        return Ok(());
    };

    let _guard = history_append_lock().lock().await;
    let should_append = match history.get_object_hash(object_type, object_id).await {
        Ok(Some(existing)) => existing != hash,
        Ok(None) => true,
        Err(e) => {
            return Err(format!(
                "Failed to check history for {object_type}/{object_id}: {e}"
            ));
        }
    };

    if should_append {
        history
            .append(object_type, object_id, hash)
            .await
            .map_err(|e| format!("Failed to append {object_type}/{object_id} to history: {e}"))?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// JSON 参数提取辅助函数 / JSON Parameter Extraction Helpers
// ---------------------------------------------------------------------------

/// 从 Codex 通知的 `params` JSON 对象中提取线程 ID（thread ID）。
///
/// Codex 服务器在不同事件中使用多种字段命名惯例，本函数按优先级依次尝试：
/// `params.thread.id` → `params.thread.threadId` → `params.thread.thread_id`
/// → `params.threadId` → `params.thread_id` → `session.thread.id`（兜底）。
///
/// Extracts the thread ID from a Codex notification `params` object.
/// Tries multiple field-name conventions in priority order before falling back
/// to the current `session.thread.id`.
///
/// # Arguments
/// * `params`  — 通知消息中的 `params` JSON 值。
/// * `session` — 当前会话（用于兜底，可选）。Optional session for fallback.
fn extract_thread_id(params: &serde_json::Value, session: Option<&CodexSession>) -> String {
    params
        .get("thread")
        .and_then(|thread| {
            thread
                .get("id")
                .or_else(|| thread.get("threadId"))
                .or_else(|| thread.get("thread_id"))
        })
        .and_then(|value| value.as_str())
        .map(String::from)
        .or_else(|| {
            params
                .get("threadId")
                .or_else(|| params.get("thread_id"))
                .and_then(|value| value.as_str())
                .map(String::from)
        })
        .or_else(|| {
            session.and_then(|session| {
                if session.thread.id.is_empty() {
                    None
                } else {
                    Some(session.thread.id.clone())
                }
            })
        })
        .unwrap_or_default()
}

/// 从 Codex 通知的 `params` JSON 对象中提取任务 ID（task ID）。
///
/// 按优先级依次尝试：`params.taskId` → `params.task_id` → `params.id`
/// → `params.task.id` → `params.task.taskId`。
/// 若所有字段均不存在则返回空字符串。
///
/// Extracts the task ID from a Codex notification `params` object.
/// Tries `taskId`, `task_id`, `id`, `task.id`, `task.taskId` in order.
/// Returns an empty string if none of the fields are present.
fn extract_task_id(params: &serde_json::Value) -> String {
    params
        .get("taskId")
        .or_else(|| params.get("task_id"))
        .or_else(|| params.get("id"))
        .or_else(|| params.get("task").and_then(|task| task.get("id")))
        .or_else(|| params.get("task").and_then(|task| task.get("taskId")))
        .and_then(|value| value.as_str())
        .map(String::from)
        .unwrap_or_default()
}

/// 从 Codex 通知的 `params` JSON 对象中提取任务名称（task name）。
///
/// 按优先级依次尝试：`params.taskName` → `params.task_name` → `params.name`
/// → `params.title` → `params.task.name` → `params.task.title`。
/// 若所有字段均不存在则返回空字符串。
///
/// Extracts the task name from a Codex notification `params` object.
/// Tries `taskName`, `task_name`, `name`, `title`, `task.name`, `task.title`
/// in order. Returns an empty string if none are present.
fn extract_task_name(params: &serde_json::Value) -> String {
    params
        .get("taskName")
        .or_else(|| params.get("task_name"))
        .or_else(|| params.get("name"))
        .or_else(|| params.get("title"))
        .or_else(|| params.get("task").and_then(|task| task.get("name")))
        .or_else(|| params.get("task").and_then(|task| task.get("title")))
        .and_then(|value| value.as_str())
        .map(String::from)
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// 计划步骤与聚合逻辑 / Plan Step & Aggregation Helpers
// ---------------------------------------------------------------------------

/// 将计划步骤的状态字符串规范化为固定的内部表示。
///
/// 将 camelCase 变体（如 `"inProgress"`）统一映射为 snake_case，
/// 并将未知字符串映射为 `"pending"`。
///
/// Normalises a plan-step status string to a canonical internal representation.
/// Maps camelCase variants (`"inProgress"`) to snake_case and defaults
/// unknown values to `"pending"`.
fn normalize_plan_step_status(status: &str) -> &'static str {
    match status {
        "completed" => "completed",
        "failed" => "failed",
        "in_progress" | "inProgress" => "in_progress",
        _ => "pending",
    }
}

/// 将文本截断至最多 `max_chars` 个 Unicode 字符（用于 CLI 显示）。
///
/// 返回 `(截断后的字符串, 是否发生了截断)`。
/// 截断在 Unicode 字符边界处进行，不会产生无效 UTF-8 序列。
///
/// Truncates `text` to at most `max_chars` Unicode scalar values for display.
/// Returns `(truncated_string, was_truncated)`. Truncation occurs on Unicode
/// character boundaries so the result is always valid UTF-8.
fn truncate_for_display(text: &str, max_chars: usize) -> (String, bool) {
    match text.char_indices().nth(max_chars) {
        Some((idx, _)) => (text[..idx].to_string(), true),
        None => (text.to_string(), false),
    }
}

/// 将计划步骤（plan step）的状态字符串映射为 `TaskStatus` 枚举值。
///
/// 先调用 `normalize_plan_step_status` 将状态规范化，再映射为枚举。
/// 用于在计划步骤更新时同步更新对应任务的状态。
///
/// Maps a plan-step status string to `TaskStatus` by first normalising it
/// via `normalize_plan_step_status`, then converting to the enum variant.
fn task_status_from_plan_step(status: &str) -> TaskStatus {
    match normalize_plan_step_status(status) {
        "completed" => TaskStatus::Completed,
        "failed" => TaskStatus::Failed,
        "in_progress" => TaskStatus::InProgress,
        _ => TaskStatus::Pending,
    }
}

/// 根据所有计划步骤的状态聚合出整体计划状态（`PlanStatus`）。
///
/// 聚合规则（按优先级）：
/// 1. 若任意步骤处于 `"in_progress"`，整体状态为 `InProgress`；
/// 2. 若列表非空且所有步骤均为 `"completed"`，整体状态为 `Completed`；
/// 3. 否则为 `Pending`（包括列表为空的情况）。
///
/// Aggregates the overall `PlanStatus` from a slice of plan steps.
/// Priority: any `in_progress` step → `InProgress`; all `completed` → `Completed`;
/// otherwise (or empty) → `Pending`.
fn aggregate_plan_status(plan_steps: &[TurnPlanStep]) -> PlanStatus {
    if plan_steps
        .iter()
        .any(|step| normalize_plan_step_status(&step.status) == "in_progress")
    {
        PlanStatus::InProgress
    } else if !plan_steps.is_empty()
        && plan_steps
            .iter()
            .all(|step| normalize_plan_step_status(&step.status) == "completed")
    {
        PlanStatus::Completed
    } else {
        PlanStatus::Pending
    }
}

/// 将计划的说明文字（explanation）与各步骤文本（plan steps）拼接为单一文本字符串。
///
/// 拼接规则：
/// - 若 `explanation` 非空且步骤列表也非空：`"{explanation}\n{步骤1}\n{步骤2}\n..."`
/// - 若 `explanation` 非空但步骤列表为空：直接返回 `explanation`
/// - 若 `explanation` 为空：返回步骤文本的换行拼接
///
/// 步骤文本会先 `trim()` 并过滤掉空白行。
fn build_plan_text(explanation: Option<&String>, plan_steps: &[TurnPlanStep]) -> String {
    let lines: Vec<&str> = plan_steps
        .iter()
        .map(|step| step.step.trim())
        .filter(|step| !step.is_empty())
        .collect();

    match explanation
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        Some(explanation) if lines.is_empty() => explanation.to_string(),
        Some(explanation) => format!("{explanation}\n{}", lines.join("\n")),
        None => lines.join("\n"),
    }
}

// ---------------------------------------------------------------------------
// MCP 服务器初始化 / MCP Server Initialization
// ---------------------------------------------------------------------------

/// 为 Codex 代理初始化 MCP（Model Context Protocol）服务器实例。
///
/// 初始化流程：
/// 1. 通过 `try_get_storage_path` 定位 `.libra/` 目录（若失败则退回到 `{cwd}/.libra/`）；
/// 2. 创建 `.libra/objects/` 目录（若创建失败则以只读模式运行）；
/// 3. 建立 SQLite 数据库连接（`libra.db`）（若失败则以只读模式运行）；
/// 4. 初始化 `LocalStorage`（content-addressable 对象存储）；
/// 5. 创建 `HistoryManager`（负责维护历史索引）；
/// 6. 返回包含上述组件的 `Arc<LibraMcpServer>`。
///
/// Initialises the `LibraMcpServer` used by the Codex agent for data persistence.
///
/// The function:
/// 1. Resolves the `.libra/` storage directory via `try_get_storage_path`.
/// 2. Creates the `objects/` sub-directory (falls back to read-only on failure).
/// 3. Establishes a SQLite connection to `libra.db` (falls back to read-only).
/// 4. Sets up `LocalStorage` (content-addressable object store).
/// 5. Creates `HistoryManager` (manages the history index in SQLite).
/// 6. Returns a fully initialised `Arc<LibraMcpServer>`.
///
/// # Arguments
/// * `working_dir` — 代理的工作目录，用于定位 `.libra/` 存储路径。
pub async fn init_mcp_server(working_dir: &Path) -> Arc<LibraMcpServer> {
    let storage_dir = try_get_storage_path(Some(working_dir.to_path_buf())).unwrap_or_else(|error| {
        // Part C §C.4.1: annotate the degrade fallback so a phantom storage root
        // (a linked worktree with a broken `commondir`) is diagnosable rather
        // than silently routing db/objects at `<working_dir>/.libra`.
        tracing::warn!(
            working_dir = %working_dir.display(),
            %error,
            "storage-root resolution failed for the MCP server; falling back to <working_dir>/.libra — run `libra worktree repair <worktree-path>` for a linked worktree"
        );
        working_dir.join(".libra")
    });
    let (objects_dir, dot_libra) = (storage_dir.join("objects"), storage_dir);

    // Try to create the directory
    if let Err(error) = std::fs::create_dir_all(&objects_dir) {
        tracing::warn!(
            target: "libra::internal::ai::codex",
            objects_dir = %objects_dir.display(),
            %error,
            "failed to create storage directory; running in read-only mode"
        );
        return Arc::new(LibraMcpServer::new_with_working_dir(
            None,
            None,
            working_dir.to_path_buf(),
        ));
    }

    // Connect to DB
    let db_path = dot_libra.join("libra.db");
    let db_path_str = db_path.to_str().unwrap_or_default();

    #[cfg(target_os = "windows")]
    let db_path_string = db_path_str.replace("\\", "/");
    #[cfg(target_os = "windows")]
    let db_path_str = &db_path_string;

    let db_conn = match db::establish_connection(db_path_str).await {
        Ok(conn) => conn,
        Err(error) => {
            tracing::warn!(
                target: "libra::internal::ai::codex",
                db_path = %db_path.display(),
                %error,
                "failed to connect to database; running in read-only mode"
            );
            return Arc::new(LibraMcpServer::new_with_working_dir(
                None,
                None,
                working_dir.to_path_buf(),
            ));
        }
    };

    // Initialize storage
    let storage: Arc<dyn Storage + Send + Sync> = Arc::new(ClientStorage::init(objects_dir));

    let intent_history_manager = Arc::new(HistoryManager::new(
        storage.clone(),
        dot_libra,
        Arc::new(db_conn),
    ));

    Arc::new(LibraMcpServer::new_with_working_dir(
        Some(intent_history_manager),
        Some(storage),
        working_dir.to_path_buf(),
    ))
}

// ---------------------------------------------------------------------------
// CLI 参数结构体 / CLI Arguments Struct
// ---------------------------------------------------------------------------

/// `libra code --provider=codex` 命令的命令行参数结构体。
///
/// 通过 `clap` derive 宏解析，包含以下配置项：
/// - `url`：Codex WebSocket 服务器地址（默认 `ws://127.0.0.1:8080`）；
/// - `cwd`：代理工作目录（默认当前目录 `.`）；
/// - `approval`：审批模式（`ask` / `accept` / `decline`）；
/// - `model_provider`：传给 Codex 的模型提供商标识符；
/// - `service_tier`：传给 Codex 的服务层级标识符；
/// - `personality`：传给 Codex 的人格标识符；
/// - `model`：传给 Codex 的具体模型标识符；
/// - `plan_mode`：是否启用严格的先规划后执行模式；
/// - `debug`：是否输出调试信息。
///
/// Command-line argument struct for the Codex agent (`libra code --provider=codex`),
/// parsed by `clap`. See field-level doc comments for details.
#[derive(Parser, Debug, Clone)]
pub struct AgentCodexArgs {
    /// Codex WebSocket URL
    #[arg(long, default_value = CODEX_WS_URL)]
    pub url: String,

    /// Working directory for the agent
    #[arg(long, default_value = ".")]
    pub cwd: String,

    /// Approval mode: ask (prompt), accept (auto-accept), decline (auto-decline)
    #[arg(long, default_value = "accept")]
    pub approval: String,

    /// Model provider identifier passed to Codex
    #[arg(long)]
    pub model_provider: Option<String>,

    /// Service tier identifier passed to Codex
    #[arg(long)]
    pub service_tier: Option<String>,

    /// Personality identifier passed to Codex
    #[arg(long)]
    pub personality: Option<String>,

    /// Model identifier passed to Codex
    #[arg(long)]
    pub model: Option<String>,

    /// Require Codex to produce a plan before attempting execution.
    #[arg(long, default_value_t = false)]
    pub plan_mode: bool,

    /// Debug mode: print collected data
    #[arg(long, default_value = "false")]
    pub debug: bool,

    /// UI mode for the embedded Code UI read model.
    #[arg(skip)]
    pub ui_mode: Option<String>,
}

// ---------------------------------------------------------------------------
// Prompt 构建函数 / Prompt Builder Functions
// ---------------------------------------------------------------------------

/// 生成面向开发者角色的计划模式系统提示（详细版）。
///
/// 当 `--plan-mode` 启用时，作为 `system` 或 `developer` 角色提示注入 Codex，
/// 强制 AI 在执行任何操作之前先生成结构化的步骤计划，等待用户批准后再执行。
/// 同时禁止使用 Markdown 格式，要求回复为纯文本（适合 CLI 终端显示）。
///
/// Returns the detailed developer-role system prompt for plan-first mode.
/// Injected as a `system`/`developer` instruction when `--plan-mode` is active.
/// Enforces structured plan generation before execution, prohibits Markdown
/// formatting, and guides the model to use `fileChange`-emitting edit paths
/// rather than `apply_patch`-style shell commands.
fn plan_mode_developer_instructions() -> &'static str {
    PlanningPromptBuilder::codex_plan_mode_developer_instructions()
}

/// 生成面向基础角色的计划模式系统提示（简洁版）。
///
/// 与 `plan_mode_developer_instructions` 相比更为简洁，
/// 适合注入到非 developer 角色的上下文（如 `user` 或 `assistant` 初始消息）中。
/// 同样禁止 Markdown 格式，要求先规划再执行，并优先使用 fileChange 路径编辑文件。
///
/// Returns the concise base-role system prompt for plan-first mode.
/// Shorter than `plan_mode_developer_instructions`; suitable for injection into
/// non-developer role contexts. Same constraints apply: structured plan before
/// execution, plain text only, prefer `fileChange`-emitting edit paths.
fn plan_mode_base_instructions() -> &'static str {
    PlanningPromptBuilder::codex_plan_mode_base_instructions()
}

// ---------------------------------------------------------------------------
// MCP 对象存储 / MCP Object Storage
// ---------------------------------------------------------------------------

/// 将任意可序列化对象写入 MCP（`.libra/objects/`）内容寻址存储，并更新历史索引。
///
/// 执行流程：
/// 1. 若 `object_id` 为空，打印警告并直接返回（防止产生无法寻址的对象）；
/// 2. 调用 `storage.put_json(object)` 将对象序列化为 JSON 并写入对象存储，
///    返回内容哈希 `ObjectHash`；
/// 3. 调用 `append_history_hash_if_changed` 在历史索引中更新该对象的哈希；
/// 4. 若 `debug` 为 `true`，打印调试日志（对象类型、ID、哈希）。
///
/// Writes any `Serialize` value to the MCP content-addressable object store and
/// updates the history index. Skips objects with an empty `object_id` to avoid
/// creating un-addressable entries.
///
/// # Arguments
/// * `mcp_server`   — MCP 服务器实例（提供 `storage` 和历史索引）。
/// * `object_type`  — 对象类型名称（如 `"patchset"`、`"run"`、`"thread"`）。
/// * `object_id`    — 对象的业务唯一 ID。
/// * `object`       — 需要存储的对象（必须实现 `serde::Serialize`）。
/// * `debug`        — 是否输出调试日志。
pub async fn store_to_mcp<T: serde::Serialize + Send + Sync>(
    mcp_server: &Arc<LibraMcpServer>,
    object_type: &str,
    object_id: &str,
    object: &T,
    debug: bool,
) {
    if object_id.is_empty() {
        tracing::warn!(
            target: "libra::internal::ai::codex",
            object_type,
            "refusing to store object with empty id"
        );
        return;
    }
    if let Some(storage) = &mcp_server.storage {
        match storage.put_json(object).await {
            Ok(hash) => {
                if let Err(error) =
                    append_history_hash_if_changed(mcp_server, object_type, object_id, hash).await
                {
                    tracing::warn!(
                        target: "libra::internal::ai::codex",
                        object_type,
                        object_id,
                        %error,
                        "failed to append history hash"
                    );
                }
                let _ = debug; // tracing replaces ad-hoc debug-print routing
                tracing::debug!(
                    target: "libra::internal::ai::codex",
                    object_type,
                    object_id,
                    %hash,
                    "stored object to MCP"
                );
            }
            Err(error) => {
                tracing::warn!(
                    target: "libra::internal::ai::codex",
                    object_type,
                    object_id,
                    %error,
                    "failed to store object to MCP"
                );
            }
        }
    } else {
        tracing::warn!(
            target: "libra::internal::ai::codex",
            object_type,
            object_id,
            "MCP storage not available"
        );
    }
}

fn codex_code_ui_capabilities() -> CodeUiCapabilities {
    CodeUiCapabilities {
        message_input: true,
        streaming_text: true,
        plan_updates: true,
        tool_calls: true,
        patchsets: true,
        interactive_approvals: true,
        structured_questions: false,
        provider_session_resume: false,
    }
}

fn codex_code_ui_status(session: &CodexSession) -> CodeUiSessionStatus {
    if session
        .approval_requests
        .iter()
        .any(|request| request.decision.is_none())
    {
        return CodeUiSessionStatus::AwaitingInteraction;
    }
    if session
        .tool_invocations
        .iter()
        .any(|invocation| invocation.status == ToolStatus::InProgress)
    {
        return CodeUiSessionStatus::ExecutingTool;
    }
    if session
        .runs
        .iter()
        .any(|run| run.status == RunStatus::InProgress)
        || session.thread.status == ThreadStatus::Running
    {
        return CodeUiSessionStatus::Thinking;
    }
    if session
        .runs
        .iter()
        .any(|run| run.status == RunStatus::Failed)
    {
        return CodeUiSessionStatus::Error;
    }
    if session
        .runs
        .iter()
        .any(|run| run.status == RunStatus::Completed)
    {
        return CodeUiSessionStatus::Completed;
    }
    CodeUiSessionStatus::Idle
}

fn build_code_ui_snapshot_from_codex_session(
    session: &CodexSession,
    current: &CodeUiSessionSnapshot,
    working_dir: &str,
) -> CodeUiSessionSnapshot {
    let active_run_ids = session
        .runs
        .iter()
        .filter(|run| run.status == RunStatus::InProgress)
        .map(|run| run.id.clone())
        .collect::<BTreeSet<_>>();
    let mut transcript = session
        .intents
        .iter()
        .map(|intent| CodeUiTranscriptEntry {
            id: intent.id.clone(),
            kind: CodeUiTranscriptEntryKind::UserMessage,
            title: Some("Developer".to_string()),
            content: Some(intent.content.clone()),
            status: Some("completed".to_string()),
            streaming: false,
            metadata: serde_json::json!({}),
            created_at: intent.created_at,
            updated_at: intent.created_at,
        })
        .chain(session.agent_messages.iter().map(|message| {
            let streaming = active_run_ids.contains(&message.run_id)
                || session.thread.current_turn_id.as_deref() == Some(message.run_id.as_str());
            CodeUiTranscriptEntry {
                id: message.id.clone(),
                kind: CodeUiTranscriptEntryKind::AssistantMessage,
                title: Some("Assistant".to_string()),
                content: Some(message.content.clone()),
                status: Some(if streaming {
                    "streaming".to_string()
                } else {
                    "completed".to_string()
                }),
                streaming,
                metadata: serde_json::json!({}),
                created_at: message.created_at,
                updated_at: message.created_at,
            }
        }))
        .collect::<Vec<_>>();
    transcript.sort_by_key(|entry| entry.created_at);

    CodeUiSessionSnapshot {
        session_id: current.session_id.clone(),
        thread_id: if session.thread.id.is_empty() {
            current.thread_id.clone()
        } else {
            Some(session.thread.id.clone())
        },
        working_dir: working_dir.to_string(),
        provider: current.provider.clone(),
        capabilities: current.capabilities.clone(),
        controller: current.controller.clone(),
        status: codex_code_ui_status(session),
        transcript,
        plans: session
            .plans
            .iter()
            .map(|plan| CodeUiPlanSnapshot {
                id: plan.id.clone(),
                title: Some("Plan".to_string()),
                summary: Some(plan.text.clone()),
                status: format!("{:?}", plan.status).to_lowercase(),
                steps: plan
                    .text
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .map(|line| CodeUiPlanStep {
                        step: line.trim().to_string(),
                        status: format!("{:?}", plan.status).to_lowercase(),
                    })
                    .collect(),
                updated_at: plan.created_at,
            })
            .collect(),
        tasks: session
            .tasks
            .iter()
            .map(|task| CodeUiTaskSnapshot {
                id: task.id.clone(),
                title: task.tool_name.clone(),
                status: format!("{:?}", task.status).to_lowercase(),
                details: None,
                updated_at: task.created_at,
            })
            .collect(),
        tool_calls: session
            .tool_invocations
            .iter()
            .map(|tool_call| CodeUiToolCallSnapshot {
                id: tool_call.id.clone(),
                tool_name: tool_call.tool_name.clone(),
                status: format!("{:?}", tool_call.status).to_lowercase(),
                summary: tool_call.arguments.as_ref().map(ToString::to_string),
                details: tool_call
                    .error
                    .clone()
                    .or_else(|| tool_call.result.as_ref().map(ToString::to_string)),
                updated_at: tool_call.created_at,
            })
            .collect(),
        patchsets: session
            .patchsets
            .iter()
            .map(|patchset| CodeUiPatchsetSnapshot {
                id: patchset.id.clone(),
                status: format!("{:?}", patchset.status).to_lowercase(),
                changes: patchset
                    .changes
                    .iter()
                    .map(|change| CodeUiPatchChange {
                        path: change.path.clone(),
                        change_type: change.change_type.clone(),
                        diff: Some(change.diff.clone()),
                    })
                    .collect(),
                updated_at: patchset.created_at,
            })
            .collect(),
        interactions: session
            .approval_requests
            .iter()
            .filter(|request| request.decision.is_none())
            .map(|request| CodeUiInteractionRequest {
                id: request.id.clone(),
                kind: match request.approval_type {
                    ApprovalType::CommandExecution
                    | ApprovalType::FileChange
                    | ApprovalType::ApplyPatch
                    | ApprovalType::Unknown => CodeUiInteractionKind::Approval,
                },
                title: Some("Approval required".to_string()),
                description: request.description.clone(),
                prompt: request.command.clone(),
                options: vec![
                    CodeUiInteractionOption {
                        id: "approve".to_string(),
                        label: "Approve".to_string(),
                        description: Some("Allow this request".to_string()),
                    },
                    CodeUiInteractionOption {
                        id: "approve_all".to_string(),
                        label: "Approve All".to_string(),
                        description: Some("Allow this and future approvals".to_string()),
                    },
                    CodeUiInteractionOption {
                        id: "decline".to_string(),
                        label: "Decline".to_string(),
                        description: Some("Reject this request".to_string()),
                    },
                    CodeUiInteractionOption {
                        id: "decline_all".to_string(),
                        label: "Decline All".to_string(),
                        description: Some("Reject this and future approvals".to_string()),
                    },
                ],
                status: CodeUiInteractionStatus::Pending,
                metadata: serde_json::json!({
                    "threadId": request.thread_id,
                    "runId": request.run_id,
                    "changes": request.changes,
                    "itemId": request.item_id,
                }),
                requested_at: request.requested_at,
                resolved_at: request.resolved_at,
            })
            .collect(),
        updated_at: Utc::now(),
    }
}

async fn publish_code_ui_snapshot(
    code_ui_session: &Arc<CodeUiSession>,
    codex_session: &Arc<Mutex<CodexSession>>,
    working_dir: &str,
) {
    let session = {
        let Some(session_guard) = lock_or_warn(codex_session, "publish code ui snapshot") else {
            return;
        };
        session_guard.clone()
    };
    let current = code_ui_session.snapshot().await;
    let snapshot = build_code_ui_snapshot_from_codex_session(&session, &current, working_dir);
    code_ui_session
        .replace_snapshot(CodeUiEventType::SessionUpdated, snapshot)
        .await;
}

async fn send_request(
    tx: &mpsc::Sender<String>,
    responses: &Arc<Mutex<std::collections::HashMap<u64, serde_json::Value>>>,
    notifies: &Arc<Mutex<std::collections::HashMap<u64, Arc<tokio::sync::Notify>>>>,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    use std::sync::atomic::{AtomicU64, Ordering};

    static REQUEST_ID: AtomicU64 = AtomicU64::new(1);
    let id = REQUEST_ID.fetch_add(1, Ordering::Relaxed);

    let notify = Arc::new(tokio::sync::Notify::new());
    if let Some(mut notifs) = lock_or_warn(notifies, "send_request notify insert") {
        notifs.insert(id, notify.clone());
    } else {
        return Err("failed to register request notify".to_string());
    }

    let msg = CodexMessage::new_request(id, method, params);
    tx.send(msg.to_json()).await.map_err(|e| e.to_string())?;

    let timeout = tokio::time::timeout(tokio::time::Duration::from_secs(30), async {
        notify.notified().await;
    });

    match timeout.await {
        Ok(_) => {
            let response =
                if let Some(mut resp) = lock_or_warn(responses, "send_request response read") {
                    resp.remove(&id)
                } else {
                    None
                };
            if let Some(response) = response {
                if let Some(mut notifs) = lock_or_warn(notifies, "send_request notify cleanup") {
                    notifs.remove(&id);
                }
                if let Some(error_obj) = response.get("error") {
                    return Err(format!("Error: {}", error_obj));
                }
                return Ok(response.get("result").cloned().unwrap_or(response));
            }
            Err("Response not found".to_string())
        }
        Err(_) => {
            if let Some(mut notifs) = lock_or_warn(notifies, "send_request notify cleanup timeout")
            {
                notifs.remove(&id);
            }
            Err("Timeout".to_string())
        }
    }
}

#[derive(Clone)]
struct CodexCodeUiAdapter {
    browser_session: Arc<CodeUiSession>,
    tx: mpsc::Sender<String>,
    responses: Arc<Mutex<std::collections::HashMap<u64, serde_json::Value>>>,
    notifies: Arc<Mutex<std::collections::HashMap<u64, Arc<tokio::sync::Notify>>>>,
    thread_id: Arc<Mutex<String>>,
    approval_mode: Arc<Mutex<String>>,
    pending_approvals: Arc<AsyncMutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>>,
}

#[async_trait::async_trait]
impl CodeUiReadModel for CodexCodeUiAdapter {
    fn session(&self) -> Arc<CodeUiSession> {
        self.browser_session.clone()
    }
}

#[async_trait::async_trait]
impl CodeUiCommandAdapter for CodexCodeUiAdapter {
    fn capabilities(&self) -> CodeUiCapabilities {
        codex_code_ui_capabilities()
    }

    async fn submit_message(&self, text: String) -> anyhow::Result<()> {
        let thread_id = lock_or_warn(&self.thread_id, "codex code ui thread id read")
            .map(|guard| guard.clone())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("Codex thread is not ready"))?;

        let approval_policy =
            match lock_or_warn(&self.approval_mode, "codex code ui approval mode read")
                .as_ref()
                .map(|mode| mode.as_str())
                .unwrap_or("ask")
            {
                "accept" => serde_json::json!("never"),
                _ => serde_json::json!("on-request"),
            };

        send_request(
            &self.tx,
            &self.responses,
            &self.notifies,
            "turn/start",
            serde_json::json!({
                "input": [{ "type": "text", "text": text }],
                "threadId": thread_id,
                "approvalPolicy": approval_policy,
            }),
        )
        .await
        .map(|_| ())
        .map_err(|error| anyhow!(error))
    }

    async fn respond_interaction(
        &self,
        interaction_id: &str,
        response: CodeUiInteractionResponse,
    ) -> anyhow::Result<()> {
        // Validate the decision and locate the pending sender BEFORE mutating
        // shared `approval_mode`. Otherwise a malformed response (no decision,
        // unknown interaction id, or already-resolved approval) would still
        // flip future Codex approvals to accept-all / decline-all, which is a
        // privilege-escalation risk. Mutate `approval_mode` only after we have
        // committed to delivering the user's decision.
        let Some(approved) = response
            .approved
            .or(match response.selected_option.as_deref() {
                Some("approve") | Some("approve_all") => Some(true),
                Some("decline") | Some("decline_all") => Some(false),
                _ => None,
            })
        else {
            return Err(anyhow!("Codex approvals require an explicit decision"));
        };

        let sender = {
            let mut pending = self.pending_approvals.lock().await;
            pending.remove(interaction_id)
        }
        .ok_or_else(|| anyhow!("Unknown pending approval: {interaction_id}"))?;
        sender
            .send(approved)
            .map_err(|_| anyhow!("The pending approval is no longer awaiting a response"))?;

        if let Some(apply_to_future) = response.apply_to_future.as_ref()
            && let Some(mut approval_mode) =
                lock_or_warn(&self.approval_mode, "codex code ui approval mode write")
        {
            *approval_mode = match apply_to_future {
                CodeUiApplyToFuture::No => "ask".to_string(),
                CodeUiApplyToFuture::AcceptAll => "accept".to_string(),
                CodeUiApplyToFuture::DeclineAll => "decline".to_string(),
            };
        }
        Ok(())
    }
}

pub async fn start_code_ui_runtime(
    args: AgentCodexArgs,
    mcp_server: Arc<LibraMcpServer>,
    browser_write_enabled: bool,
    initial_controller: CodeUiInitialController,
) -> anyhow::Result<Arc<CodeUiRuntimeHandle>> {
    let history_recorder = Arc::new(HistoryRecorder::new(mcp_server.clone(), args.debug));
    let history_writer = Arc::new(HistoryWriter::new(mcp_server.clone(), args.debug));
    tracing::info!(
        target: "libra::internal::ai::codex",
        url = %args.url,
        cwd = %args.cwd,
        approval = %args.approval,
        plan_mode = args.plan_mode,
        model = %args.model.as_deref().unwrap_or("(default)"),
        "connecting to Codex app-server"
    );
    let (ws_stream, _) = connect_async(args.url.as_str())
        .await
        .map_err(|e| anyhow::anyhow!("failed to connect to Codex at {}: {}", args.url, e))?;
    tracing::info!(
        target: "libra::internal::ai::codex",
        url = %args.url,
        "connected to Codex app-server"
    );

    let (mut write, read) = ws_stream.split();
    let (tx, mut rx) = mpsc::channel::<String>(100);
    let responses: Arc<Mutex<std::collections::HashMap<u64, serde_json::Value>>> =
        Arc::new(Mutex::new(std::collections::HashMap::new()));
    let notifies: Arc<Mutex<std::collections::HashMap<u64, Arc<tokio::sync::Notify>>>> =
        Arc::new(Mutex::new(std::collections::HashMap::new()));
    let thread_id = Arc::new(Mutex::new(String::new()));
    let approval_mode = Arc::new(Mutex::new(args.approval.clone()));
    let pending_approvals = Arc::new(AsyncMutex::new(HashMap::<
        String,
        tokio::sync::oneshot::Sender<bool>,
    >::new()));

    let browser_session = CodeUiSession::new(initial_snapshot(
        args.cwd.clone(),
        CodeUiProviderInfo {
            provider: "codex".to_string(),
            model: args.model.clone(),
            mode: Some(args.ui_mode.clone().unwrap_or_else(|| "web".to_string())),
            managed: true,
        },
        codex_code_ui_capabilities(),
    ));
    let codex_session: Arc<Mutex<CodexSession>> = Arc::new(Mutex::new(CodexSession::new()));
    if let Some(mut session_guard) = lock_or_warn(&codex_session, "init codex browser session") {
        session_guard.debug = args.debug;
    }

    let history_reader = HistoryReader::new(mcp_server.clone());
    let rebuild = history_reader.rebuild_view().await;
    if let Some(mut session_guard) = lock_or_warn(&codex_session, "rebuild codex browser session") {
        if !rebuild.thread.thread_id.is_empty() {
            session_guard.thread.id = rebuild.thread.thread_id.clone();
        }
        session_guard.thread.current_turn_id = rebuild.scheduler.active_run_id.clone();
        session_guard.thread.status = if rebuild.scheduler.active_run_id.is_some() {
            ThreadStatus::Running
        } else {
            ThreadStatus::Pending
        };
        session_guard.intents = rebuild
            .thread
            .intents
            .values()
            .map(|intent| Intent {
                id: intent.id.clone(),
                content: intent.content.clone(),
                thread_id: intent.thread_id.clone(),
                created_at: intent.created_at,
            })
            .collect();
        session_guard.agent_messages = rebuild
            .thread
            .runs
            .values()
            .flat_map(|_| Vec::<AgentMessage>::new())
            .collect();
        session_guard.plans = rebuild
            .thread
            .plans
            .values()
            .map(|plan| Plan {
                id: plan.id.clone(),
                text: plan.step_text.clone(),
                intent_id: plan.intent_id.clone(),
                thread_id: plan.thread_id.clone(),
                turn_id: plan.turn_id.clone(),
                status: PlanStatus::Pending,
                created_at: plan.created_at,
            })
            .collect();
        session_guard.tasks = rebuild
            .thread
            .tasks
            .values()
            .map(|task| Task {
                id: task.id.clone(),
                tool_name: task.title.clone(),
                plan_id: task.plan_id.clone(),
                thread_id: task.thread_id.clone(),
                turn_id: task.turn_id.clone(),
                status: TaskStatus::Pending,
                created_at: task.created_at,
            })
            .collect();
        session_guard.runs = rebuild
            .thread
            .runs
            .values()
            .map(|run| Run {
                id: run.id.clone(),
                thread_id: run.thread_id.clone(),
                status: RunStatus::Pending,
                started_at: run.started_at,
                completed_at: None,
            })
            .collect();
        session_guard.tool_invocations = rebuild.tool_invocations.clone();
        session_guard.patchsets = rebuild
            .thread
            .patchsets
            .values()
            .map(|patchset| PatchSet {
                id: patchset.id.clone(),
                run_id: patchset.run_id.clone(),
                thread_id: patchset.thread_id.clone(),
                changes: patchset.changes.clone(),
                status: patchset.status.clone(),
                created_at: patchset.created_at,
            })
            .collect();
    }

    publish_code_ui_snapshot(&browser_session, &codex_session, &args.cwd).await;

    tokio::spawn(async move {
        // Drain the outbound channel as fast as it fills. Earlier code wrapped
        // `rx.recv()` in a `tokio::select!` with a 1s timer, but `recv().await`
        // already wakes immediately on every message and exits cleanly once the
        // channel closes — the timer added cost without changing semantics.
        while let Some(msg) = rx.recv().await {
            if let Err(error) = write.send(Message::Text(msg.into())).await {
                tracing::warn!(
                    target: "libra::internal::ai::codex",
                    %error,
                    "code-ui WebSocket write failed; closing writer task"
                );
                break;
            }
        }
        tracing::debug!(
            target: "libra::internal::ai::codex",
            "code-ui WebSocket writer task exited"
        );
    });

    let responses_clone = responses.clone();
    let notifies_clone = notifies.clone();
    let tx_clone = tx.clone();
    let approval_mode_clone = approval_mode.clone();
    let debug_mode = args.debug;
    let codex_session_clone = codex_session.clone();
    let browser_session_clone = browser_session.clone();
    let mcp_server_clone = mcp_server.clone();
    let history_recorder_clone = history_recorder.clone();
    let history_writer_clone = history_writer.clone();
    let pending_approvals_clone = pending_approvals.clone();
    let working_dir_clone = args.cwd.clone();
    let thread_id_clone = thread_id.clone();
    ClientStorage::spawn_background_index_work(async move {
        let mut read = read;
        // When a streaming-delta event mutates in-memory state we deliberately
        // skip the publish_code_ui_snapshot broadcast so subscribers don't pay
        // a per-token deep clone of the entire CodexSession. This flag remembers
        // whether at least one delta has been skipped since the last publish so
        // that, if the WebSocket closes (clean disconnect, error abort) right
        // after a delta — without ever emitting a non-delta event such as
        // ItemCompleted / TurnCompleted — we still flush the final accumulated
        // text to subscribers before the reader task exits.
        let mut delta_skipped_since_publish = false;
        while let Some(message) = read.next().await {
            let Ok(Message::Text(text)) = message else {
                if let Err(error) = message {
                    tracing::warn!(
                        target: "libra::internal::ai::codex",
                        %error,
                        "code-ui WebSocket frame error"
                    );
                }
                continue;
            };
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                let has_result_or_error =
                    json.get("result").is_some() || json.get("error").is_some();
                let has_method = json.get("method").is_some();

                if let Some(id_val) = json.get("id") {
                    if let Some(id) = id_val.as_u64()
                        && has_result_or_error
                        && !has_method
                    {
                        let is_error = json.get("error").is_some();
                        tracing::debug!(
                            target: "libra::internal::ai::codex",
                            request_id = id,
                            is_error,
                            "code-ui RPC response received"
                        );
                        if let Some(mut resp) =
                            lock_or_warn(&responses_clone, "store code ui response")
                        {
                            resp.insert(id, json.clone());
                        }
                        if let Some(notifies_guard) =
                            lock_or_warn(&notifies_clone, "notify code ui response waiter")
                            && let Some(notify) = notifies_guard.get(&id)
                        {
                            notify.notify_waiters();
                        }
                    }
                    continue;
                }

                let Some(method) = json.get("method").and_then(|method| method.as_str()) else {
                    continue;
                };
                let mk = MethodKind::from(method);
                let params = json
                    .get("params")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                tracing::trace!(
                    target: "libra::internal::ai::codex",
                    method,
                    "code-ui notification dispatch"
                );

                match mk {
                    MethodKind::ThreadStarted => {
                        let started_thread_id = extract_thread_id(&params, None);
                        tracing::info!(
                            target: "libra::internal::ai::codex",
                            thread_id = %started_thread_id,
                            "Codex thread started"
                        );
                        if let Some(mut current_thread_id) =
                            lock_or_warn(&thread_id_clone, "code ui thread id update")
                        {
                            *current_thread_id = started_thread_id.clone();
                        }
                        if let Some(mut session) =
                            lock_or_warn(&codex_session_clone, "code ui thread started update")
                        {
                            session.update_thread(CodexThread {
                                id: started_thread_id.clone(),
                                status: ThreadStatus::Running,
                                name: None,
                                current_turn_id: None,
                                created_at: Utc::now(),
                                updated_at: Utc::now(),
                            });
                        }
                        let history_recorder = history_recorder_clone.clone();
                        let thread_event_payload = params.clone();
                        ClientStorage::spawn_background_index_work(async move {
                            history_recorder
                                .event(
                                    EventKind::ThreadStatus,
                                    &started_thread_id,
                                    "started",
                                    thread_event_payload,
                                )
                                .await;
                        });
                    }
                    MethodKind::ThreadStatusChanged => {
                        if let Some(mut session) =
                            lock_or_warn(&codex_session_clone, "code ui thread status update")
                        {
                            session.thread.status = match params
                                .get("status")
                                .and_then(|value| value.as_str())
                                .unwrap_or("running")
                            {
                                "completed" => ThreadStatus::Completed,
                                "archived" => ThreadStatus::Archived,
                                "closed" => ThreadStatus::Closed,
                                _ => ThreadStatus::Running,
                            };
                        }
                    }
                    MethodKind::TurnStarted => {
                        let run_id = params
                            .get("turnId")
                            .or_else(|| params.get("turn_id"))
                            .and_then(|value| value.as_str())
                            .unwrap_or("")
                            .to_string();
                        tracing::info!(
                            target: "libra::internal::ai::codex",
                            turn_id = %run_id,
                            "Codex turn started"
                        );
                        if let Some(mut session) =
                            lock_or_warn(&codex_session_clone, "code ui turn started update")
                        {
                            let thread_id = session.thread.id.clone();
                            session.thread.current_turn_id = Some(run_id.clone());
                            if !run_id.is_empty() {
                                session.add_run(Run {
                                    id: run_id,
                                    thread_id,
                                    status: RunStatus::InProgress,
                                    started_at: Utc::now(),
                                    completed_at: None,
                                });
                            }
                        }
                    }
                    MethodKind::TurnCompleted => {
                        if let Some(mut session) =
                            lock_or_warn(&codex_session_clone, "code ui turn completed update")
                            && let Some(run_id) = session.thread.current_turn_id.clone()
                        {
                            tracing::info!(
                                target: "libra::internal::ai::codex",
                                turn_id = %run_id,
                                "Codex turn completed"
                            );
                            let thread_id = session.thread.id.clone();
                            session.add_run(Run {
                                id: run_id,
                                thread_id,
                                status: RunStatus::Completed,
                                started_at: Utc::now(),
                                completed_at: Some(Utc::now()),
                            });
                        }
                    }
                    MethodKind::ItemStarted => {
                        if let Some(item) = params.get("item")
                            && let Some(item_type) =
                                item.get("type").and_then(|value| value.as_str())
                        {
                            let item_id = item
                                .get("id")
                                .and_then(|value| value.as_str())
                                .unwrap_or("")
                                .to_string();
                            let thread_id_value = extract_thread_id(&params, None);
                            let run_id = params
                                .get("turnId")
                                .or_else(|| params.get("turn_id"))
                                .and_then(|value| value.as_str())
                                .unwrap_or("")
                                .to_string();
                            tracing::debug!(
                                target: "libra::internal::ai::codex",
                                item_type,
                                item_id = %item_id,
                                turn_id = %run_id,
                                "Codex item started"
                            );
                            match item_type {
                                "intent" => {
                                    if let Some(content) =
                                        item.get("text").and_then(|value| value.as_str())
                                        && let Some(mut session) = lock_or_warn(
                                            &codex_session_clone,
                                            "code ui intent started update",
                                        )
                                    {
                                        tracing::debug!(
                                            target: "libra::internal::ai::codex",
                                            item_id = %item_id,
                                            preview = %truncate_for_log(content, 200),
                                            "Codex intent draft started"
                                        );
                                        session.add_intent(Intent {
                                            id: item_id,
                                            content: content.to_string(),
                                            thread_id: thread_id_value,
                                            created_at: Utc::now(),
                                        });
                                    }
                                }
                                "agentMessage" => {
                                    if let Some(content) =
                                        item.get("text").and_then(|value| value.as_str())
                                        && let Some(mut session) = lock_or_warn(
                                            &codex_session_clone,
                                            "code ui agent message started update",
                                        )
                                    {
                                        tracing::debug!(
                                            target: "libra::internal::ai::codex",
                                            item_id = %item_id,
                                            preview = %truncate_for_log(content, 200),
                                            "Codex agent message started"
                                        );
                                        session.add_agent_message(AgentMessage {
                                            id: item_id,
                                            run_id,
                                            thread_id: thread_id_value,
                                            content: content.to_string(),
                                            created_at: Utc::now(),
                                        });
                                    }
                                }
                                "reasoning" => {
                                    let preview = item
                                        .get("text")
                                        .and_then(|value| value.as_str())
                                        .map(|text| truncate_for_log(text, 400))
                                        .unwrap_or_default();
                                    tracing::debug!(
                                        target: "libra::internal::ai::codex",
                                        item_id = %item_id,
                                        turn_id = %run_id,
                                        preview = %preview,
                                        "Codex reasoning (thinking) started"
                                    );
                                }
                                "plan" => {
                                    let text = item
                                        .get("text")
                                        .and_then(|value| value.as_str())
                                        .unwrap_or("");
                                    tracing::debug!(
                                        target: "libra::internal::ai::codex",
                                        item_id = %item_id,
                                        turn_id = %run_id,
                                        preview = %truncate_for_log(text, 400),
                                        "Codex plan started"
                                    );
                                }
                                "commandExecution" => {
                                    let cmd = item
                                        .get("command")
                                        .and_then(|value| value.as_str())
                                        .unwrap_or("");
                                    tracing::debug!(
                                        target: "libra::internal::ai::codex",
                                        item_id = %item_id,
                                        turn_id = %run_id,
                                        command = %truncate_for_log(cmd, 200),
                                        "Codex command execution started"
                                    );
                                }
                                "fileChange" => {
                                    tracing::debug!(
                                        target: "libra::internal::ai::codex",
                                        item_id = %item_id,
                                        turn_id = %run_id,
                                        "Codex file change started"
                                    );
                                }
                                "mcpToolCall" | "tool" => {
                                    let tool = item
                                        .get("tool")
                                        .and_then(|value| value.as_str())
                                        .unwrap_or("");
                                    tracing::debug!(
                                        target: "libra::internal::ai::codex",
                                        item_id = %item_id,
                                        tool = %tool,
                                        "Codex tool invocation started"
                                    );
                                }
                                _ => {
                                    tracing::trace!(
                                        target: "libra::internal::ai::codex",
                                        item_type,
                                        item_id = %item_id,
                                        "Codex unhandled item type started"
                                    );
                                }
                            }
                        }
                    }
                    MethodKind::AgentMessageDelta => {
                        if let Some(item_id) = params
                            .get("itemId")
                            .or_else(|| params.get("item_id"))
                            .and_then(|value| value.as_str())
                            && let Some(delta) =
                                params.get("delta").and_then(|value| value.as_str())
                        {
                            tracing::trace!(
                                target: "libra::internal::ai::codex",
                                item_id,
                                delta_bytes = delta.len(),
                                delta = %truncate_for_log(delta, 200),
                                "Codex agent message delta"
                            );
                            if let Some(mut session) = lock_or_warn(
                                &codex_session_clone,
                                "code ui agent message delta update",
                            ) && let Some(message) = session
                                .agent_messages
                                .iter_mut()
                                .find(|message| message.id == item_id)
                            {
                                message.content.push_str(delta);
                            }
                        }
                    }
                    MethodKind::PlanUpdated | MethodKind::PlanDelta => {
                        if let Some(plan) = params.get("plan")
                            && let Some(plan_id) = plan
                                .get("id")
                                .or_else(|| plan.get("planId"))
                                .and_then(|value| value.as_str())
                        {
                            let explanation = plan
                                .get("explanation")
                                .and_then(|value| value.as_str())
                                .map(ToString::to_string);
                            let kind = if matches!(mk, MethodKind::PlanUpdated) {
                                "plan_updated"
                            } else {
                                "plan_delta"
                            };
                            tracing::debug!(
                                target: "libra::internal::ai::codex",
                                plan_id,
                                kind,
                                explanation = %explanation
                                    .as_deref()
                                    .map(|text| truncate_for_log(text, 400))
                                    .unwrap_or_default(),
                                "Codex plan update"
                            );
                            if let Some(mut session) =
                                lock_or_warn(&codex_session_clone, "code ui plan update")
                            {
                                let thread_id = session.thread.id.clone();
                                let turn_id = session.thread.current_turn_id.clone();
                                session.add_plan(Plan {
                                    id: plan_id.to_string(),
                                    text: build_plan_text(explanation.as_ref(), &[]),
                                    intent_id: None,
                                    thread_id,
                                    turn_id,
                                    status: PlanStatus::InProgress,
                                    created_at: Utc::now(),
                                });
                            }
                        }
                    }
                    MethodKind::RequestApproval
                    | MethodKind::RequestApprovalCommandExecution
                    | MethodKind::RequestApprovalFileChange
                    | MethodKind::RequestApprovalApplyPatch
                    | MethodKind::RequestApprovalExec => {
                        let request_id = params
                            .get("requestId")
                            .or_else(|| params.get("request_id"))
                            .and_then(|value| value.as_str())
                            .map(ToString::to_string)
                            .unwrap_or_else(|| format!("req_{}", Utc::now().timestamp_millis()));
                        let approval_type = match mk {
                            MethodKind::RequestApprovalCommandExecution => {
                                ApprovalType::CommandExecution
                            }
                            MethodKind::RequestApprovalFileChange => ApprovalType::FileChange,
                            MethodKind::RequestApprovalApplyPatch => ApprovalType::ApplyPatch,
                            _ => ApprovalType::Unknown,
                        };
                        tracing::info!(
                            target: "libra::internal::ai::codex",
                            request_id = %request_id,
                            approval_kind = ?approval_type,
                            "Codex approval requested"
                        );
                        let approval_request = ApprovalRequest {
                            id: request_id.clone(),
                            approval_type,
                            item_id: params
                                .get("itemId")
                                .and_then(|value| value.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            thread_id: extract_thread_id(&params, None),
                            run_id: None,
                            command: params
                                .get("command")
                                .and_then(|value| value.as_str())
                                .map(ToString::to_string),
                            changes: params.get("changes").and_then(|value| {
                                value.as_array().map(|items| {
                                    items
                                        .iter()
                                        .filter_map(|item| item.as_str().map(ToString::to_string))
                                        .collect::<Vec<_>>()
                                })
                            }),
                            description: params
                                .get("description")
                                .and_then(|value| value.as_str())
                                .map(ToString::to_string),
                            decision: None,
                            requested_at: Utc::now(),
                            resolved_at: None,
                        };
                        if let Some(mut session) =
                            lock_or_warn(&codex_session_clone, "code ui approval request update")
                        {
                            session.add_approval_request(approval_request);
                        }

                        let current_mode =
                            lock_or_warn(&approval_mode_clone, "code ui approval mode read")
                                .map(|mode| mode.clone())
                                .unwrap_or_else(|| "ask".to_string());
                        let approved = if current_mode == "accept" {
                            true
                        } else if current_mode == "decline" {
                            false
                        } else {
                            let (oneshot_tx, oneshot_rx) = tokio::sync::oneshot::channel();
                            pending_approvals_clone
                                .lock()
                                .await
                                .insert(request_id.clone(), oneshot_tx);
                            publish_code_ui_snapshot(
                                &browser_session_clone,
                                &codex_session_clone,
                                &working_dir_clone,
                            )
                            .await;
                            // Default to *deny* when the approval channel is
                            // dropped (TUI exit, runtime teardown). Auto-
                            // approving on cancellation could let a sandbox-
                            // escaping command run after the operator already
                            // closed the session.
                            match oneshot_rx.await {
                                Ok(decision) => decision,
                                Err(_) => {
                                    tracing::warn!(
                                        target: "libra::internal::ai::codex",
                                        request_id = %request_id,
                                        "approval channel closed before user response; \
                                         defaulting to DECLINE"
                                    );
                                    false
                                }
                            }
                        };

                        if let Some(mut session) =
                            lock_or_warn(&codex_session_clone, "code ui approval request resolve")
                            && let Some(approval) = session
                                .approval_requests
                                .iter_mut()
                                .find(|approval| approval.id == request_id)
                        {
                            approval.decision = Some(approved);
                            approval.resolved_at = Some(Utc::now());
                        }

                        let resolve_method = match mk {
                            MethodKind::RequestApprovalCommandExecution => {
                                "item/commandExecution/requestApproval/resolve"
                            }
                            MethodKind::RequestApprovalFileChange => {
                                "item/fileChange/requestApproval/resolve"
                            }
                            MethodKind::RequestApprovalExec => "exec_approval_request/resolve",
                            MethodKind::RequestApprovalApplyPatch => {
                                "apply_patch_approval_request/resolve"
                            }
                            _ => "requestApproval/resolve",
                        };
                        let approval_msg = CodexMessage::new_request(
                            Utc::now().timestamp_millis() as u64,
                            resolve_method,
                            serde_json::json!({
                                "requestId": request_id,
                                "approved": approved,
                            }),
                        );
                        let _ = tx_clone.send(approval_msg.to_json()).await;
                    }
                    _ => {}
                }

                // Coalesce broadcast: streaming-delta methods (AgentMessageDelta,
                // CommandExecutionOutputDelta, FileChangeOutputDelta, PlanDelta)
                // can fire many times per second per item. Skipping the publish
                // for those methods avoids per-token deep clones of the entire
                // CodexSession; the next item-completion / turn-completion /
                // approval event flushes the accumulated state to subscribers.
                if is_streaming_delta_method(mk) {
                    delta_skipped_since_publish = true;
                } else {
                    publish_code_ui_snapshot(
                        &browser_session_clone,
                        &codex_session_clone,
                        &working_dir_clone,
                    )
                    .await;
                    delta_skipped_since_publish = false;
                }

                let _ = (
                    &mcp_server_clone,
                    &history_recorder_clone,
                    &history_writer_clone,
                    debug_mode,
                );
            }
        }

        // Final flush: the WebSocket has closed (clean disconnect, error abort,
        // or peer EOF). If any streaming delta was skipped without being
        // followed by a non-delta event, the broadcast snapshot is now stale.
        // Publish once unconditionally so subscribers (e.g. the App's
        // `start_managed_code_turn`) always observe the final agent text /
        // patchset content even on abrupt termination.
        if delta_skipped_since_publish {
            tracing::debug!(
                target: "libra::internal::ai::codex",
                "WebSocket closed with pending streaming deltas; flushing final snapshot"
            );
            publish_code_ui_snapshot(
                &browser_session_clone,
                &codex_session_clone,
                &working_dir_clone,
            )
            .await;
        }
        tracing::info!(
            target: "libra::internal::ai::codex",
            "code-ui WebSocket reader task exited"
        );
    });

    let _ = send_request(
        &tx,
        &responses,
        &notifies,
        "initialize",
        serde_json::json!({
            "capabilities": serde_json::Value::Null,
            "clientInfo": { "name": "libra", "version": env!("CARGO_PKG_VERSION") },
            "cliVersion": env!("CARGO_PKG_VERSION"),
            "cwd": args.cwd,
            "modelProvider": args.model_provider,
            "serviceTier": args.service_tier,
            "personality": args.personality
        }),
    )
    .await
    .map_err(|error| anyhow!("initialization failed: {error}"))?;

    let thread_start = send_request(
        &tx,
        &responses,
        &notifies,
        "thread/start",
        serde_json::json!({
            "cwd": args.cwd,
            "approvalPolicy": if args.approval == "accept" { serde_json::json!("never") } else { serde_json::json!("on-request") },
            "serviceTier": args.service_tier,
            "model": args.model,
            "modelProvider": args.model_provider,
            "personality": args.personality,
            "sandbox": SandboxMode::WorkspaceWrite,
            "developerInstructions": if args.plan_mode {
                serde_json::json!(plan_mode_developer_instructions())
            } else {
                serde_json::Value::Null
            },
            "baseInstructions": if args.plan_mode {
                serde_json::json!(plan_mode_base_instructions())
            } else {
                serde_json::Value::Null
            },
        }),
    )
    .await
    .map_err(|error| anyhow!("thread start failed: {error}"))?;
    if let Some(id) = thread_start
        .get("thread")
        .and_then(|thread: &serde_json::Value| thread.get("id"))
        .and_then(|value: &serde_json::Value| value.as_str())
        .or_else(|| {
            thread_start
                .get("threadId")
                .and_then(|value: &serde_json::Value| value.as_str())
        })
        && let Some(mut current_thread_id) = lock_or_warn(&thread_id, "code ui thread id init")
    {
        *current_thread_id = id.to_string();
    }

    let adapter: Arc<dyn CodeUiProviderAdapter> = Arc::new(CodexCodeUiAdapter {
        browser_session,
        tx,
        responses,
        notifies,
        thread_id,
        approval_mode,
        pending_approvals,
    });
    let mut runtime_options = crate::internal::ai::web::code_ui::CodeUiRuntimeOptions::new(
        browser_write_enabled,
        false,
        initial_controller,
    );
    runtime_options.lease_duration =
        crate::internal::ai::web::code_ui::test_lease_duration_override()
            .map_err(|message| anyhow::anyhow!(message))?;
    Ok(CodeUiRuntimeHandle::build_with_options(adapter, runtime_options).await)
}

// ---------------------------------------------------------------------------
// 主入口函数 / Main Entry Point
// ---------------------------------------------------------------------------

/// 启动并运行 Codex 代理主循环。
///
/// 这是 `libra code --provider=codex` 的核心执行函数，完整的运行流程如下：
///
/// 1. **MCP 服务器准备**：若调用方传入了 `Some(mcp_server)`（例如由 `code.rs` 中的
///    HTTP 服务器已创建并共享），则复用该实例；否则调用 `init_mcp_server` 创建
///    仅本地使用的实例（向下兼容）。
///
/// 2. **WebSocket 连接**：通过 `connect_async` 连接到 Codex app-server（`args.url`），
///    将 stream 拆分为读（`read`）和写（`write`）两个半连接。
///
/// 3. **会话恢复**：调用 `HistoryReader::rebuild_view()` 从持久化对象重建
///    `CodexSession`，恢复 Thread、Run、Plan、Task、PatchSet 等状态。
///
/// 4. **后台任务启动**：
///    - `writer task`：从 `mpsc` 通道接收字符串并写入 WebSocket；
///    - `reader task`：从 WebSocket 读取服务器推送的通知，更新 `CodexSession` 状态，
///      并在收到审批请求（`ToolApprovalRequested`）时将请求转发给主循环。
///
/// 5. **主循环（stdin + 审批流）**：
///    - 从 stdin 读取用户输入，以 `sendMessage` 请求格式发送给 Codex；
///    - 处理来自 reader task 的审批请求（`approval_rx`），根据 `--approval` 模式
///      自动接受（`accept`）、自动拒绝（`decline`）或询问用户（`ask`）；
///    - 在 `ask` 模式下将工具调用详情展示给用户，并等待 `y/n` 输入。
///
/// # Arguments
/// * `args`       — CLI 参数（WebSocket URL、工作目录、审批模式、模型配置等）。
/// * `mcp_server` — 调用方提供的 MCP 服务器实例（`Some`）；或 `None`（创建本地实例）。
///
/// # Legacy stdin loop
///
/// `libra code --provider codex` does not call this path; it starts the default
/// Libra TUI and uses [`start_code_ui_runtime`] as the managed execution
/// backend. This function remains only for old internal callers that explicitly
/// want Codex's stdin/stdout loop.
///
/// The `mcp_server` parameter allows an HTTP-serving caller to share its
/// already-initialised `LibraMcpServer` instead of creating a duplicate. When
/// `None`, a local-only instance is created for backward compatibility.
///
/// # Errors
/// - WebSocket 连接失败时返回 `anyhow::Error`。
/// - 内部互斥锁初始化失败时返回 `anyhow::Error`。
#[deprecated(
    note = "legacy standalone Codex stdin loop; libra code --provider codex uses the default Libra TUI"
)]
pub async fn execute(
    args: AgentCodexArgs,
    mcp_server: Option<Arc<LibraMcpServer>>,
) -> anyhow::Result<()> {
    // ==========================================================================
    // 第一节：初始化 (Initialization)
    // 复用调用方已创建的 MCP server（当 `libra code` 命令已启动 HTTP 服务时），
    // 或在独立运行时创建本地专用实例。
    // 同时创建 HistoryRecorder（记录事件流）和 HistoryWriter（持久化快照），
    // 然后通过 tokio-tungstenite 建立与 Codex app-server 的 WebSocket 连接。
    // ==========================================================================

    // 复用调用方传入的 MCP server，或为当前工作目录新建一个本地专用实例
    let working_dir = PathBuf::from(&args.cwd);
    let mcp_server = match mcp_server {
        Some(server) => server,
        None => init_mcp_server(&working_dir).await,
    };
    // HistoryRecorder 负责向 MCP 写入结构化事件（EventKind 枚举），
    // HistoryWriter 负责将快照对象（Snapshot）序列化后写入存储
    let history_recorder = Arc::new(HistoryRecorder::new(mcp_server.clone(), args.debug));
    let history_writer = Arc::new(HistoryWriter::new(mcp_server.clone(), args.debug));
    println!("MCP server initialized.");

    println!("Connecting to Codex at {}...", args.url);

    // 建立 WebSocket 连接，失败时返回包含 URL 信息的可读错误
    let (ws_stream, _) = connect_async(args.url.as_str())
        .await
        .map_err(|e| anyhow::anyhow!("failed to connect to Codex at {}: {}", args.url, e))?;

    println!("Connected to Codex!");
    println!("Initializing...");
    if args.plan_mode {
        println!("Plan Mode: enabled (plan required before execution)");
    }

    // 将 WebSocket 流拆分为独立的写半部（write）和读半部（read），
    // 分别交给 writer task 和 reader task 独立处理
    let (mut write, read) = ws_stream.split();

    // 用于将待发送的 JSON 字符串从主循环/reader task 路由到 writer task 的 channel
    let (tx, mut rx) = mpsc::channel::<String>(100);

    // 审批流 channel：reader task 收到 RequestApproval* 通知后，
    // 通过此 channel 将审批参数和 oneshot 应答 sender 转发给主循环，
    // 由主循环负责向用户呈现并收集交互输入
    let (approval_tx, mut approval_rx) =
        mpsc::channel::<(serde_json::Value, tokio::sync::oneshot::Sender<bool>)>(10);

    // ==========================================================================
    // 第二节：Session 状态初始化与历史恢复 (Session State Setup)
    // 建立进程内的共享状态容器：
    //   - `thread_id`：当前 Codex thread 的 ID，由 thread/start 响应填充
    //   - `responses`：存储 WebSocket RPC 响应的 map（id -> JSON），
    //     由 reader task 写入，由 send_request 辅助函数消费
    //   - `notifies`：与 responses 配套的 Notify map，用于异步等待特定响应
    //   - `session`：内存中的完整会话视图（thread、run、plan、task、patchset 等），
    //     由 reader task 随 Codex 通知实时更新，供 MCP 工具查询
    // 随后通过 HistoryReader::rebuild_view() 从持久化存储重建上一次会话的状态，
    // 保证重启后会话上下文连续。
    // ==========================================================================

    // thread_id 在 thread/start 成功后由响应解析填充
    let mut thread_id = String::new();
    // responses / notifies：实现 send_request 的异步请求-响应等待机制
    let responses: Arc<Mutex<std::collections::HashMap<u64, serde_json::Value>>> =
        Arc::new(Mutex::new(std::collections::HashMap::new()));
    let notifies: Arc<Mutex<std::collections::HashMap<u64, Arc<tokio::sync::Notify>>>> =
        Arc::new(Mutex::new(std::collections::HashMap::new()));

    // session 是 MCP 工具层可查询的完整内存快照，需在初始化时先设置 debug 标志
    let session: Arc<Mutex<CodexSession>> = Arc::new(Mutex::new(CodexSession::new()));
    if let Some(mut session_guard) = lock_or_warn(&session, "init session") {
        session_guard.debug = args.debug;
    } else {
        return Err(anyhow::anyhow!("failed to initialize session state"));
    }
    // 从持久化存储重建上次会话视图（thread/run/plan/task/patchset/intent），
    // 使重启后的会话能从断点继续而非从零开始
    let history_reader = HistoryReader::new(mcp_server.clone());
    let rebuild = history_reader.rebuild_view().await;
    if let Some(mut session_guard) = lock_or_warn(&session, "rebuild session from history") {
        // 遍历历史事件，为每个 plan / task / run 保留"最新状态"快照，
        // 因为同一对象可能存在多条状态变更事件（created → in_progress → completed），
        // 这里只取时间戳最大的那条作为当前状态

        // 按 plan_id 汇聚最新的计划状态（PlanStatus）
        let mut latest_plan_status: std::collections::HashMap<
            String,
            (chrono::DateTime<Utc>, PlanStatus),
        > = std::collections::HashMap::new();
        for event in &rebuild.plan_step_events {
            let status = plan_status_from_event(&event.status);
            let entry = latest_plan_status
                .entry(event.plan_id.clone())
                .or_insert((event.at, status.clone()));
            if event.at >= entry.0 {
                *entry = (event.at, status);
            }
        }

        // 按 task_id 汇聚最新的任务状态（TaskStatus）
        let mut latest_task_status: std::collections::HashMap<
            String,
            (chrono::DateTime<Utc>, TaskStatus),
        > = std::collections::HashMap::new();
        for event in &rebuild.task_events {
            let status = task_status_from_event(&event.status);
            let entry = latest_task_status
                .entry(event.task_id.clone())
                .or_insert((event.at, status.clone()));
            if event.at >= entry.0 {
                *entry = (event.at, status);
            }
        }

        // 按 run_id 汇聚最新的执行状态（RunStatus），
        // 同时记录终态（Completed/Failed）对应的时间戳作为 completed_at
        let mut latest_run_status: std::collections::HashMap<
            String,
            (chrono::DateTime<Utc>, RunStatus),
        > = std::collections::HashMap::new();
        let mut latest_run_terminal_at: std::collections::HashMap<String, chrono::DateTime<Utc>> =
            std::collections::HashMap::new();
        for event in &rebuild.run_events {
            let status = run_status_from_event(&event.status);
            let entry = latest_run_status
                .entry(event.run_id.clone())
                .or_insert((event.at, status.clone()));
            if event.at >= entry.0 {
                *entry = (event.at, status.clone());
            }
            if matches!(status, RunStatus::Completed | RunStatus::Failed) {
                latest_run_terminal_at.insert(event.run_id.clone(), event.at);
            }
        }

        // 收集用户曾拒绝的 patchset ID 集合，
        // 以便在恢复 session 时将对应 patchset 标记为 Declined
        let mut patchset_declined: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for decision in &rebuild.decisions {
            if !decision.approved
                && let Some(patchset_id) = decision.chosen_patchset_id.as_ref()
            {
                patchset_declined.insert(patchset_id.clone());
            }
        }

        // 将历史数据填充回 session 内存视图，使本次启动后的 MCP 查询能看到完整历史

        // 恢复 thread 基础信息：ID、当前活跃 run（若有）、及线程状态
        if !rebuild.thread.thread_id.is_empty() {
            session_guard.thread.id = rebuild.thread.thread_id.clone();
        }
        session_guard.thread.current_turn_id = rebuild.scheduler.active_run_id.clone();
        session_guard.thread.status = if rebuild.scheduler.active_run_id.is_some() {
            ThreadStatus::Running
        } else if !rebuild.thread.thread_id.is_empty() {
            ThreadStatus::Completed
        } else {
            ThreadStatus::Pending
        };
        session_guard.intents = rebuild
            .thread
            .intents
            .values()
            .map(|i| Intent {
                id: i.id.clone(),
                content: i.content.clone(),
                thread_id: i.thread_id.clone(),
                created_at: i.created_at,
            })
            .collect();
        session_guard.plans = rebuild
            .thread
            .plans
            .values()
            .map(|p| Plan {
                id: p.id.clone(),
                text: p.step_text.clone(),
                intent_id: p.intent_id.clone(),
                thread_id: p.thread_id.clone(),
                turn_id: p.turn_id.clone(),
                status: latest_plan_status
                    .get(&p.id)
                    .map(|(_, status)| status.clone())
                    .unwrap_or(PlanStatus::Pending),
                created_at: p.created_at,
            })
            .collect();
        session_guard.tasks = rebuild
            .thread
            .tasks
            .values()
            .map(|t| Task {
                id: t.id.clone(),
                tool_name: t.title.clone(),
                plan_id: t.plan_id.clone(),
                thread_id: t.thread_id.clone(),
                turn_id: t.turn_id.clone(),
                status: latest_task_status
                    .get(&t.id)
                    .map(|(_, status)| status.clone())
                    .unwrap_or(TaskStatus::Pending),
                created_at: t.created_at,
            })
            .collect();
        session_guard.runs = rebuild
            .thread
            .runs
            .values()
            .map(|r| Run {
                id: r.id.clone(),
                thread_id: r.thread_id.clone(),
                status: latest_run_status
                    .get(&r.id)
                    .map(|(_, status)| status.clone())
                    .unwrap_or(RunStatus::Pending),
                started_at: r.started_at,
                completed_at: latest_run_terminal_at.get(&r.id).copied(),
            })
            .collect();
        session_guard.tool_invocations = rebuild.tool_invocations.clone();
        session_guard.patchsets = rebuild
            .thread
            .patchsets
            .values()
            .map(|p| PatchSet {
                id: p.id.clone(),
                run_id: p.run_id.clone(),
                thread_id: p.thread_id.clone(),
                changes: p.changes.clone(),
                status: if patchset_declined.contains(&p.id) {
                    PatchStatus::Declined
                } else {
                    p.status.clone()
                },
                created_at: p.created_at,
            })
            .collect();
    }

    // ==========================================================================
    // 第三节：Writer Task（WebSocket 写半部）
    // 独立 tokio task，负责将所有待发送消息从 mpsc channel 取出并写入
    // WebSocket 写半部（write）。主循环和 reader task 均通过 tx/tx_clone 发送消息，
    // 由此统一序列化写入，避免并发写冲突。
    // ==========================================================================
    let _write_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                // 从发送 channel 取出序列化后的 JSON 字符串，写入 WebSocket
                Some(msg) = rx.recv() => {
                    if write.send(Message::Text(msg.into())).await.is_err() {
                        break;
                    }
                }
                // 每秒空转一次，防止 select 在 channel 关闭时无限阻塞
                _ = tokio::time::sleep(tokio::time::Duration::from_secs(1)) => {}
            }
        }
    });

    // ==========================================================================
    // 第四节：Reader Task（WebSocket 通知处理器）
    // 这是整个 execute 函数的核心 task，负责持续监听 Codex app-server 通过
    // WebSocket 推送的所有通知（ServerNotification），按照 MethodKind 分派处理：
    //   - 更新内存 session 状态
    //   - 向 MCP storage 持久化快照
    //   - 向 history writer 写入事件流
    //   - 处理审批请求（通过 approval_tx 转发给主循环交互）
    //
    // 所有需要在 reader task 内访问的 Arc/clone 变量在 spawn 前提前 clone，
    // 以满足 'static + Send 约束。
    // ==========================================================================

    // 提前 clone 所有需要移入 reader task 闭包的 Arc 引用
    let responses_clone = responses.clone();
    let notifies_clone = notifies.clone();
    let tx_clone = tx.clone();
    let approval_tx_clone = approval_tx.clone();
    // approval_mode 在 reader task、主循环、turn/start 请求中均可能被修改（accept all / decline all），
    // 因此使用 Arc<Mutex> 在三处共享
    let approval_mode = Arc::new(Mutex::new(args.approval.clone()));
    let approval_mode_clone = approval_mode.clone();
    let approval_mode_for_turn = approval_mode.clone();
    let debug_mode = args.debug;
    let session_clone = session.clone();
    let mcp_server_clone = mcp_server.clone();
    let history_recorder_clone = history_recorder.clone();
    let history_writer_clone = history_writer.clone();
    // 以下字段用于构建 ProvenanceSnapshot，记录每次 run 使用的模型信息
    let model_for_run = args.model.clone();
    let model_provider_for_run = args.model_provider.clone();
    let service_tier_for_run = args.service_tier.clone();
    let personality_for_run = args.personality.clone();
    // commandExecution item 的默认工作目录（当 item 未提供 cwd 时使用）
    let default_command_cwd = args.cwd.clone();
    let _reader_task = ClientStorage::spawn_background_index_work(async move {
        let mut read = read;
        #[allow(clippy::while_let_loop)]
        loop {
            match read.next().await {
                Some(Ok(Message::Text(text))) => {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                        // Codex WebSocket 协议使用 JSON-RPC 2.0 风格。
                        // 消息分为两类，鉴别方式如下：
                        //   - RPC 响应：有 "id" + ("result" | "error")，且无 "method"
                        //   - 服务端通知：有 "method"（可选带 "id"，但没有 result/error）
                        // 因为两者都可能带 "id"，必须先检查 result/error 字段
                        let has_result_or_error =
                            json.get("result").is_some() || json.get("error").is_some();
                        let has_method = json.get("method").is_some();

                        // 处理 RPC 响应：将结果存入 responses map，并通知对应的 Notify，
                        // 唤醒 send_request 中正在 await 的 future
                        if let Some(id_val) = json.get("id") {
                            if let Some(id) = id_val.as_u64() {
                                // 仅当有 result/error 且无 method 时才视为响应（排除带 id 的通知请求）
                                if has_result_or_error && !has_method {
                                    if let Some(mut resp) =
                                        lock_or_warn(&responses_clone, "store response")
                                    {
                                        resp.insert(id, json.clone());
                                    }
                                    // 通知 send_request 中对应 id 的 Notify，结束等待
                                    if let Some(notifies_guard) =
                                        lock_or_warn(&notifies_clone, "notify response waiter")
                                        && let Some(notify) = notifies_guard.get(&id)
                                    {
                                        let notify: &tokio::sync::Notify = notify.as_ref();
                                        notify.notify_waiters();
                                    }
                                }
                                // 带 method 字段的消息为请求通知，不存储为响应
                            }
                        }
                        // 处理服务端推送的通知（Notification）：
                        // 没有 id 且有 method 字段，根据 MethodKind 分派到对应处理逻辑
                        else if let Some(method) = json.get("method") {
                            let method_str = method.as_str().unwrap_or("");

                            // Debug: print all method names
                            // eprintln!("[DEBUG] Received method: {}", method_str);

                            // 将原始 method 字符串转换为类型安全的 MethodKind 枚举，
                            // 后续所有分支均通过 matches!(mk, MethodKind::Xxx) 判断
                            let mk = MethodKind::from(method_str);
                            // Filter out truly noisy notifications
                            let _is_noise = matches!(mk, MethodKind::TokenUsageUpdated);

                            // Extract and print useful info based on notification type
                            if let Some(params_val) = json.get("params") {
                                let params = params_val.clone();
                                // 通知层次结构：Thread → Turn → Plan → Item → Detail
                                // --- Handle ThreadStarted：新建 thread，初始化 session 并持久化到 MCP ---
                                if matches!(mk, MethodKind::ThreadStarted) {
                                    let thread_id = parse_params::<ThreadStartedParams>(&params)
                                        .map(|p| p.thread.thread_id)
                                        .or_else(|| {
                                            params
                                                .get("thread")
                                                .and_then(|t| t.get("id"))
                                                .and_then(|t| t.as_str())
                                                .map(String::from)
                                        })
                                        .or_else(|| {
                                            params
                                                .get("thread")
                                                .and_then(|t| t.get("threadId"))
                                                .and_then(|t| t.as_str())
                                                .map(String::from)
                                        })
                                        .or_else(|| {
                                            params
                                                .get("threadId")
                                                .and_then(|t| t.as_str())
                                                .map(String::from)
                                        })
                                        .or_else(|| {
                                            params
                                                .get("thread_id")
                                                .and_then(|t| t.as_str())
                                                .map(String::from)
                                        })
                                        .unwrap_or_default();
                                    println!(
                                        "
=== New Thread: {} ===",
                                        &thread_id[..8.min(thread_id.len())]
                                    );

                                    let thread = CodexThread {
                                        id: thread_id.to_string(),
                                        status: ThreadStatus::Running,
                                        name: None,
                                        current_turn_id: None,
                                        created_at: Utc::now(),
                                        updated_at: Utc::now(),
                                    };
                                    let thread_id_for_mcp = thread_id.to_string();
                                    let thread_for_mcp = thread.clone();
                                    if let Some(mut session) =
                                        lock_or_warn(&session_clone, "thread started update")
                                    {
                                        session.update_thread(thread);
                                    }

                                    let mcp_server_for_thread = mcp_server_clone.clone();
                                    ClientStorage::spawn_background_index_work(async move {
                                        store_to_mcp(
                                            &mcp_server_for_thread,
                                            "thread",
                                            &thread_id_for_mcp,
                                            &thread_for_mcp,
                                            debug_mode,
                                        )
                                        .await;
                                    });
                                // --- Handle ThreadStatusChanged：同步更新内存中 thread 的状态字段 ---
                                } else if matches!(mk, MethodKind::ThreadStatusChanged) {
                                    let (thread_id, status) = if let Some(p) =
                                        parse_params::<ThreadStatusChangedParams>(&params)
                                    {
                                        (p.thread_id, p.status)
                                    } else {
                                        (
                                            params
                                                .get("threadId")
                                                .and_then(|t| t.as_str())
                                                .unwrap_or("")
                                                .to_string(),
                                            params
                                                .get("status")
                                                .and_then(|s| s.as_str())
                                                .unwrap_or("")
                                                .to_string(),
                                        )
                                    };

                                    let new_status = match status.as_str() {
                                        "pending" => ThreadStatus::Pending,
                                        "running" => ThreadStatus::Running,
                                        "completed" => ThreadStatus::Completed,
                                        "archived" => ThreadStatus::Archived,
                                        "closed" => ThreadStatus::Closed,
                                        _ => ThreadStatus::Running,
                                    };

                                    if debug_mode {
                                        eprintln!(
                                            "[DEBUG] Thread status changed: {} -> {:?}",
                                            thread_id, new_status
                                        );
                                    }

                                    if let Some(mut session) =
                                        lock_or_warn(&session_clone, "thread status update")
                                    {
                                        session.thread.status = new_status;
                                        session.thread.updated_at = Utc::now();
                                    }
                                // --- Handle ThreadNameUpdated：更新 thread 的显示名称 ---
                                } else if matches!(mk, MethodKind::ThreadNameUpdated) {
                                    let (thread_id, name) = if let Some(p) =
                                        parse_params::<ThreadNameUpdatedParams>(&params)
                                    {
                                        (p.thread_id, p.name)
                                    } else {
                                        (
                                            params
                                                .get("threadId")
                                                .and_then(|t| t.as_str())
                                                .unwrap_or("")
                                                .to_string(),
                                            params
                                                .get("name")
                                                .and_then(|n| n.as_str())
                                                .map(String::from),
                                        )
                                    };

                                    if debug_mode {
                                        eprintln!(
                                            "[DEBUG] Thread name updated: {} -> {:?}",
                                            thread_id, name
                                        );
                                    }

                                    if let Some(mut session) =
                                        lock_or_warn(&session_clone, "thread name update")
                                    {
                                        session.thread.name = name;
                                        session.thread.updated_at = Utc::now();
                                    }
                                // --- Handle ThreadArchived：将 thread 标记为已归档，停止接受新 turn ---
                                } else if matches!(mk, MethodKind::ThreadArchived) {
                                    let thread_id = parse_params::<ThreadArchivedParams>(&params)
                                        .map(|p| p.thread_id)
                                        .or_else(|| {
                                            params
                                                .get("threadId")
                                                .and_then(|t| t.as_str())
                                                .map(String::from)
                                        })
                                        .unwrap_or_default();

                                    if debug_mode {
                                        eprintln!("[DEBUG] Thread archived: {}", thread_id);
                                    }

                                    if let Some(mut session) =
                                        lock_or_warn(&session_clone, "thread archived update")
                                    {
                                        session.thread.status = ThreadStatus::Archived;
                                        session.thread.updated_at = Utc::now();
                                    }
                                // --- Handle ThreadCompacted：上下文窗口压缩事件，生成 ContextSnapshot 并存入 MCP ---
                                // Codex 在 token 接近上下文窗口上限时会触发此事件，将历史消息摘要化
                                } else if matches!(mk, MethodKind::ThreadCompacted) {
                                    let (thread_id, turn_id) = if let Some(p) =
                                        parse_params::<ThreadCompactedParams>(&params)
                                    {
                                        (p.thread_id, p.turn_id)
                                    } else {
                                        (
                                            params
                                                .get("threadId")
                                                .and_then(|t| t.as_str())
                                                .unwrap_or("")
                                                .to_string(),
                                            params
                                                .get("turnId")
                                                .and_then(|t| t.as_str())
                                                .unwrap_or("")
                                                .to_string(),
                                        )
                                    };
                                    println!("Context compacted for thread {}", thread_id);

                                    let snapshot = ContextSnapshot {
                                        id: format!("context_{}", turn_id),
                                        thread_id: thread_id.clone(),
                                        run_id: Some(turn_id.clone()),
                                        created_at: Utc::now(),
                                        data: serde_json::json!({}),
                                    };
                                    let snapshot_id = snapshot.id.clone();
                                    let snapshot_for_mcp = snapshot.clone();
                                    let mcp_server_for_snapshot = mcp_server_clone.clone();
                                    ClientStorage::spawn_background_index_work(async move {
                                        store_to_mcp(
                                            &mcp_server_for_snapshot,
                                            "context_snapshot",
                                            &snapshot_id,
                                            &snapshot_for_mcp,
                                            debug_mode,
                                        )
                                        .await;
                                    });
                                // --- Handle ThreadClosed：thread 已关闭，标记 session 状态为 Closed ---
                                } else if matches!(mk, MethodKind::ThreadClosed) {
                                    let thread_id = parse_params::<ThreadClosedParams>(&params)
                                        .map(|p| p.thread_id)
                                        .or_else(|| {
                                            params
                                                .get("threadId")
                                                .and_then(|t| t.as_str())
                                                .map(String::from)
                                        })
                                        .unwrap_or_default();

                                    if debug_mode {
                                        eprintln!("[DEBUG] Thread closed: {}", thread_id);
                                    }

                                    if let Some(mut session) =
                                        lock_or_warn(&session_clone, "thread closed update")
                                    {
                                        session.thread.status = ThreadStatus::Closed;
                                        session.thread.updated_at = Utc::now();
                                    }
                                // --- Handle TurnStarted：创建 Run 快照并记录到历史 ---
                                // 一个"Turn"对应一次完整的 LLM 推理轮次（即一次用户输入 → agent 响应）。
                                // 在此处：
                                //   1. 构建 Run / RunSnapshot / RunEvent / ProvenanceSnapshot 并通过
                                //      HistoryWriter 异步持久化
                                //   2. 将 Run 加入内存 session，并将 thread.current_turn_id 指向本 run
                                //   3. 通过 HistoryRecorder 记录 RunStatus in_progress 事件
                                //   4. 将 Run 对象存入 MCP storage
                                } else if matches!(mk, MethodKind::TurnStarted) {
                                    let (turn_id, thread_id) = if let Some(p) =
                                        parse_params::<TurnStartedParams>(&params)
                                    {
                                        (p.turn.id, p.thread_id)
                                    } else {
                                        (
                                            params
                                                .get("turn")
                                                .and_then(|t| t.get("id"))
                                                .and_then(|t| t.as_str())
                                                .unwrap_or("")
                                                .to_string(),
                                            params
                                                .get("threadId")
                                                .and_then(|t| t.as_str())
                                                .unwrap_or("")
                                                .to_string(),
                                        )
                                    };
                                    println!(
                                        "
--- Turn started: {} (thread: {}) ---",
                                        &turn_id[..8.min(turn_id.len())],
                                        &thread_id[..8.min(thread_id.len())]
                                    );

                                    // 构建 Run（内存状态）、RunSnapshot（持久化快照）、
                                    // RunEvent（状态变更事件）和 ProvenanceSnapshot（模型溯源信息）
                                    let run = Run {
                                        id: turn_id.to_string(),
                                        thread_id: thread_id.to_string(),
                                        status: RunStatus::InProgress,
                                        started_at: Utc::now(),
                                        completed_at: None,
                                    };
                                    let run_snapshot = RunSnapshot {
                                        id: turn_id.to_string(),
                                        thread_id: thread_id.to_string(),
                                        plan_id: None,
                                        task_id: None,
                                        started_at: Utc::now(),
                                    };
                                    let run_event = RunEvent {
                                        id: format!("run_event_{}", turn_id),
                                        run_id: turn_id.to_string(),
                                        status: "in_progress".to_string(),
                                        at: Utc::now(),
                                        error: None,
                                    };
                                    // ProvenanceSnapshot 记录本次 run 使用的模型/provider/tier 信息，
                                    // 用于后续审计和溯源查询
                                    let provenance = ProvenanceSnapshot {
                                        id: format!("prov_{}", turn_id),
                                        run_id: turn_id.to_string(),
                                        model: model_for_run.clone(),
                                        provider: model_provider_for_run.clone(),
                                        parameters: serde_json::json!({
                                            "service_tier": service_tier_for_run.clone(),
                                            "personality": personality_for_run.clone(),
                                        }),
                                        created_at: Utc::now(),
                                    };
                                    let history_writer = history_writer_clone.clone();
                                    let run_id_for_write = turn_id.to_string();
                                    ClientStorage::spawn_background_index_work(async move {
                                        history_writer
                                            .write("run_snapshot", &run_id_for_write, &run_snapshot)
                                            .await;
                                        history_writer
                                            .write("run_event", &run_event.id, &run_event)
                                            .await;
                                        history_writer
                                            .write(
                                                "provenance_snapshot",
                                                &provenance.id,
                                                &provenance,
                                            )
                                            .await;
                                    });
                                    let run_id = turn_id.to_string();
                                    let run_for_mcp = run.clone();
                                    if let Some(mut session) =
                                        lock_or_warn(&session_clone, "turn started update")
                                    {
                                        session.add_run(run);
                                        session.thread.current_turn_id = Some(turn_id.to_string());
                                        session.thread.status = ThreadStatus::Running;
                                        session.thread.updated_at = Utc::now();
                                    }

                                    let mcp_server_for_run = mcp_server_clone.clone();
                                    let history = history_recorder_clone.clone();
                                    ClientStorage::spawn_background_index_work(async move {
                                        store_to_mcp(
                                            &mcp_server_for_run,
                                            "run",
                                            &run_id,
                                            &run_for_mcp,
                                            debug_mode,
                                        )
                                        .await;
                                        history
                                            .event(
                                                history::EventKind::RunStatus,
                                                &run_id,
                                                "in_progress",
                                                serde_json::json!({"thread_id": thread_id}),
                                            )
                                            .await;
                                    });
                                // --- Handle TurnCompleted：将 Run 标记为完成并持久化终态 ---
                                // 将对应 run 的状态更新为 Completed，清除 thread.current_turn_id，
                                // 并写入终态 RunEvent 和 ContextSnapshot（release_candidate 标记）
                                } else if matches!(mk, MethodKind::TurnCompleted) {
                                    let turn_id = parse_params::<TurnCompletedParams>(&params)
                                        .map(|p| p.turn.id)
                                        .or_else(|| {
                                            params
                                                .get("turn")
                                                .and_then(|t| t.get("id"))
                                                .and_then(|t| t.as_str())
                                                .map(String::from)
                                        })
                                        .unwrap_or_default();
                                    if !turn_id.is_empty() {
                                        println!(
                                            "--- Turn completed: {} ---",
                                            &turn_id[..8.min(turn_id.len())]
                                        );

                                        let run_to_store = if let Some(mut session) =
                                            lock_or_warn(&session_clone, "turn completed update")
                                        {
                                            let run = session
                                                .runs
                                                .iter_mut()
                                                .find(|r| r.id == turn_id)
                                                .map(|run| {
                                                    run.status = RunStatus::Completed;
                                                    run.completed_at = Some(Utc::now());
                                                    run.clone()
                                                });
                                            session.thread.updated_at = Utc::now();
                                            run
                                        } else {
                                            None
                                        };

                                        if let Some(run) = run_to_store {
                                            let run_id = run.id.clone();
                                            let mcp_server_for_run = mcp_server_clone.clone();
                                            let history = history_recorder_clone.clone();
                                            let history_writer = history_writer_clone.clone();
                                            let run_event = RunEvent {
                                                id: format!("run_event_{}_completed", run_id),
                                                run_id: run_id.clone(),
                                                status: "completed".to_string(),
                                                at: Utc::now(),
                                                error: None,
                                            };
                                            if let Some(mut session) = lock_or_warn(
                                                &session_clone,
                                                "scheduler cleanup on run complete",
                                            ) && session.thread.current_turn_id.as_deref()
                                                == Some(&run_id)
                                            {
                                                session.thread.current_turn_id = None;
                                            }
                                            ClientStorage::spawn_background_index_work(
                                                async move {
                                                    store_to_mcp(
                                                        &mcp_server_for_run,
                                                        "run",
                                                        &run_id,
                                                        &run,
                                                        debug_mode,
                                                    )
                                                    .await;
                                                    history
                                                        .event(
                                                            history::EventKind::RunStatus,
                                                            &run_id,
                                                            "completed",
                                                            serde_json::json!({}),
                                                        )
                                                        .await;
                                                    history_writer
                                                        .write(
                                                            "run_event",
                                                            &run_event.id,
                                                            &run_event,
                                                        )
                                                        .await;
                                                    let context_snapshot = ContextSnapshot {
                                                        id: format!("context_rc_{}", run_id),
                                                        thread_id: run.thread_id.clone(),
                                                        run_id: Some(run_id.clone()),
                                                        created_at: Utc::now(),
                                                        data: serde_json::json!({ "release_candidate": true }),
                                                    };
                                                    history_writer
                                                        .write(
                                                            "context_snapshot",
                                                            &context_snapshot.id,
                                                            &context_snapshot,
                                                        )
                                                        .await;
                                                },
                                            );
                                        }
                                    } else {
                                        println!("--- Turn completed ---");
                                    }
                                // --- Handle TokenUsageUpdated：实时更新 token 消耗统计 ---
                                // 该通知频率较高（每次 LLM 调用后触发），仅更新内存 session 中的
                                // TurnTokenUsage，同时通过 HistoryWriter 异步写入 RunUsage 持久化记录
                                } else if matches!(mk, MethodKind::TokenUsageUpdated) {
                                    if let Some(p) =
                                        parse_params::<ThreadTokenUsageUpdatedParams>(&params)
                                    {
                                        let last = TokenUsage {
                                            cached_input_tokens: Some(
                                                p.token_usage.last.cached_input_tokens,
                                            ),
                                            input_tokens: Some(p.token_usage.last.input_tokens),
                                            output_tokens: Some(p.token_usage.last.output_tokens),
                                            reasoning_output_tokens: Some(
                                                p.token_usage.last.reasoning_output_tokens,
                                            ),
                                            total_tokens: Some(p.token_usage.last.total_tokens),
                                        };
                                        let total = TokenUsage {
                                            cached_input_tokens: Some(
                                                p.token_usage.total.cached_input_tokens,
                                            ),
                                            input_tokens: Some(p.token_usage.total.input_tokens),
                                            output_tokens: Some(p.token_usage.total.output_tokens),
                                            reasoning_output_tokens: Some(
                                                p.token_usage.total.reasoning_output_tokens,
                                            ),
                                            total_tokens: Some(p.token_usage.total.total_tokens),
                                        };
                                        let usage = TurnTokenUsage {
                                            thread_id: p.thread_id.clone(),
                                            turn_id: p.turn_id.clone(),
                                            last,
                                            total,
                                            model_context_window: p
                                                .token_usage
                                                .model_context_window,
                                            updated_at: Utc::now(),
                                        };
                                        if let Some(mut session) =
                                            lock_or_warn(&session_clone, "token usage update")
                                        {
                                            session.add_token_usage(usage);
                                        }

                                        let run_usage = RunUsage {
                                            run_id: p.turn_id.clone(),
                                            thread_id: p.thread_id.clone(),
                                            at: Utc::now(),
                                            usage: serde_json::json!(p.token_usage),
                                        };
                                        let history_writer = history_writer_clone.clone();
                                        let run_usage_id = format!(
                                            "run_usage_{}_{}",
                                            p.turn_id.clone(),
                                            Utc::now().timestamp_millis()
                                        );
                                        ClientStorage::spawn_background_index_work(async move {
                                            history_writer
                                                .write("run_usage", &run_usage_id, &run_usage)
                                                .await;
                                        });
                                    }
                                // --- Handle PlanUpdated：处理计划更新，为每个步骤创建 Task 并持久化 ---
                                // 此通知携带完整的 plan 步骤数组（包含 step 文本和状态）。
                                // 处理逻辑：
                                //   1. 判断是否复用已有 plan（相同 turn_id 且文本未变则复用）
                                //   2. 为每个步骤生成 PlanStepSnapshot、TaskSnapshot 和对应的状态变更事件
                                //   3. 只在状态发生变化时才写入 PlanStepEvent 和 TaskEvent，避免重复记录
                                //   4. 异步将 plan/plan_steps/tasks/events 通过 HistoryWriter 持久化
                                //   5. 通过 HistoryRecorder 记录 PlanStepStatus 聚合事件
                                } else if matches!(mk, MethodKind::PlanUpdated) {
                                    // params: { plan: [...], threadId, turnId, explanation? }
                                    let (thread_id, turn_id, plan_steps, explanation) =
                                        if let Some(p) =
                                            parse_params::<TurnPlanUpdatedParams>(&params)
                                        {
                                            (p.thread_id, p.turn_id, p.plan, p.explanation)
                                        } else {
                                            let thread_id = params
                                                .get("threadId")
                                                .and_then(|t| t.as_str())
                                                .unwrap_or("")
                                                .to_string();
                                            let turn_id = params
                                                .get("turnId")
                                                .and_then(|t| t.as_str())
                                                .unwrap_or("")
                                                .to_string();
                                            let explanation = params
                                                .get("explanation")
                                                .and_then(|e| e.as_str())
                                                .map(String::from);
                                            let plan_steps = params
                                                .get("plan")
                                                .and_then(|p| p.as_array())
                                                .map(|arr| {
                                                    arr.iter()
                                                        .map(|item| TurnPlanStep {
                                                            status: item
                                                                .get("status")
                                                                .and_then(|s| s.as_str())
                                                                .unwrap_or("unknown")
                                                                .to_string(),
                                                            step: item
                                                                .get("step")
                                                                .and_then(|s| s.as_str())
                                                                .unwrap_or("")
                                                                .to_string(),
                                                        })
                                                        .collect()
                                                })
                                                .unwrap_or_default();
                                            (thread_id, turn_id, plan_steps, explanation)
                                        };

                                    if !plan_steps.is_empty() {
                                        let plan_text =
                                            build_plan_text(explanation.as_ref(), &plan_steps);
                                        let plan_status = aggregate_plan_status(&plan_steps);
                                        let plan_now = Utc::now();
                                        let (
                                            intent_id_for_plan,
                                            plan_id,
                                            plan_created_at,
                                            parent_plan_ids,
                                            persisted_plan,
                                            persisted_plan_steps,
                                            persisted_tasks,
                                            persisted_plan_step_events,
                                            persisted_task_events,
                                        ) = if let Some(mut session) =
                                            lock_or_warn(&session_clone, "plan update")
                                        {
                                            let intent_id_for_plan =
                                                latest_thread_intent_id(&session, &thread_id, None);
                                            let latest_plan = session
                                                .plans
                                                .iter()
                                                .filter(|plan| {
                                                    plan.thread_id == thread_id
                                                        && plan.intent_id.as_deref()
                                                            == intent_id_for_plan.as_deref()
                                                })
                                                .max_by_key(|plan| plan.created_at)
                                                .cloned();
                                            let reuse_latest_plan =
                                                latest_plan.as_ref().is_some_and(|plan| {
                                                    plan.turn_id.as_deref()
                                                        == Some(turn_id.as_str())
                                                        && plan.text == plan_text
                                                });
                                            let plan_id = if reuse_latest_plan {
                                                latest_plan
                                                    .as_ref()
                                                    .map(|plan| plan.id.clone())
                                                    .unwrap_or_default()
                                            } else {
                                                format!(
                                                    "plan_{}_{}",
                                                    turn_id,
                                                    plan_now.timestamp_millis()
                                                )
                                            };
                                            let plan_created_at = latest_plan
                                                .as_ref()
                                                .filter(|_| reuse_latest_plan)
                                                .map(|plan| plan.created_at)
                                                .unwrap_or(plan_now);
                                            let parent_plan_ids = if reuse_latest_plan {
                                                Vec::new()
                                            } else {
                                                latest_plan
                                                    .as_ref()
                                                    .map(|plan| vec![plan.id.clone()])
                                                    .unwrap_or_default()
                                            };

                                            let plan = Plan {
                                                id: plan_id.clone(),
                                                text: plan_text.clone(),
                                                intent_id: intent_id_for_plan.clone(),
                                                thread_id: thread_id.to_string(),
                                                turn_id: Some(turn_id.to_string()),
                                                status: plan_status.clone(),
                                                created_at: plan_created_at,
                                            };
                                            let plan_snapshot = PlanSnapshot {
                                                id: plan_id.clone(),
                                                thread_id: thread_id.to_string(),
                                                intent_id: plan.intent_id.clone(),
                                                turn_id: Some(turn_id.to_string()),
                                                step_text: plan_text.clone(),
                                                parents: parent_plan_ids.clone(),
                                                context_frames: Vec::new(),
                                                created_at: plan_created_at,
                                            };

                                            let mut plan_step_snapshots =
                                                Vec::with_capacity(plan_steps.len());
                                            let mut task_snapshots =
                                                Vec::with_capacity(plan_steps.len());
                                            let mut plan_step_events = Vec::new();
                                            let mut task_events = Vec::new();

                                            for (ordinal, item) in plan_steps.iter().enumerate() {
                                                let normalized_status =
                                                    normalize_plan_step_status(&item.status);
                                                let step_id =
                                                    format!("{}_step_{}", plan_id, ordinal);
                                                let task_id =
                                                    format!("task_{}_{}", plan_id, ordinal);
                                                let previous_task_status = session
                                                    .tasks
                                                    .iter()
                                                    .find(|task| task.id == task_id)
                                                    .map(|task| task.status.clone());
                                                let task_status =
                                                    task_status_from_plan_step(&item.status);

                                                let plan_step_snapshot = PlanStepSnapshot {
                                                    id: step_id.clone(),
                                                    plan_id: plan_id.clone(),
                                                    text: item.step.clone(),
                                                    ordinal: ordinal as i64,
                                                    created_at: plan_created_at,
                                                };
                                                let task_snapshot = TaskSnapshot {
                                                    id: task_id.clone(),
                                                    thread_id: thread_id.to_string(),
                                                    plan_id: Some(plan_id.clone()),
                                                    intent_id: intent_id_for_plan.clone(),
                                                    turn_id: Some(turn_id.to_string()),
                                                    title: Some(item.step.clone()),
                                                    parent_task_id: None,
                                                    origin_step_id: Some(step_id.clone()),
                                                    dependencies: Vec::new(),
                                                    created_at: plan_created_at,
                                                };
                                                let task = Task {
                                                    id: task_id.clone(),
                                                    tool_name: Some(item.step.clone()),
                                                    plan_id: Some(plan_id.clone()),
                                                    thread_id: thread_id.to_string(),
                                                    turn_id: Some(turn_id.to_string()),
                                                    status: task_status.clone(),
                                                    created_at: plan_created_at,
                                                };

                                                if previous_task_status.as_ref()
                                                    != Some(&task_status)
                                                {
                                                    plan_step_events.push(PlanStepEvent {
                                                        id: format!(
                                                            "plan_step_event_{}_{}_{}",
                                                            plan_id,
                                                            ordinal,
                                                            plan_now.timestamp_millis()
                                                        ),
                                                        plan_id: plan_id.clone(),
                                                        step_id: step_id.clone(),
                                                        status: normalized_status.to_string(),
                                                        at: plan_now,
                                                        run_id: Some(turn_id.to_string()),
                                                    });
                                                    if normalized_status != "pending" {
                                                        task_events.push(TaskEvent {
                                                            id: format!(
                                                                "task_event_{}_{}_{}",
                                                                task_id,
                                                                normalized_status,
                                                                plan_now.timestamp_millis()
                                                            ),
                                                            task_id: task_id.clone(),
                                                            status: normalized_status.to_string(),
                                                            at: plan_now,
                                                            run_id: Some(turn_id.to_string()),
                                                        });
                                                    }
                                                }

                                                session.add_task(task);
                                                plan_step_snapshots.push(plan_step_snapshot);
                                                task_snapshots.push(task_snapshot);
                                            }

                                            session.add_plan(plan.clone());

                                            (
                                                intent_id_for_plan,
                                                plan_id,
                                                plan_created_at,
                                                parent_plan_ids,
                                                plan_snapshot,
                                                plan_step_snapshots,
                                                task_snapshots,
                                                plan_step_events,
                                                task_events,
                                            )
                                        } else {
                                            continue;
                                        };

                                        println!("\nPlan Updated:");
                                        if let Some(exp) = explanation.as_ref() {
                                            println!("  Explanation: {}", exp);
                                        }
                                        for item in plan_steps.iter() {
                                            let status_string =
                                                normalize_plan_step_status(&item.status);
                                            let step_string = item.step.as_str();
                                            let marker = match status_string {
                                                "completed" => "[x]",
                                                "in_progress" => "[>]",
                                                _ => "[ ]",
                                            };
                                            println!("  {} {}", marker, step_string);
                                        }

                                        let history_writer = history_writer_clone.clone();
                                        let plan_snapshot_id = plan_id.clone();
                                        let mcp_server_for_plan = mcp_server_clone.clone();
                                        let history = history_recorder_clone.clone();
                                        let plan_for_mcp = Plan {
                                            id: plan_id.clone(),
                                            text: plan_text.clone(),
                                            intent_id: intent_id_for_plan.clone(),
                                            thread_id: thread_id.to_string(),
                                            turn_id: Some(turn_id.to_string()),
                                            status: plan_status,
                                            created_at: plan_created_at,
                                        };
                                        ClientStorage::spawn_background_index_work(async move {
                                            history_writer
                                                .write(
                                                    "plan_snapshot",
                                                    &plan_snapshot_id,
                                                    &persisted_plan,
                                                )
                                                .await;
                                            for step in persisted_plan_steps {
                                                history_writer
                                                    .write("plan_step_snapshot", &step.id, &step)
                                                    .await;
                                            }
                                            for task in persisted_tasks {
                                                history_writer
                                                    .write("task_snapshot", &task.id, &task)
                                                    .await;
                                            }
                                            for event in persisted_plan_step_events {
                                                history_writer
                                                    .write("plan_step_event", &event.id, &event)
                                                    .await;
                                            }
                                            for event in persisted_task_events {
                                                history_writer
                                                    .write("task_event", &event.id, &event)
                                                    .await;
                                            }
                                            store_to_mcp(
                                                &mcp_server_for_plan,
                                                "plan",
                                                &plan_id,
                                                &plan_for_mcp,
                                                debug_mode,
                                            )
                                            .await;
                                            history
                                                .event(
                                                    history::EventKind::PlanStepStatus,
                                                    &plan_id,
                                                    match aggregate_plan_status(&plan_steps) {
                                                        PlanStatus::Completed => "completed",
                                                        PlanStatus::InProgress => "in_progress",
                                                        PlanStatus::Pending => "pending",
                                                    },
                                                    serde_json::json!({
                                                        "step_count": plan_steps.len(),
                                                        "parents": parent_plan_ids
                                                    }),
                                                )
                                                .await;
                                        });
                                    }
                                // --- Handle PlanDelta：流式追加计划文本片段到内存 plan ---
                                // 当 Codex 以流式方式输出计划时，文本通过多个 PlanDelta 逐片到达。
                                // 若 plan 已存在则追加，否则创建一个 InProgress 占位 plan
                                } else if matches!(mk, MethodKind::PlanDelta) {
                                    if let Some(p) =
                                        parse_params::<DeltaNotificationParams>(&params)
                                    {
                                        if let Some(mut session) =
                                            lock_or_warn(&session_clone, "plan delta update")
                                        {
                                            if let Some(plan) = session
                                                .plans
                                                .iter_mut()
                                                .find(|pl| pl.id == p.item_id)
                                            {
                                                // 将本次片段追加到已有 plan 的文本末尾
                                                plan.text.push_str(&p.delta);
                                            } else {
                                                let plan = Plan {
                                                    id: p.item_id.clone(),
                                                    text: p.delta.clone(),
                                                    intent_id: None,
                                                    thread_id: p.thread_id.clone(),
                                                    turn_id: Some(p.turn_id.clone()),
                                                    status: PlanStatus::InProgress,
                                                    created_at: Utc::now(),
                                                };
                                                session.add_plan(plan);
                                            }
                                        }
                                        if debug_mode {
                                            eprintln!("[DEBUG] plan delta {} bytes", p.delta.len());
                                        }
                                    }
                                // --- Handle AgentMessageDelta：流式输出 agent 回复文本 ---
                                // 将每个 delta 片段追加到内存 AgentMessage.content，
                                // 同时直接打印到 stdout 以实现流式显示效果
                                } else if matches!(mk, MethodKind::AgentMessageDelta) {
                                    if let Some(p) =
                                        parse_params::<DeltaNotificationParams>(&params)
                                    {
                                        if let Some(mut session) = lock_or_warn(
                                            &session_clone,
                                            "agent message delta update",
                                        ) {
                                            if let Some(msg) = session
                                                .agent_messages
                                                .iter_mut()
                                                .find(|m| m.id == p.item_id)
                                            {
                                                msg.content.push_str(&p.delta);
                                            } else {
                                                let msg = AgentMessage {
                                                    id: p.item_id.clone(),
                                                    run_id: p.turn_id.clone(),
                                                    thread_id: p.thread_id.clone(),
                                                    content: p.delta.clone(),
                                                    created_at: Utc::now(),
                                                };
                                                session.add_agent_message(msg);
                                            }
                                        }
                                        print!("{}", p.delta);
                                    }
                                // --- Handle CommandExecutionOutputDelta：流式追加命令执行输出 ---
                                // 将 shell 命令的 stdout/stderr 输出片段追加到对应 ToolInvocation 的
                                // result.output 字段，同时打印到 stdout 供用户实时查看
                                } else if matches!(mk, MethodKind::CommandExecutionOutputDelta) {
                                    if let Some(p) =
                                        parse_params::<DeltaNotificationParams>(&params)
                                    {
                                        if let Some(mut session) = lock_or_warn(
                                            &session_clone,
                                            "command output delta update",
                                        ) && let Some(invocation) = session
                                            .tool_invocations
                                            .iter_mut()
                                            .find(|i| i.id == p.item_id)
                                        {
                                            let mut output = invocation
                                                .result
                                                .as_ref()
                                                .and_then(|v| v.get("output"))
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("")
                                                .to_string();
                                            output.push_str(&p.delta);
                                            invocation.result =
                                                Some(serde_json::json!({ "output": output }));
                                        }
                                        print!("{}", p.delta);
                                    }
                                // --- Handle FileChangeOutputDelta：流式追加文件变更 diff 片段 ---
                                // Codex 以流式方式输出 patch 内容时，每个 delta 追加到
                                // 对应 PatchSet 的 "(stream)" 占位 change 的 diff 字段；
                                // 待 ItemCompleted/fileChange 到达后再替换为完整的 changes 列表
                                } else if matches!(mk, MethodKind::FileChangeOutputDelta) {
                                    if let Some(p) =
                                        parse_params::<DeltaNotificationParams>(&params)
                                    {
                                        if let Some(mut session) =
                                            lock_or_warn(&session_clone, "file change delta update")
                                        {
                                            if let Some(patchset) = session
                                                .patchsets
                                                .iter_mut()
                                                .find(|ps| ps.id == p.item_id)
                                            {
                                                if let Some(change) = patchset
                                                    .changes
                                                    .iter_mut()
                                                    .find(|c| c.path == "(stream)")
                                                {
                                                    change.diff.push_str(&p.delta);
                                                } else {
                                                    patchset.changes.push(FileChange {
                                                        path: "(stream)".to_string(),
                                                        diff: p.delta.clone(),
                                                        change_type: "delta".to_string(),
                                                    });
                                                }
                                            } else {
                                                let patchset = PatchSet {
                                                    id: p.item_id.clone(),
                                                    run_id: p.turn_id.clone(),
                                                    thread_id: p.thread_id.clone(),
                                                    changes: vec![FileChange {
                                                        path: "(stream)".to_string(),
                                                        diff: p.delta.clone(),
                                                        change_type: "delta".to_string(),
                                                    }],
                                                    status: PatchStatus::InProgress,
                                                    created_at: Utc::now(),
                                                };
                                                session.add_patchset(patchset);
                                            }
                                        }
                                        print!("{}", p.delta);
                                    }
                                } else if matches!(mk, MethodKind::Initialized) {
                                    // Server initialized notification (after client sends initialize request)
                                    println!("[Codex] Server initialized");
                                // --- Handle TaskStarted：顶层任务开始，创建 Task 快照并记录事件 ---
                                // 一个 Task 对应 plan 中的一个步骤的实际执行。
                                // 从已有的 plan/intent 上下文中查找关联 ID，
                                // 构建 TaskSnapshot 和 TaskEvent 写入历史，
                                // 并将 Task 对象存入 MCP storage
                                } else if matches!(mk, MethodKind::TaskStarted) {
                                    // Task started - top level notification
                                    let thread_id = lock_or_warn(
                                        &session_clone,
                                        "thread lookup for task start",
                                    )
                                    .as_deref()
                                    .map(|session| extract_thread_id(&params, Some(session)))
                                    .unwrap_or_else(|| extract_thread_id(&params, None));
                                    let task_id = extract_task_id(&params);
                                    if task_id.is_empty() {
                                        eprintln!(
                                            "[WARN] TaskStarted notification missing task id: {}",
                                            params
                                        );
                                        continue;
                                    }
                                    let (
                                        existing_plan_id,
                                        existing_turn_id,
                                        intent_id_for_task,
                                        run_id_for_task_event,
                                    ) = if let Some(mut session) =
                                        lock_or_warn(&session_clone, "task started update")
                                    {
                                        let mut plan_id = None;
                                        let mut turn_id = None;
                                        if let Some(task) =
                                            session.tasks.iter_mut().find(|t| t.id == task_id)
                                        {
                                            task.status = TaskStatus::InProgress;
                                            plan_id = task.plan_id.clone();
                                            turn_id = task.turn_id.clone();
                                        }
                                        let run_id = session.thread.current_turn_id.clone();
                                        let intent_id = plan_id
                                            .as_ref()
                                            .and_then(|pid| {
                                                session
                                                    .plans
                                                    .iter()
                                                    .find(|plan| &plan.id == pid)
                                                    .and_then(|plan| plan.intent_id.clone())
                                            })
                                            .or_else(|| {
                                                latest_thread_intent_id(&session, &thread_id, None)
                                            });
                                        (plan_id, turn_id, intent_id, run_id)
                                    } else {
                                        (None, None, None, None)
                                    };
                                    let task_name = extract_task_name(&params);

                                    println!(
                                        "\n🚀 Task Started: {} (thread: {})",
                                        task_name,
                                        &thread_id[..8.min(thread_id.len())]
                                    );

                                    // Store Task (tool_name stores task name)
                                    let task = Task {
                                        id: task_id.clone(),
                                        tool_name: Some(task_name.clone()),
                                        plan_id: existing_plan_id.clone(),
                                        thread_id: thread_id.clone(),
                                        turn_id: existing_turn_id
                                            .clone()
                                            .or(run_id_for_task_event.clone()),
                                        status: TaskStatus::InProgress,
                                        created_at: Utc::now(),
                                    };
                                    let task_snapshot = TaskSnapshot {
                                        id: task_id.clone(),
                                        thread_id: thread_id.clone(),
                                        plan_id: existing_plan_id.clone(),
                                        intent_id: intent_id_for_task.clone(),
                                        turn_id: existing_turn_id
                                            .clone()
                                            .or(run_id_for_task_event.clone()),
                                        title: Some(task_name.clone()),
                                        parent_task_id: None,
                                        origin_step_id: existing_plan_id.clone(),
                                        dependencies: Vec::new(),
                                        created_at: Utc::now(),
                                    };
                                    let task_event = TaskEvent {
                                        id: format!("task_event_{}", task_id),
                                        task_id: task_id.clone(),
                                        status: "in_progress".to_string(),
                                        at: Utc::now(),
                                        run_id: run_id_for_task_event.clone(),
                                    };
                                    let history_writer = history_writer_clone.clone();
                                    let task_id_for_write = task_id.clone();
                                    ClientStorage::spawn_background_index_work(async move {
                                        history_writer
                                            .write(
                                                "task_snapshot",
                                                &task_id_for_write,
                                                &task_snapshot,
                                            )
                                            .await;
                                        history_writer
                                            .write("task_event", &task_event.id, &task_event)
                                            .await;
                                    });
                                    let task_id_for_mcp = task_id.clone();
                                    let task_for_mcp = task.clone();

                                    if let Some(mut session) =
                                        lock_or_warn(&session_clone, "task started update")
                                    {
                                        session.add_task(task);
                                    }

                                    // Store to MCP in background
                                    let mcp_server_for_task = mcp_server_clone.clone();
                                    let history = history_recorder_clone.clone();
                                    ClientStorage::spawn_background_index_work(async move {
                                        store_to_mcp(
                                            &mcp_server_for_task,
                                            "task",
                                            &task_id_for_mcp,
                                            &task_for_mcp,
                                            debug_mode,
                                        )
                                        .await;
                                        history
                                            .event(
                                                history::EventKind::TaskStatus,
                                                &task_id_for_mcp,
                                                "in_progress",
                                                serde_json::json!({"task_name": task_name}),
                                            )
                                            .await;
                                    });
                                // --- Handle TaskCompleted：顶层任务完成，写入终态事件并更新 Intent 状态 ---
                                // 将 task 状态更新为 Completed，写入终态 TaskEvent；
                                // 同时写入 IntentEvent（completed），标志用户意图已被执行完成
                                } else if matches!(mk, MethodKind::TaskCompleted) {
                                    // Task completed - top level notification
                                    println!(
                                        "
Task Completed"
                                    );
                                    let task_id = extract_task_id(&params);
                                    if task_id.is_empty() {
                                        eprintln!(
                                            "[WARN] TaskCompleted notification missing task id: {}",
                                            params
                                        );
                                        continue;
                                    }
                                    let (intent_id_for_event, run_id_for_task_event) =
                                        if let Some(mut session) =
                                            lock_or_warn(&session_clone, "task completed update")
                                        {
                                            let mut plan_id = None;
                                            let mut run_id = session.thread.current_turn_id.clone();
                                            if let Some(task) =
                                                session.tasks.iter_mut().find(|t| t.id == task_id)
                                            {
                                                task.status = TaskStatus::Completed;
                                                plan_id = task.plan_id.clone();
                                                run_id = task.turn_id.clone().or(run_id);
                                            }
                                            let intent_id = plan_id
                                                .as_ref()
                                                .and_then(|pid| {
                                                    session
                                                        .plans
                                                        .iter()
                                                        .find(|plan| &plan.id == pid)
                                                        .and_then(|plan| plan.intent_id.clone())
                                                })
                                                .or_else(|| {
                                                    latest_thread_intent_id(
                                                        &session,
                                                        &session.thread.id,
                                                        None,
                                                    )
                                                })
                                                .unwrap_or_default();
                                            (intent_id, run_id)
                                        } else {
                                            (String::new(), None)
                                        };
                                    let task_event = TaskEvent {
                                        id: format!("task_event_completed_{}", task_id),
                                        task_id,
                                        status: "completed".to_string(),
                                        at: Utc::now(),
                                        run_id: run_id_for_task_event,
                                    };
                                    let intent_event = IntentEvent {
                                        id: format!(
                                            "intent_event_completed_{}",
                                            Utc::now().timestamp_millis()
                                        ),
                                        intent_id: intent_id_for_event.clone(),
                                        status: "completed".to_string(),
                                        at: Utc::now(),
                                        next_intent_id: None,
                                    };
                                    let history_writer = history_writer_clone.clone();
                                    ClientStorage::spawn_background_index_work(async move {
                                        history_writer
                                            .write("task_event", &task_event.id, &task_event)
                                            .await;
                                        history_writer
                                            .write("intent_event", &intent_event.id, &intent_event)
                                            .await;
                                    });
                                }
                                // --- Handle ItemStarted：处理各类子项目（item）启动事件 ---
                                // 注意：此处使用 `if` 而非 `else if`，因为需要在 TaskStarted/TaskCompleted
                                // 的 else-if 链之外独立匹配，避免借用跨越 async 边界的问题。
                                // item.type 决定具体处理逻辑，支持以下类型：
                                //   - "mcpToolCall"：MCP 工具调用，记录 ToolInvocation 并存入 MCP
                                //   - "toolCall"：内置工具调用
                                //   - "commandExecution"：Shell 命令执行，额外记录工作区快照（用于后续 diff）
                                //   - "reasoning"：LLM 思考过程（extended thinking）
                                //   - "plan"：计划项目
                                //   - "fileChange"：文件变更，创建 PatchSet 占位
                                //   - "dynamicToolCall"：动态工具调用
                                //   - "webSearch"：网络搜索
                                //   - "userMessage"：用户消息，触发 Intent 创建和溯源链接
                                //   - "agentMessage"：agent 回复消息
                                //   - "collabAgentToolCall"：协作 agent 工具调用
                                //   - "contextCompaction"：上下文压缩开始
                                if matches!(mk, MethodKind::ItemStarted) {
                                    if let Some(params_obj) = json.get("params").cloned() {
                                        let thread_id = params_obj
                                            .get("threadId")
                                            .and_then(|t| t.as_str())
                                            .unwrap_or("")
                                            .to_string();
                                        let turn_id = params_obj
                                            .get("turnId")
                                            .and_then(|t| t.as_str())
                                            .unwrap_or("")
                                            .to_string();
                                        let run_id = turn_id.clone();

                                        if let Some(item) = params_obj.get("item").cloned() {
                                            let item_type = item
                                                .get("type")
                                                .and_then(|t| t.as_str())
                                                .unwrap_or("")
                                                .to_string();
                                            let item_id = item
                                                .get("id")
                                                .and_then(|i| i.as_str())
                                                .unwrap_or("")
                                                .to_string();

                                            match item_type.as_str() {
                                                "mcpToolCall" => {
                                                    let tool = item
                                                        .get("tool")
                                                        .and_then(|t| t.as_str())
                                                        .unwrap_or("unknown")
                                                        .to_string();
                                                    let server = item
                                                        .get("server")
                                                        .and_then(|s| s.as_str())
                                                        .unwrap_or("")
                                                        .to_string();
                                                    let args = item.get("arguments").cloned();

                                                    print!("  MCP Tool: {}", tool);
                                                    if !server.is_empty() {
                                                        print!(" (server: {})", server);
                                                    }
                                                    println!(" started");
                                                    if let Some(arguments) = &args {
                                                        let args_str = arguments.to_string();
                                                        let (truncated_args, was_truncated) =
                                                            truncate_for_display(&args_str, 200);
                                                        if was_truncated {
                                                            println!(
                                                                "    Args: {}...",
                                                                truncated_args
                                                            );
                                                        } else {
                                                            println!("    Args: {}", args_str);
                                                        }
                                                    }

                                                    let invocation = ToolInvocation {
                                                        id: item_id.clone(),
                                                        run_id: run_id.clone(),
                                                        thread_id: thread_id.clone(),
                                                        tool_name: tool.clone(),
                                                        server: Some(server.clone()),
                                                        arguments: args,
                                                        result: None,
                                                        error: None,
                                                        status: ToolStatus::InProgress,
                                                        duration_ms: None,
                                                        created_at: Utc::now(),
                                                    };
                                                    if let Some(mut session) = lock_or_warn(
                                                        &session_clone,
                                                        "tool invocation started update",
                                                    ) {
                                                        session.add_tool_invocation(
                                                            invocation.clone(),
                                                        );
                                                    }

                                                    let history = history_recorder_clone.clone();
                                                    let history_writer =
                                                        history_writer_clone.clone();
                                                    let tool_id = item_id.clone();
                                                    let tool_for_mcp = invocation.clone();
                                                    let tool_event =
                                                        build_tool_invocation_event(&invocation);
                                                    let tool_event_object_id =
                                                        next_tool_invocation_event_object_id(
                                                            &item_id,
                                                            &tool_event.status,
                                                        );
                                                    let mcp_server_for_tool =
                                                        mcp_server_clone.clone();
                                                    ClientStorage::spawn_background_index_work(
                                                        async move {
                                                            store_to_mcp(
                                                                &mcp_server_for_tool,
                                                                "tool_invocation",
                                                                &tool_id,
                                                                &tool_for_mcp,
                                                                debug_mode,
                                                            )
                                                            .await;
                                                            history
                                                            .event(
                                                                history::EventKind::ToolInvocationStatus,
                                                                &tool_id,
                                                                "in_progress",
                                                                serde_json::json!({"tool": tool, "server": server}),
                                                            )
                                                            .await;
                                                            history_writer
                                                                .write(
                                                                    "tool_invocation_event",
                                                                    &tool_event_object_id,
                                                                    &tool_event,
                                                                )
                                                                .await;
                                                        },
                                                    );
                                                }
                                                "toolCall" => {
                                                    let tool = item
                                                        .get("name")
                                                        .or_else(|| item.get("tool"))
                                                        .and_then(|t| t.as_str())
                                                        .unwrap_or("unknown")
                                                        .to_string();
                                                    let args = item.get("arguments").cloned();
                                                    println!("  Tool: {} started", tool);

                                                    let invocation = ToolInvocation {
                                                        id: item_id.clone(),
                                                        run_id: run_id.clone(),
                                                        thread_id: thread_id.clone(),
                                                        tool_name: tool.clone(),
                                                        server: None,
                                                        arguments: args,
                                                        result: None,
                                                        error: None,
                                                        status: ToolStatus::InProgress,
                                                        duration_ms: None,
                                                        created_at: Utc::now(),
                                                    };
                                                    let tool_id = item_id.clone();
                                                    let tool_for_mcp = invocation.clone();
                                                    if let Some(mut session) = lock_or_warn(
                                                        &session_clone,
                                                        "tool invocation started update",
                                                    ) {
                                                        session.add_tool_invocation(invocation);
                                                    }

                                                    let mcp_server_for_tool =
                                                        mcp_server_clone.clone();
                                                    let history = history_recorder_clone.clone();
                                                    let history_writer =
                                                        history_writer_clone.clone();
                                                    let tool_event =
                                                        build_tool_invocation_event(&tool_for_mcp);
                                                    let tool_event_object_id =
                                                        next_tool_invocation_event_object_id(
                                                            &tool_id,
                                                            &tool_event.status,
                                                        );
                                                    ClientStorage::spawn_background_index_work(
                                                        async move {
                                                            store_to_mcp(
                                                                &mcp_server_for_tool,
                                                                "tool_invocation",
                                                                &tool_id,
                                                                &tool_for_mcp,
                                                                debug_mode,
                                                            )
                                                            .await;
                                                            history
                                                            .event(
                                                                history::EventKind::ToolInvocationStatus,
                                                                &tool_id,
                                                                "in_progress",
                                                                serde_json::json!({"tool": tool}),
                                                            )
                                                            .await;
                                                            history_writer
                                                                .write(
                                                                    "tool_invocation_event",
                                                                    &tool_event_object_id,
                                                                    &tool_event,
                                                                )
                                                                .await;
                                                        },
                                                    );
                                                }
                                                "commandExecution" => {
                                                    let cmd = item
                                                        .get("command")
                                                        .and_then(|c| c.as_str())
                                                        .unwrap_or("")
                                                        .to_string();
                                                    let command_cwd = item
                                                        .get("cwd")
                                                        .and_then(|c| c.as_str())
                                                        .map(String::from)
                                                        .filter(|cwd| !cwd.is_empty())
                                                        .unwrap_or_else(|| {
                                                            default_command_cwd.clone()
                                                        });
                                                    // 在命令执行前捕获工作区快照（文件内容 HashMap），
                                                    // 存入 session.command_baselines，待命令完成后
                                                    // 与执行后快照做 diff，生成 commandExecution 产生的 PatchSet
                                                    let command_snapshot =
                                                        capture_workspace_snapshot(Path::new(
                                                            &command_cwd,
                                                        ));
                                                    println!("  Command: {} started", cmd);

                                                    let invocation = ToolInvocation {
                                                        id: item_id.clone(),
                                                        run_id: run_id.clone(),
                                                        thread_id: thread_id.clone(),
                                                        tool_name: "commandExecution".to_string(),
                                                        server: None,
                                                        arguments: Some(
                                                            serde_json::json!({ "command": cmd }),
                                                        ),
                                                        result: None,
                                                        error: None,
                                                        status: ToolStatus::InProgress,
                                                        duration_ms: item
                                                            .get("durationMs")
                                                            .and_then(|d| d.as_i64()),
                                                        created_at: Utc::now(),
                                                    };
                                                    let cmd_id = item_id.clone();
                                                    let cmd_for_mcp = invocation.clone();
                                                    if let Some(mut session) = lock_or_warn(
                                                        &session_clone,
                                                        "tool invocation started update",
                                                    ) {
                                                        session.add_tool_invocation(invocation);
                                                        session.command_baselines.insert(
                                                            item_id.clone(),
                                                            CommandExecutionBaseline {
                                                                cwd: command_cwd.clone(),
                                                                files: command_snapshot,
                                                            },
                                                        );
                                                    }

                                                    let mcp_server_for_cmd =
                                                        mcp_server_clone.clone();
                                                    let history = history_recorder_clone.clone();
                                                    let history_writer =
                                                        history_writer_clone.clone();
                                                    let tool_event =
                                                        build_tool_invocation_event(&cmd_for_mcp);
                                                    let tool_event_object_id =
                                                        next_tool_invocation_event_object_id(
                                                            &cmd_id,
                                                            &tool_event.status,
                                                        );
                                                    ClientStorage::spawn_background_index_work(
                                                        async move {
                                                            store_to_mcp(
                                                                &mcp_server_for_cmd,
                                                                "tool_invocation",
                                                                &cmd_id,
                                                                &cmd_for_mcp,
                                                                debug_mode,
                                                            )
                                                            .await;
                                                            history
                                                            .event(
                                                                history::EventKind::ToolInvocationStatus,
                                                                &cmd_id,
                                                                "started",
                                                                serde_json::json!({"command": cmd}),
                                                            )
                                                            .await;
                                                            history_writer
                                                                .write(
                                                                    "tool_invocation_event",
                                                                    &tool_event_object_id,
                                                                    &tool_event,
                                                                )
                                                                .await;
                                                        },
                                                    );
                                                }
                                                "reasoning" => {
                                                    println!("  Thinking started");

                                                    let reasoning = Reasoning {
                                                        id: item_id.clone(),
                                                        run_id: run_id.clone(),
                                                        thread_id: thread_id.clone(),
                                                        summary: vec![],
                                                        text: None,
                                                        created_at: Utc::now(),
                                                    };
                                                    let reasoning_id = item_id.clone();
                                                    let reasoning_for_mcp = reasoning.clone();
                                                    if let Some(mut session) = lock_or_warn(
                                                        &session_clone,
                                                        "reasoning started update",
                                                    ) {
                                                        session.add_reasoning(reasoning);
                                                    }

                                                    let mcp_server_for_reasoning =
                                                        mcp_server_clone.clone();
                                                    ClientStorage::spawn_background_index_work(
                                                        async move {
                                                            store_to_mcp(
                                                                &mcp_server_for_reasoning,
                                                                "reasoning",
                                                                &reasoning_id,
                                                                &reasoning_for_mcp,
                                                                debug_mode,
                                                            )
                                                            .await;
                                                        },
                                                    );
                                                }
                                                "plan" => {
                                                    let text = item
                                                        .get("text")
                                                        .and_then(|t| t.as_str())
                                                        .unwrap_or("")
                                                        .to_string();
                                                    if !text.is_empty() {
                                                        println!("  Plan started: {}", text);
                                                    } else {
                                                        println!("  Plan started");
                                                    }

                                                    let plan = Plan {
                                                        id: item_id.clone(),
                                                        text: text.clone(),
                                                        intent_id: None,
                                                        thread_id: thread_id.clone(),
                                                        turn_id: Some(turn_id.clone()),
                                                        status: PlanStatus::InProgress,
                                                        created_at: Utc::now(),
                                                    };
                                                    let plan_id_2 = item_id.clone();
                                                    let plan_for_mcp_2 = plan.clone();
                                                    if let Some(mut session) = lock_or_warn(
                                                        &session_clone,
                                                        "plan started update",
                                                    ) {
                                                        session.add_plan(plan);
                                                    }

                                                    let mcp_server_for_plan_2 =
                                                        mcp_server_clone.clone();
                                                    ClientStorage::spawn_background_index_work(
                                                        async move {
                                                            store_to_mcp(
                                                                &mcp_server_for_plan_2,
                                                                "plan",
                                                                &plan_id_2,
                                                                &plan_for_mcp_2,
                                                                debug_mode,
                                                            )
                                                            .await;
                                                        },
                                                    );
                                                }
                                                "fileChange" => {
                                                    println!("  ?? File Change started");

                                                    let patchset = PatchSet {
                                                        id: item_id.clone(),
                                                        run_id: run_id.clone(),
                                                        thread_id: thread_id.clone(),
                                                        changes: vec![],
                                                        status: PatchStatus::InProgress,
                                                        created_at: Utc::now(),
                                                    };
                                                    let patchset_id = item_id.clone();
                                                    let patchset_for_mcp = patchset.clone();
                                                    if let Some(mut session) = lock_or_warn(
                                                        &session_clone,
                                                        "patchset started update",
                                                    ) {
                                                        session.add_patchset(patchset);
                                                    }

                                                    let mcp_server_for_patchset =
                                                        mcp_server_clone.clone();
                                                    ClientStorage::spawn_background_index_work(
                                                        async move {
                                                            store_to_mcp(
                                                                &mcp_server_for_patchset,
                                                                "patchset",
                                                                &patchset_id,
                                                                &patchset_for_mcp,
                                                                debug_mode,
                                                            )
                                                            .await;
                                                        },
                                                    );
                                                }
                                                "dynamicToolCall" => {
                                                    let tool = item
                                                        .get("tool")
                                                        .and_then(|t| t.as_str())
                                                        .unwrap_or("unknown")
                                                        .to_string();
                                                    let args = item.get("arguments").cloned();
                                                    println!("  Dynamic Tool: {} started", tool);

                                                    let invocation = ToolInvocation {
                                                        id: item_id.clone(),
                                                        run_id: run_id.clone(),
                                                        thread_id: thread_id.clone(),
                                                        tool_name: tool.clone(),
                                                        server: None,
                                                        arguments: args,
                                                        result: None,
                                                        error: None,
                                                        status: ToolStatus::InProgress,
                                                        duration_ms: item
                                                            .get("durationMs")
                                                            .and_then(|d| d.as_i64()),
                                                        created_at: Utc::now(),
                                                    };
                                                    let dyn_tool_id = item_id.clone();
                                                    let dyn_tool_for_mcp = invocation.clone();
                                                    if let Some(mut session) = lock_or_warn(
                                                        &session_clone,
                                                        "tool invocation started update",
                                                    ) {
                                                        session.add_tool_invocation(invocation);
                                                    }

                                                    let mcp_server_for_dyn =
                                                        mcp_server_clone.clone();
                                                    let history_writer =
                                                        history_writer_clone.clone();
                                                    let tool_event = build_tool_invocation_event(
                                                        &dyn_tool_for_mcp,
                                                    );
                                                    let tool_event_object_id =
                                                        next_tool_invocation_event_object_id(
                                                            &dyn_tool_id,
                                                            &tool_event.status,
                                                        );
                                                    ClientStorage::spawn_background_index_work(
                                                        async move {
                                                            store_to_mcp(
                                                                &mcp_server_for_dyn,
                                                                "tool_invocation",
                                                                &dyn_tool_id,
                                                                &dyn_tool_for_mcp,
                                                                debug_mode,
                                                            )
                                                            .await;
                                                            history_writer
                                                                .write(
                                                                    "tool_invocation_event",
                                                                    &tool_event_object_id,
                                                                    &tool_event,
                                                                )
                                                                .await;
                                                        },
                                                    );
                                                }
                                                "webSearch" => {
                                                    let query = item
                                                        .get("query")
                                                        .and_then(|q| q.as_str())
                                                        .unwrap_or("")
                                                        .to_string();
                                                    let action = item.get("action").cloned();
                                                    println!("  Web Search: {}", query);

                                                    let invocation = ToolInvocation {
                                                        id: item_id.clone(),
                                                        run_id: run_id.clone(),
                                                        thread_id: thread_id.clone(),
                                                        tool_name: "webSearch".to_string(),
                                                        server: None,
                                                        arguments: Some(
                                                            serde_json::json!({ "query": query.clone(), "action": action }),
                                                        ),
                                                        result: None,
                                                        error: None,
                                                        status: ToolStatus::InProgress,
                                                        duration_ms: None,
                                                        created_at: Utc::now(),
                                                    };
                                                    let ws_id = item_id.clone();
                                                    let ws_for_mcp = invocation.clone();
                                                    if let Some(mut session) = lock_or_warn(
                                                        &session_clone,
                                                        "tool invocation started update",
                                                    ) {
                                                        session.add_tool_invocation(invocation);
                                                    }

                                                    let mcp_server_for_ws =
                                                        mcp_server_clone.clone();
                                                    let history_writer =
                                                        history_writer_clone.clone();
                                                    let tool_event =
                                                        build_tool_invocation_event(&ws_for_mcp);
                                                    let tool_event_object_id =
                                                        next_tool_invocation_event_object_id(
                                                            &ws_id,
                                                            &tool_event.status,
                                                        );
                                                    ClientStorage::spawn_background_index_work(
                                                        async move {
                                                            store_to_mcp(
                                                                &mcp_server_for_ws,
                                                                "tool_invocation",
                                                                &ws_id,
                                                                &ws_for_mcp,
                                                                debug_mode,
                                                            )
                                                            .await;
                                                            history_writer
                                                                .write(
                                                                    "tool_invocation_event",
                                                                    &tool_event_object_id,
                                                                    &tool_event,
                                                                )
                                                                .await;
                                                        },
                                                    );
                                                }
                                                "userMessage" => {
                                                    let content = item
                                                        .get("content")
                                                        .and_then(|c| c.as_array())
                                                        .and_then(|arr| arr.first())
                                                        .and_then(|first| first.get("text"))
                                                        .and_then(|t| t.as_str())
                                                        .unwrap_or("")
                                                        .to_string();
                                                    let (truncated, _) =
                                                        truncate_for_display(&content, 50);
                                                    println!("  User: {}", truncated);

                                                    // 查找同一 thread 内的上一个 Intent ID，
                                                    // 用于构建 parent_link_event（"continued" 状态），
                                                    // 形成多轮对话的 Intent 因果链
                                                    let parent_intent_id = lock_or_warn(
                                                        &session_clone,
                                                        "intent parent lookup",
                                                    )
                                                    .and_then(|session| {
                                                        latest_thread_intent_id(
                                                            &session,
                                                            &thread_id,
                                                            Some(&item_id),
                                                        )
                                                    });
                                                    let intent = Intent {
                                                        id: item_id.clone(),
                                                        content: content.clone(),
                                                        thread_id: thread_id.clone(),
                                                        created_at: Utc::now(),
                                                    };
                                                    let intent_snapshot = IntentSnapshot {
                                                        id: item_id.clone(),
                                                        content: content.clone(),
                                                        thread_id: thread_id.clone(),
                                                        parents: parent_intent_id
                                                            .clone()
                                                            .into_iter()
                                                            .collect(),
                                                        analysis_context_frames: Vec::new(),
                                                        created_at: Utc::now(),
                                                    };
                                                    let intent_event = IntentEvent {
                                                        id: format!("intent_event_{}", item_id),
                                                        intent_id: item_id.clone(),
                                                        status: "created".to_string(),
                                                        at: Utc::now(),
                                                        next_intent_id: None,
                                                    };
                                                    let parent_link_event = parent_intent_id
                                                        .clone()
                                                        .map(|parent_id| IntentEvent {
                                                            id: format!(
                                                                "intent_event_link_{}_{}",
                                                                parent_id, item_id
                                                            ),
                                                            intent_id: parent_id,
                                                            status: "continued".to_string(),
                                                            at: Utc::now(),
                                                            next_intent_id: Some(item_id.clone()),
                                                        });
                                                    let history_writer =
                                                        history_writer_clone.clone();
                                                    let intent_id_for_write = item_id.clone();
                                                    ClientStorage::spawn_background_index_work(
                                                        async move {
                                                            history_writer
                                                                .write(
                                                                    "intent_snapshot",
                                                                    &intent_id_for_write,
                                                                    &intent_snapshot,
                                                                )
                                                                .await;
                                                            history_writer
                                                                .write(
                                                                    "intent_event",
                                                                    &intent_event.id,
                                                                    &intent_event,
                                                                )
                                                                .await;
                                                            if let Some(link_event) =
                                                                parent_link_event
                                                            {
                                                                history_writer
                                                                    .write(
                                                                        "intent_event",
                                                                        &link_event.id,
                                                                        &link_event,
                                                                    )
                                                                    .await;
                                                            }
                                                        },
                                                    );
                                                    let intent_id = item_id.clone();
                                                    let intent_for_mcp = intent.clone();
                                                    if let Some(mut session) = lock_or_warn(
                                                        &session_clone,
                                                        "intent started update",
                                                    ) {
                                                        session.add_intent(intent);
                                                    }

                                                    let mcp_server_for_intent =
                                                        mcp_server_clone.clone();
                                                    ClientStorage::spawn_background_index_work(
                                                        async move {
                                                            store_to_mcp(
                                                                &mcp_server_for_intent,
                                                                "intent",
                                                                &intent_id,
                                                                &intent_for_mcp,
                                                                debug_mode,
                                                            )
                                                            .await;
                                                        },
                                                    );
                                                }
                                                "agentMessage" => {
                                                    let content = item
                                                        .get("text")
                                                        .and_then(|t| t.as_str())
                                                        .unwrap_or("")
                                                        .to_string();
                                                    println!("Agent Response started");

                                                    let msg = AgentMessage {
                                                        id: item_id.clone(),
                                                        run_id: run_id.clone(),
                                                        thread_id: thread_id.clone(),
                                                        content,
                                                        created_at: Utc::now(),
                                                    };
                                                    if let Some(mut session) = lock_or_warn(
                                                        &session_clone,
                                                        "agent message started update",
                                                    ) {
                                                        session.add_agent_message(msg);
                                                    }
                                                }
                                                "collabAgentToolCall" => {
                                                    let tool = item
                                                        .get("tool")
                                                        .and_then(|t| t.as_str())
                                                        .unwrap_or("unknown")
                                                        .to_string();
                                                    let status = item
                                                        .get("status")
                                                        .and_then(|s| s.as_str())
                                                        .unwrap_or("started")
                                                        .to_string();
                                                    let prompt = item.get("prompt").cloned();
                                                    let sender_thread_id = item
                                                        .get("senderThreadId")
                                                        .and_then(|t| t.as_str())
                                                        .unwrap_or("")
                                                        .to_string();
                                                    let receiver_thread_ids =
                                                        item.get("receiverThreadIds").cloned();
                                                    println!("  Collab Tool: {} started", tool);

                                                    let invocation = ToolInvocation {
                                                        id: item_id.clone(),
                                                        run_id: run_id.clone(),
                                                        thread_id: thread_id.clone(),
                                                        tool_name: format!(
                                                            "collabAgentToolCall:{}",
                                                            tool
                                                        ),
                                                        server: None,
                                                        arguments: Some(serde_json::json!({
                                                            "tool": tool,
                                                            "status": status,
                                                            "prompt": prompt,
                                                            "sender_thread_id": sender_thread_id,
                                                            "receiver_thread_ids": receiver_thread_ids
                                                        })),
                                                        result: None,
                                                        error: None,
                                                        status: ToolStatus::InProgress,
                                                        duration_ms: None,
                                                        created_at: Utc::now(),
                                                    };
                                                    let inv_id = item_id.clone();
                                                    let inv_for_mcp = invocation.clone();
                                                    if let Some(mut session) = lock_or_warn(
                                                        &session_clone,
                                                        "collab tool invocation started update",
                                                    ) {
                                                        session.add_tool_invocation(invocation);
                                                    }

                                                    let mcp_server_for_inv =
                                                        mcp_server_clone.clone();
                                                    let history_writer =
                                                        history_writer_clone.clone();
                                                    let tool_event =
                                                        build_tool_invocation_event(&inv_for_mcp);
                                                    let tool_event_object_id =
                                                        next_tool_invocation_event_object_id(
                                                            &inv_id,
                                                            &tool_event.status,
                                                        );
                                                    ClientStorage::spawn_background_index_work(
                                                        async move {
                                                            store_to_mcp(
                                                                &mcp_server_for_inv,
                                                                "tool_invocation",
                                                                &inv_id,
                                                                &inv_for_mcp,
                                                                debug_mode,
                                                            )
                                                            .await;
                                                            history_writer
                                                                .write(
                                                                    "tool_invocation_event",
                                                                    &tool_event_object_id,
                                                                    &tool_event,
                                                                )
                                                                .await;
                                                        },
                                                    );
                                                }
                                                "enteredReviewMode" => {
                                                    let review_id = item
                                                        .get("review")
                                                        .and_then(|r| r.as_str())
                                                        .unwrap_or("");
                                                    println!(
                                                        "  Entered review mode: {}",
                                                        review_id
                                                    );
                                                }
                                                "exitedReviewMode" => {
                                                    let review_id = item
                                                        .get("review")
                                                        .and_then(|r| r.as_str())
                                                        .unwrap_or("");
                                                    println!("  Exited review mode: {}", review_id);
                                                }
                                                "contextCompaction" => {
                                                    println!("  Context compaction started");
                                                    let context_frame = ContextFrameEvent {
                                                        id: format!(
                                                            "context_frame_started_{}",
                                                            item_id
                                                        ),
                                                        run_id: run_id.clone(),
                                                        plan_id: None,
                                                        step_id: None,
                                                        at: Utc::now(),
                                                        delta: serde_json::json!({
                                                            "kind": "context_compaction",
                                                            "status": "started"
                                                        }),
                                                    };
                                                    let history_writer =
                                                        history_writer_clone.clone();
                                                    let context_frame_id = context_frame.id.clone();
                                                    ClientStorage::spawn_background_index_work(
                                                        async move {
                                                            history_writer
                                                                .write(
                                                                    "context_frame",
                                                                    &context_frame_id,
                                                                    &context_frame,
                                                                )
                                                                .await;
                                                        },
                                                    );
                                                    let snapshot = ContextSnapshot {
                                                        id: item_id.clone(),
                                                        thread_id: thread_id.clone(),
                                                        run_id: Some(run_id.clone()),
                                                        created_at: Utc::now(),
                                                        data: serde_json::json!({}),
                                                    };
                                                    let snapshot_id = snapshot.id.clone();
                                                    let snapshot_for_mcp = snapshot.clone();
                                                    let mcp_server_for_snapshot =
                                                        mcp_server_clone.clone();
                                                    ClientStorage::spawn_background_index_work(
                                                        async move {
                                                            store_to_mcp(
                                                                &mcp_server_for_snapshot,
                                                                "context_snapshot",
                                                                &snapshot_id,
                                                                &snapshot_for_mcp,
                                                                debug_mode,
                                                            )
                                                            .await;
                                                        },
                                                    );
                                                }
                                                _ => {
                                                    println!("  Task: {} started", item_type);
                                                }
                                            }
                                        }
                                    }
                                }
                                // --- Handle ItemCompleted：处理各类子项目完成事件 ---
                                // 对应 ItemStarted 中的每种 item.type，在完成时：
                                //   - "mcpToolCall"/"toolCall"：更新 ToolInvocation 状态、result、duration
                                //   - "commandExecution"：比较执行前后工作区快照，生成文件变更 PatchSet；
                                //     若命令失败则写入 RunEvent failed 和 DecisionEvent（不批准）
                                //   - "reasoning"：更新 Reasoning 的 summary 和 text
                                //   - "plan"：将 Plan 标记为 Completed
                                //   - "fileChange"：合并流式 diff 与完整 changes 列表，
                                //     持久化 PatchSetSnapshot 和 EvidenceEvent
                                //   - "agentMessage"：补全流式 delta 后的完整消息内容，打印到 stdout
                                else if matches!(mk, MethodKind::ItemCompleted) {
                                    if let Some(params_obj) = json.get("params").cloned() {
                                        let _thread_id = params_obj
                                            .get("threadId")
                                            .and_then(|t| t.as_str())
                                            .unwrap_or("")
                                            .to_string();
                                        let _turn_id = params_obj
                                            .get("turnId")
                                            .and_then(|t| t.as_str())
                                            .unwrap_or("")
                                            .to_string();

                                        if let Some(item) = params_obj.get("item").cloned() {
                                            let item_type = item
                                                .get("type")
                                                .and_then(|t| t.as_str())
                                                .unwrap_or("")
                                                .to_string();
                                            let item_id = item
                                                .get("id")
                                                .and_then(|i| i.as_str())
                                                .unwrap_or("")
                                                .to_string();

                                            match item_type.as_str() {
                                                "mcpToolCall" => {
                                                    let tool = item
                                                        .get("tool")
                                                        .and_then(|t| t.as_str())
                                                        .unwrap_or("unknown")
                                                        .to_string();
                                                    let status = item
                                                        .get("status")
                                                        .and_then(|s| s.as_str())
                                                        .unwrap_or("completed")
                                                        .to_string();
                                                    let result = item.get("result").cloned();
                                                    let error = item
                                                        .get("error")
                                                        .and_then(|e| e.as_str())
                                                        .map(String::from);
                                                    let duration_ms = item
                                                        .get("durationMs")
                                                        .and_then(|d| d.as_i64());

                                                    print!("  MCP Tool: {} - {}", tool, status);
                                                    if let Some(result_val) = result.as_ref() {
                                                        let result_str = result_val.to_string();
                                                        let (truncated_result, was_truncated) =
                                                            truncate_for_display(&result_str, 100);
                                                        if was_truncated {
                                                            println!(
                                                                " | Result: {}...",
                                                                truncated_result
                                                            );
                                                        } else if !result_str.is_empty()
                                                            && result_str != "null"
                                                        {
                                                            println!(" | Result: {}", result_str);
                                                        } else {
                                                            println!();
                                                        }
                                                    } else if let Some(error_val) = error.as_ref() {
                                                        println!(" | Error: {}", error_val);
                                                    } else {
                                                        println!();
                                                    }

                                                    let updated_inv = if let Some(mut session) =
                                                        lock_or_warn(
                                                            &session_clone,
                                                            "tool invocation completed update",
                                                        ) {
                                                        if let Some(invocation) = session
                                                            .tool_invocations
                                                            .iter_mut()
                                                            .find(|i| i.id == item_id)
                                                        {
                                                            invocation.status =
                                                                match status.as_str() {
                                                                    "completed" => {
                                                                        ToolStatus::Completed
                                                                    }
                                                                    "failed" => ToolStatus::Failed,
                                                                    _ => ToolStatus::Completed,
                                                                };
                                                            invocation.result = result.clone();
                                                            invocation.error = error.clone();
                                                            invocation.duration_ms = duration_ms;
                                                        }
                                                        session
                                                            .tool_invocations
                                                            .iter()
                                                            .find(|i| i.id == item_id)
                                                            .cloned()
                                                    } else {
                                                        None
                                                    };

                                                    if let Some(inv) = updated_inv {
                                                        let mcp_server_for_inv =
                                                            mcp_server_clone.clone();
                                                        let history =
                                                            history_recorder_clone.clone();
                                                        let history_writer =
                                                            history_writer_clone.clone();
                                                        let inv_id = inv.id.clone();
                                                        let tool_name = tool.clone();
                                                        let tool_event =
                                                            build_tool_invocation_event(&inv);
                                                        let tool_event_object_id =
                                                            next_tool_invocation_event_object_id(
                                                                &inv_id,
                                                                &tool_event.status,
                                                            );
                                                        ClientStorage::spawn_background_index_work(
                                                            async move {
                                                                store_to_mcp(
                                                                    &mcp_server_for_inv,
                                                                    "tool_invocation",
                                                                    &inv_id,
                                                                    &inv,
                                                                    debug_mode,
                                                                )
                                                                .await;
                                                                history
                                                                .event(
                                                                    history::EventKind::ToolInvocationStatus,
                                                                    &inv_id,
                                                                    inv.status.to_string(),
                                                                    serde_json::json!({
                                                                        "tool": tool_name,
                                                                        "duration_ms": duration_ms,
                                                                        "error": error,
                                                                        "result": result
                                                                    }),
                                                                )
                                                                .await;
                                                                history_writer
                                                                    .write(
                                                                        "tool_invocation_event",
                                                                        &tool_event_object_id,
                                                                        &tool_event,
                                                                    )
                                                                    .await;
                                                            },
                                                        );
                                                    }
                                                }
                                                "commandExecution" => {
                                                    let cmd = item
                                                        .get("command")
                                                        .and_then(|c| c.as_str())
                                                        .unwrap_or("")
                                                        .to_string();
                                                    let command_cwd_from_item = item
                                                        .get("cwd")
                                                        .and_then(|c| c.as_str())
                                                        .map(String::from)
                                                        .filter(|cwd| !cwd.is_empty());
                                                    let exit_code = item
                                                        .get("exitCode")
                                                        .and_then(|c| c.as_i64());
                                                    let duration_ms = item
                                                        .get("durationMs")
                                                        .and_then(|d| d.as_i64());
                                                    let output = item
                                                        .get("aggregatedOutput")
                                                        .and_then(|o| o.as_str())
                                                        .map(String::from);

                                                    println!(
                                                        "  Command: {} exit={:?}",
                                                        cmd, exit_code
                                                    );

                                                    // 取出命令执行前保存的工作区快照（ItemStarted 时存入），
                                                    // 用于与执行后快照做 diff，生成 commandExecution 产生的 PatchSet
                                                    let command_baseline = if let Some(
                                                        mut session,
                                                    ) = lock_or_warn(
                                                        &session_clone,
                                                        "command execution baseline read",
                                                    ) {
                                                        session.command_baselines.remove(&item_id)
                                                    } else {
                                                        None
                                                    };

                                                    let updated_invocation = if let Some(
                                                        mut session,
                                                    ) = lock_or_warn(
                                                        &session_clone,
                                                        "command execution completed update",
                                                    ) {
                                                        if let Some(invocation) = session
                                                            .tool_invocations
                                                            .iter_mut()
                                                            .find(|i| i.id == item_id)
                                                        {
                                                            invocation.status = match exit_code {
                                                                Some(0) => ToolStatus::Completed,
                                                                Some(_) => ToolStatus::Failed,
                                                                None => ToolStatus::Completed,
                                                            };
                                                            invocation.result = output
                                                                .as_ref()
                                                                .map(|o| serde_json::json!({ "output": o }));
                                                            invocation.duration_ms = duration_ms;
                                                            Some(invocation.clone())
                                                        } else {
                                                            None
                                                        }
                                                    } else {
                                                        None
                                                    };

                                                    if let Some(inv) = updated_invocation {
                                                        let invocation_status = inv.status.clone();
                                                        let patchset_status_string =
                                                            match invocation_status {
                                                                ToolStatus::Completed => {
                                                                    "completed".to_string()
                                                                }
                                                                ToolStatus::Failed => {
                                                                    "failed".to_string()
                                                                }
                                                                ToolStatus::InProgress => {
                                                                    "in_progress".to_string()
                                                                }
                                                                ToolStatus::Pending => {
                                                                    "pending".to_string()
                                                                }
                                                            };
                                                        let command_patchset =
                                                            command_baseline.and_then(
                                                                |baseline| {
                                                                    let effective_cwd =
                                                                        command_cwd_from_item
                                                                            .clone()
                                                                            .unwrap_or(
                                                                                baseline.cwd,
                                                                            );
                                                                    let after_snapshot =
                                                                        capture_workspace_snapshot(
                                                                            Path::new(
                                                                                &effective_cwd,
                                                                            ),
                                                                        );
                                                                    let changes =
                                                                        build_file_changes_from_snapshots(
                                                                            &baseline.files,
                                                                            &after_snapshot,
                                                                        );
                                                                    if changes.is_empty() {
                                                                        None
                                                                    } else {
                                                                        Some(PatchSet {
                                                                            id: format!(
                                                                                "command_patchset_{}",
                                                                                item_id
                                                                            ),
                                                                            run_id: _turn_id
                                                                                .to_string(),
                                                                            thread_id: _thread_id
                                                                                .to_string(),
                                                                            changes,
                                                                            status: match invocation_status
                                                                            {
                                                                                ToolStatus::Completed => {
                                                                                    PatchStatus::Completed
                                                                                }
                                                                                ToolStatus::Failed => {
                                                                                    PatchStatus::Failed
                                                                                }
                                                                                _ => {
                                                                                    PatchStatus::Pending
                                                                                }
                                                                            },
                                                                            created_at: Utc::now(),
                                                                        })
                                                                    }
                                                                },
                                                            );
                                                        let mcp_server_for_inv =
                                                            mcp_server_clone.clone();
                                                        let history =
                                                            history_recorder_clone.clone();
                                                        let history_writer =
                                                            history_writer_clone.clone();
                                                        let inv_id = item_id.clone();
                                                        let cmd_name = cmd.clone();
                                                        let tool_event =
                                                            build_tool_invocation_event(&inv);
                                                        let tool_event_object_id =
                                                            next_tool_invocation_event_object_id(
                                                                &inv_id,
                                                                &tool_event.status,
                                                            );
                                                        let decision = DecisionEvent {
                                                            id: format!("decision_{}", inv_id),
                                                            run_id: _turn_id.to_string(),
                                                            chosen_patchset_id: None,
                                                            approved: inv.status
                                                                == ToolStatus::Completed,
                                                            at: Utc::now(),
                                                            rationale: None,
                                                        };
                                                        let run_event_failed = RunEvent {
                                                            id: format!(
                                                                "run_event_{}_failed",
                                                                _turn_id
                                                            ),
                                                            run_id: _turn_id.to_string(),
                                                            status: "failed".to_string(),
                                                            at: Utc::now(),
                                                            error: if inv.status
                                                                == ToolStatus::Failed
                                                            {
                                                                Some(
                                                                    "command_execution_failed"
                                                                        .to_string(),
                                                                )
                                                            } else {
                                                                None
                                                            },
                                                        };
                                                        ClientStorage::spawn_background_index_work(
                                                            async move {
                                                                store_to_mcp(
                                                                    &mcp_server_for_inv,
                                                                    "tool_invocation",
                                                                    &inv_id,
                                                                    &inv,
                                                                    debug_mode,
                                                                )
                                                                .await;
                                                                history
                                                                .event(
                                                                    history::EventKind::ToolInvocationStatus,
                                                                    &inv_id,
                                                                    inv.status.to_string(),
                                                                    serde_json::json!({"command": cmd_name, "exit": exit_code}),
                                                                )
                                                                .await;
                                                                history_writer
                                                                    .write(
                                                                        "tool_invocation_event",
                                                                        &tool_event_object_id,
                                                                        &tool_event,
                                                                    )
                                                                    .await;
                                                                history_writer
                                                                    .write(
                                                                        "decision",
                                                                        &decision.id,
                                                                        &decision,
                                                                    )
                                                                    .await;
                                                                if inv.status == ToolStatus::Failed
                                                                {
                                                                    history_writer
                                                                        .write(
                                                                            "run_event",
                                                                            &run_event_failed.id,
                                                                            &run_event_failed,
                                                                        )
                                                                        .await;
                                                                }
                                                            },
                                                        );

                                                        if let Some(patchset) =
                                                            command_patchset.clone()
                                                        {
                                                            if let Some(mut session) = lock_or_warn(
                                                                &session_clone,
                                                                "command patchset update",
                                                            ) {
                                                                session
                                                                    .add_patchset(patchset.clone());
                                                            }
                                                            persist_patchset_snapshot_and_evidence(
                                                                mcp_server_clone.clone(),
                                                                history_recorder_clone.clone(),
                                                                history_writer_clone.clone(),
                                                                patchset,
                                                                patchset_status_string,
                                                                debug_mode,
                                                            );
                                                        }
                                                    }
                                                }
                                                "reasoning" => {
                                                    println!("  Thinking completed");

                                                    let summary = item
                                                        .get("summary")
                                                        .and_then(|s| s.as_array())
                                                        .map(|arr| {
                                                            arr.iter()
                                                                .filter_map(|v| {
                                                                    v.as_str().map(String::from)
                                                                })
                                                                .collect()
                                                        })
                                                        .unwrap_or_default();
                                                    let text = item
                                                        .get("text")
                                                        .and_then(|t| t.as_str())
                                                        .map(String::from);

                                                    if let Some(mut session) = lock_or_warn(
                                                        &session_clone,
                                                        "reasoning completed update",
                                                    ) && let Some(reasoning) = session
                                                        .reasonings
                                                        .iter_mut()
                                                        .find(|r| r.id == item_id)
                                                    {
                                                        reasoning.summary = summary;
                                                        reasoning.text = text;
                                                    }
                                                }
                                                "plan" => {
                                                    let text = item
                                                        .get("text")
                                                        .and_then(|t| t.as_str())
                                                        .unwrap_or("")
                                                        .to_string();
                                                    if !text.is_empty() {
                                                        println!("  Plan completed: {}", text);
                                                    } else {
                                                        println!("  Plan completed");
                                                    }

                                                    if let Some(mut session) = lock_or_warn(
                                                        &session_clone,
                                                        "plan completed update",
                                                    ) && let Some(plan) = session
                                                        .plans
                                                        .iter_mut()
                                                        .find(|p| p.id == item_id)
                                                    {
                                                        plan.status = PlanStatus::Completed;
                                                        if !text.is_empty() {
                                                            plan.text = text;
                                                        }
                                                    }
                                                }
                                                "fileChange" => {
                                                    let status = item
                                                        .get("status")
                                                        .and_then(|s| s.as_str())
                                                        .unwrap_or("")
                                                        .to_string();

                                                    if debug_mode {
                                                        eprintln!(
                                                            "[DEBUG] fileChange item: {:?}",
                                                            item
                                                        );
                                                    }

                                                    let changes: Vec<FileChange> =
                                                        parse_patchset_changes_from_array(
                                                            item.get("changes"),
                                                        );

                                                    if debug_mode {
                                                        eprintln!(
                                                            "[DEBUG] fileChange changes parsed: {} items",
                                                            changes.len()
                                                        );
                                                    }

                                                    let file_count = changes.len();
                                                    println!(
                                                        "  ?? File Change {} ({} files)",
                                                        status, file_count
                                                    );

                                                    for change in changes.iter().take(3) {
                                                        println!(
                                                            "    - {} ({})",
                                                            change.path, change.change_type
                                                        );
                                                        if !change.diff.is_empty() {
                                                            let diff_lines: Vec<&str> = change
                                                                .diff
                                                                .lines()
                                                                .take(10)
                                                                .collect();
                                                            for line in diff_lines {
                                                                println!("      {}", line);
                                                            }
                                                            if change.diff.lines().count() > 10 {
                                                                println!(
                                                                    "      ... ({} more lines)",
                                                                    change.diff.lines().count()
                                                                        - 10
                                                                );
                                                            }
                                                        }
                                                    }
                                                    if file_count > 3 {
                                                        println!(
                                                            "    ... and {} more files",
                                                            file_count - 3
                                                        );
                                                    }

                                                    if let Some(mut session) = lock_or_warn(
                                                        &session_clone,
                                                        "patchset completed update",
                                                    ) {
                                                        let patchset_status =
                                                            patch_status_from_str(&status);
                                                        if let Some(patchset) = session
                                                            .patchsets
                                                            .iter_mut()
                                                            .find(|p| p.id == item_id)
                                                        {
                                                            let merged_changes =
                                                                merge_patchset_changes(
                                                                    &patchset.changes,
                                                                    &changes,
                                                                );
                                                            patchset.status = patchset_status;
                                                            patchset.changes = merged_changes;
                                                        } else {
                                                            session.add_patchset(PatchSet {
                                                                id: item_id.clone(),
                                                                run_id: _turn_id.to_string(),
                                                                thread_id: _thread_id.to_string(),
                                                                changes: changes.clone(),
                                                                status: patchset_status,
                                                                created_at: Utc::now(),
                                                            });
                                                        }
                                                    }

                                                    let patchset_to_store = if let Some(session) =
                                                        lock_or_warn(
                                                            &session_clone,
                                                            "patchset completed read",
                                                        ) {
                                                        session
                                                            .patchsets
                                                            .iter()
                                                            .find(|p| p.id == item_id)
                                                            .cloned()
                                                    } else {
                                                        None
                                                    };

                                                    if let Some(patchset) = patchset_to_store {
                                                        let patchset = PatchSet {
                                                            run_id: if patchset.run_id.is_empty() {
                                                                _turn_id.to_string()
                                                            } else {
                                                                patchset.run_id.clone()
                                                            },
                                                            thread_id: if patchset
                                                                .thread_id
                                                                .is_empty()
                                                            {
                                                                _thread_id.to_string()
                                                            } else {
                                                                patchset.thread_id.clone()
                                                            },
                                                            ..patchset
                                                        };
                                                        persist_patchset_snapshot_and_evidence(
                                                            mcp_server_clone.clone(),
                                                            history_recorder_clone.clone(),
                                                            history_writer_clone.clone(),
                                                            patchset,
                                                            status.clone(),
                                                            debug_mode,
                                                        );
                                                    }
                                                }
                                                "toolCall" => {
                                                    let tool = item
                                                        .get("name")
                                                        .or_else(|| item.get("tool"))
                                                        .and_then(|t| t.as_str())
                                                        .unwrap_or("unknown")
                                                        .to_string();
                                                    let status = item
                                                        .get("status")
                                                        .and_then(|s| s.as_str())
                                                        .unwrap_or("completed")
                                                        .to_string();
                                                    let result = item.get("result").cloned();
                                                    let error = item
                                                        .get("error")
                                                        .and_then(|e| e.as_str())
                                                        .map(String::from);
                                                    let duration_ms = item
                                                        .get("durationMs")
                                                        .and_then(|d| d.as_i64());

                                                    print!("  Tool: {} - {}", tool, status);
                                                    if let Some(result_val) = result.as_ref() {
                                                        let result_str = result_val.to_string();
                                                        let (truncated_result, was_truncated) =
                                                            truncate_for_display(&result_str, 100);
                                                        if was_truncated {
                                                            println!(
                                                                " | Result: {}...",
                                                                truncated_result
                                                            );
                                                        } else if !result_str.is_empty()
                                                            && result_str != "null"
                                                        {
                                                            println!(" | Result: {}", result_str);
                                                        } else {
                                                            println!();
                                                        }
                                                    } else {
                                                        println!();
                                                    }

                                                    if let Some(mut session) = lock_or_warn(
                                                        &session_clone,
                                                        "tool invocation completed update",
                                                    ) && let Some(invocation) = session
                                                        .tool_invocations
                                                        .iter_mut()
                                                        .find(|i| i.id == item_id)
                                                    {
                                                        invocation.status = match status.as_str() {
                                                            "completed" => ToolStatus::Completed,
                                                            "failed" => ToolStatus::Failed,
                                                            _ => ToolStatus::Completed,
                                                        };
                                                        invocation.result = result.clone();
                                                        invocation.error = error.clone();
                                                        invocation.duration_ms = duration_ms;
                                                    }

                                                    let invocation_to_store = if let Some(session) =
                                                        lock_or_warn(
                                                            &session_clone,
                                                            "tool invocation completed read",
                                                        ) {
                                                        session
                                                            .tool_invocations
                                                            .iter()
                                                            .find(|i| i.id == item_id)
                                                            .cloned()
                                                    } else {
                                                        None
                                                    };

                                                    if let Some(invocation) = invocation_to_store {
                                                        let mcp_server_for_inv =
                                                            mcp_server_clone.clone();
                                                        let history_writer =
                                                            history_writer_clone.clone();
                                                        let inv_id = item_id.clone();
                                                        let tool_event =
                                                            build_tool_invocation_event(
                                                                &invocation,
                                                            );
                                                        let tool_event_object_id =
                                                            next_tool_invocation_event_object_id(
                                                                &inv_id,
                                                                &tool_event.status,
                                                            );
                                                        ClientStorage::spawn_background_index_work(
                                                            async move {
                                                                store_to_mcp(
                                                                    &mcp_server_for_inv,
                                                                    "tool_invocation",
                                                                    &inv_id,
                                                                    &invocation,
                                                                    debug_mode,
                                                                )
                                                                .await;
                                                                history_writer
                                                                    .write(
                                                                        "tool_invocation_event",
                                                                        &tool_event_object_id,
                                                                        &tool_event,
                                                                    )
                                                                    .await;
                                                            },
                                                        );
                                                    }
                                                }
                                                "collabAgentToolCall" => {
                                                    let status = item
                                                        .get("status")
                                                        .and_then(|s| s.as_str())
                                                        .unwrap_or("completed")
                                                        .to_string();
                                                    let mut updated = None;
                                                    if let Some(mut session) = lock_or_warn(
                                                        &session_clone,
                                                        "collab tool invocation completed update",
                                                    ) && let Some(invocation) = session
                                                        .tool_invocations
                                                        .iter_mut()
                                                        .find(|i| i.id == item_id)
                                                    {
                                                        invocation.status = match status.as_str() {
                                                            "completed" => ToolStatus::Completed,
                                                            "failed" => ToolStatus::Failed,
                                                            _ => ToolStatus::Completed,
                                                        };
                                                        updated = Some(invocation.clone());
                                                    }
                                                    if let Some(invocation) = updated {
                                                        let mcp_server_for_inv =
                                                            mcp_server_clone.clone();
                                                        let history_writer =
                                                            history_writer_clone.clone();
                                                        let inv_id = item_id.clone();
                                                        let tool_event =
                                                            build_tool_invocation_event(
                                                                &invocation,
                                                            );
                                                        let tool_event_object_id =
                                                            next_tool_invocation_event_object_id(
                                                                &inv_id,
                                                                &tool_event.status,
                                                            );
                                                        ClientStorage::spawn_background_index_work(
                                                            async move {
                                                                store_to_mcp(
                                                                    &mcp_server_for_inv,
                                                                    "tool_invocation",
                                                                    &inv_id,
                                                                    &invocation,
                                                                    debug_mode,
                                                                )
                                                                .await;
                                                                history_writer
                                                                    .write(
                                                                        "tool_invocation_event",
                                                                        &tool_event_object_id,
                                                                        &tool_event,
                                                                    )
                                                                    .await;
                                                            },
                                                        );
                                                    }
                                                }
                                                "webSearch" => {
                                                    let status = item
                                                        .get("status")
                                                        .and_then(|s| s.as_str())
                                                        .unwrap_or("completed")
                                                        .to_string();
                                                    let updated_invocation = if let Some(
                                                        mut session,
                                                    ) = lock_or_warn(
                                                        &session_clone,
                                                        "web search completed update",
                                                    ) && let Some(
                                                        invocation,
                                                    ) = session
                                                        .tool_invocations
                                                        .iter_mut()
                                                        .find(|i| i.id == item_id)
                                                    {
                                                        invocation.status = match status.as_str() {
                                                            "completed" => ToolStatus::Completed,
                                                            "failed" => ToolStatus::Failed,
                                                            _ => ToolStatus::Completed,
                                                        };
                                                        Some(invocation.clone())
                                                    } else {
                                                        None
                                                    };
                                                    if let Some(invocation) = updated_invocation {
                                                        let mcp_server_for_inv =
                                                            mcp_server_clone.clone();
                                                        let history_writer =
                                                            history_writer_clone.clone();
                                                        let inv_id = item_id.clone();
                                                        let tool_event =
                                                            build_tool_invocation_event(
                                                                &invocation,
                                                            );
                                                        let tool_event_object_id =
                                                            next_tool_invocation_event_object_id(
                                                                &inv_id,
                                                                &tool_event.status,
                                                            );
                                                        ClientStorage::spawn_background_index_work(
                                                            async move {
                                                                store_to_mcp(
                                                                    &mcp_server_for_inv,
                                                                    "tool_invocation",
                                                                    &inv_id,
                                                                    &invocation,
                                                                    debug_mode,
                                                                )
                                                                .await;
                                                                history_writer
                                                                    .write(
                                                                        "tool_invocation_event",
                                                                        &tool_event_object_id,
                                                                        &tool_event,
                                                                    )
                                                                    .await;
                                                            },
                                                        );
                                                    }
                                                }
                                                "contextCompaction" => {
                                                    println!("  Context compaction completed");
                                                    let context_frame = ContextFrameEvent {
                                                        id: format!(
                                                            "context_frame_completed_{}",
                                                            item_id
                                                        ),
                                                        run_id: _turn_id.to_string(),
                                                        plan_id: None,
                                                        step_id: None,
                                                        at: Utc::now(),
                                                        delta: serde_json::json!({
                                                            "kind": "context_compaction",
                                                            "status": "completed"
                                                        }),
                                                    };
                                                    let history_writer =
                                                        history_writer_clone.clone();
                                                    let context_frame_id = context_frame.id.clone();
                                                    ClientStorage::spawn_background_index_work(
                                                        async move {
                                                            history_writer
                                                                .write(
                                                                    "context_frame",
                                                                    &context_frame_id,
                                                                    &context_frame,
                                                                )
                                                                .await;
                                                        },
                                                    );
                                                }
                                                "userMessage" => {
                                                    println!("  User message completed");
                                                }
                                                "agentMessage" => {
                                                    if debug_mode {
                                                        eprintln!(
                                                            "[DEBUG] agentMessage completed item: {:?}",
                                                            item
                                                        );
                                                    }
                                                    let content = item
                                                        .get("text")
                                                        .and_then(|t| t.as_str())
                                                        .unwrap_or("")
                                                        .to_string();
                                                    let previous_content = if let Some(mut session) =
                                                        lock_or_warn(
                                                            &session_clone,
                                                            "agent message completed update",
                                                        )
                                                        && let Some(msg) = session
                                                            .agent_messages
                                                            .iter_mut()
                                                            .find(|m| m.id == item_id)
                                                    {
                                                        let previous_content = msg.content.clone();
                                                        msg.content = content.clone();
                                                        previous_content
                                                    } else {
                                                        String::new()
                                                    };
                                                    if !content.is_empty() {
                                                        if previous_content.is_empty() {
                                                            println!("Agent: {}", content);
                                                        } else if let Some(suffix) =
                                                            content.strip_prefix(&previous_content)
                                                        {
                                                            if !suffix.is_empty() {
                                                                print!("{}", suffix);
                                                                if !content.ends_with('\n') {
                                                                    println!();
                                                                }
                                                            }
                                                        } else {
                                                            println!("Agent: {}", content);
                                                        }
                                                    }
                                                    println!("Agent Response completed");
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                // --- Handle RequestApproval*：审批请求流程 ---
                                // Codex 在执行某些敏感操作（命令执行、文件变更、apply_patch）前，
                                // 会通过此通知请求用户授权。整体流程如下：
                                //
                                //   1. 解析请求参数（request_id、approval_type、command/changes/description）
                                //   2. 构建 ApprovalRequest 并存入 session + MCP storage
                                //   3. 根据 approval_mode 决策：
                                //        - "accept"：自动批准，不打断用户
                                //        - "decline"：自动拒绝，不打断用户
                                //        - "ask"：通过 approval_tx 发送给主循环，由主循环提示用户输入
                                //            （用户可输入 a/d/aa/dd 分别表示 accept/decline/accept all/decline all）
                                //   4. 更新 ApprovalRequest.decision 并再次写入 MCP storage
                                //   5. 写入 DecisionEvent 到历史（便于溯源审批决策）
                                //   6. 通过 `send_request` 向 Codex 发送对应的 resolve 方法
                                //      （不同 approval_type 对应不同的 resolve method 字符串）
                                } else if matches!(
                                    mk,
                                    MethodKind::RequestApproval
                                        | MethodKind::RequestApprovalCommandExecution
                                        | MethodKind::RequestApprovalFileChange
                                        | MethodKind::RequestApprovalApplyPatch
                                        | MethodKind::RequestApprovalExec
                                ) {
                                    // Get request ID
                                    let request_id = params
                                        .get("requestId")
                                        .or_else(|| params.get("request_id"))
                                        .and_then(|v| v.as_str())
                                        .map(String::from)
                                        .unwrap_or_else(|| {
                                            format!("req_{}", Utc::now().timestamp_millis())
                                        });
                                    let approval_params = json
                                        .get("params")
                                        .cloned()
                                        .unwrap_or(serde_json::json!({}));

                                    // Determine approval type
                                    let approval_type = match mk {
                                        MethodKind::RequestApprovalCommandExecution => {
                                            ApprovalType::CommandExecution
                                        }
                                        MethodKind::RequestApprovalFileChange => {
                                            ApprovalType::FileChange
                                        }
                                        MethodKind::RequestApprovalApplyPatch => {
                                            ApprovalType::ApplyPatch
                                        }
                                        MethodKind::RequestApprovalExec => ApprovalType::Unknown,
                                        _ => ApprovalType::Unknown,
                                    };

                                    // Get item_id if available
                                    let item_id = approval_params
                                        .get("itemId")
                                        .or_else(|| approval_params.get("call_id"))
                                        .or_else(|| approval_params.get("callId"))
                                        .and_then(|v| v.as_str())
                                        .map(String::from)
                                        .unwrap_or_default();

                                    // Get thread_id if available
                                    let thread_id = approval_params
                                        .get("threadId")
                                        .or_else(|| approval_params.get("conversationId"))
                                        .and_then(|v| v.as_str())
                                        .map(String::from)
                                        .unwrap_or_default();

                                    // Get command or changes from approval_params
                                    let command = approval_params
                                        .get("command")
                                        .and_then(|v| v.as_str())
                                        .map(String::from);
                                    let approval_patch_changes = parse_patchset_changes_from_map(
                                        approval_params
                                            .get("fileChanges")
                                            .or_else(|| approval_params.get("changes")),
                                    );
                                    let changes = if approval_patch_changes.is_empty() {
                                        approval_params
                                            .get("changes")
                                            .and_then(|c| c.as_array())
                                            .map(|arr| {
                                                arr.iter()
                                                    .filter_map(|v| v.as_str().map(String::from))
                                                    .collect()
                                            })
                                    } else {
                                        Some(
                                            approval_patch_changes
                                                .iter()
                                                .map(|change| change.path.clone())
                                                .collect(),
                                        )
                                    };
                                    let description: Option<String> = approval_params
                                        .get("description")
                                        .and_then(|v| v.as_str())
                                        .map(String::from);

                                    // Store approval request in session
                                    let approval_request = ApprovalRequest {
                                        id: request_id.clone(),
                                        approval_type: approval_type.clone(),
                                        item_id: item_id.clone(),
                                        thread_id: thread_id.clone(),
                                        run_id: None,
                                        command,
                                        changes,
                                        description: description.clone(),
                                        decision: None,
                                        requested_at: Utc::now(),
                                        resolved_at: None,
                                    };
                                    let approval_id = request_id.clone();
                                    let approval_for_mcp = approval_request.clone();
                                    if let Some(mut session) =
                                        lock_or_warn(&session_clone, "approval request update")
                                    {
                                        session.add_approval_request(approval_request);
                                    }

                                    // Store to MCP in background
                                    let mcp_server_for_approval = mcp_server_clone.clone();
                                    ClientStorage::spawn_background_index_work(async move {
                                        store_to_mcp(
                                            &mcp_server_for_approval,
                                            "approval_request",
                                            &approval_id,
                                            &approval_for_mcp,
                                            debug_mode,
                                        )
                                        .await;
                                    });

                                    // 根据当前 approval_mode 决定批准/拒绝策略：
                                    //   "accept" → 自动批准（无人机模式）
                                    //   "decline" → 自动拒绝
                                    //   其他（"ask"）→ 通过 oneshot channel 将审批请求转发给主循环，
                                    //     主循环从 stdin 读取用户输入后通过 oneshot 应答
                                    let current_mode =
                                        lock_or_warn(&approval_mode, "approval mode read")
                                            .map(|mode| mode.clone())
                                            .unwrap_or_else(|| "ask".to_string());
                                    let approved = if current_mode == "accept" {
                                        // 自动批准：无需用户交互
                                        println!("[Auto-approved]");
                                        true
                                    } else if current_mode == "decline" {
                                        // 自动拒绝：无需用户交互
                                        println!("[Auto-declined]");
                                        false
                                    } else {
                                        // Ask 模式：通过 oneshot channel 将审批参数转发给主循环，
                                        // 主循环负责提示用户并将结果通过 oneshot sender 回传
                                        let (oneshot_tx, oneshot_rx) =
                                            tokio::sync::oneshot::channel::<bool>();
                                        let _ = approval_tx_clone
                                            .send((approval_params.clone(), oneshot_tx))
                                            .await;

                                        // 等待主循环的用户输入结果（超时则默认批准）
                                        match oneshot_rx.await {
                                            Ok(approved) => {
                                                println!(
                                                    "[User {}]",
                                                    if approved { "approved" } else { "declined" }
                                                );
                                                approved
                                            }
                                            Err(_) => {
                                                println!("[Timeout - auto-approved by default]");
                                                true
                                            }
                                        }
                                    };

                                    // Update approval request with decision and persist to MCP
                                    let approval_to_store = if let Some(mut session) =
                                        lock_or_warn(&session_clone, "approval decision update")
                                    {
                                        if let Some(approval) = session
                                            .approval_requests
                                            .iter_mut()
                                            .find(|a| a.id == request_id)
                                        {
                                            approval.decision = Some(approved);
                                            approval.resolved_at = Some(Utc::now());
                                            Some(approval.clone())
                                        } else {
                                            None
                                        }
                                    } else {
                                        None
                                    };

                                    // Store updated approval to MCP
                                    if let Some(approval) = approval_to_store {
                                        let approval_id = approval.id.clone();
                                        let mcp_server_for_approval = mcp_server_clone.clone();
                                        ClientStorage::spawn_background_index_work(async move {
                                            store_to_mcp(
                                                &mcp_server_for_approval,
                                                "approval_request",
                                                &approval_id,
                                                &approval,
                                                debug_mode,
                                            )
                                            .await;
                                        });
                                    }

                                    let (run_id_for_decision, chosen_patchset_id) =
                                        if let Some(session) = lock_or_warn(
                                            &session_clone,
                                            "approval decision context read",
                                        ) {
                                            let run_id = session.thread.current_turn_id.clone();
                                            let chosen = match approval_type {
                                                ApprovalType::FileChange
                                                | ApprovalType::ApplyPatch => {
                                                    if item_id.is_empty() {
                                                        None
                                                    } else {
                                                        Some(item_id.clone())
                                                    }
                                                }
                                                _ => None,
                                            };
                                            (run_id, chosen)
                                        } else {
                                            (None, None)
                                        };
                                    if let Some(run_id) = run_id_for_decision {
                                        let decision = DecisionEvent {
                                            id: format!("decision_event_{}", request_id),
                                            run_id,
                                            chosen_patchset_id,
                                            approved,
                                            at: Utc::now(),
                                            rationale: description.clone(),
                                        };
                                        let history_writer = history_writer_clone.clone();
                                        let decision_id = decision.id.clone();
                                        ClientStorage::spawn_background_index_work(async move {
                                            history_writer
                                                .write("decision", &decision_id, &decision)
                                                .await;
                                        });
                                    }

                                    // Use the correct resolve method based on the request type
                                    let resolve_method = match mk {
                                        MethodKind::RequestApprovalCommandExecution => {
                                            "item/commandExecution/requestApproval/resolve"
                                        }
                                        MethodKind::RequestApprovalFileChange => {
                                            "item/fileChange/requestApproval/resolve"
                                        }
                                        MethodKind::RequestApprovalExec => {
                                            "exec_approval_request/resolve"
                                        }
                                        MethodKind::RequestApprovalApplyPatch => {
                                            "apply_patch_approval_request/resolve"
                                        }
                                        _ => "requestApproval/resolve",
                                    };

                                    use std::sync::atomic::{AtomicU64, Ordering};
                                    static APPROVAL_REQ_ID: AtomicU64 = AtomicU64::new(10_000);
                                    let resolve_id =
                                        APPROVAL_REQ_ID.fetch_add(1, Ordering::Relaxed);

                                    let approval_msg = CodexMessage::new_request(
                                        resolve_id,
                                        resolve_method,
                                        serde_json::json!({
                                            "requestId": request_id,
                                            "approved": approved
                                        }),
                                    );
                                    let _ = tx_clone.send(approval_msg.to_json()).await;
                                }
                            }
                        }
                    }
                }
                _ => break,
            }
        }
    });

    // ==========================================================================
    // 第五节：send_request 辅助函数（WebSocket RPC 请求-响应辅助器）
    // 封装向 Codex WebSocket 发送 JSON-RPC 请求并等待响应的完整流程：
    //   1. 生成全局唯一的请求 ID（原子计数器）
    //   2. 在 notifies map 中注册对应的 Notify，供 reader task 在收到响应时唤醒
    //   3. 序列化 CodexMessage 并通过 tx channel 发送给 writer task
    //   4. 等待 Notify 被触发（超时 30 秒）
    //   5. 从 responses map 中取出响应并返回 result 字段，超时则返回 Err
    // 此函数作为 execute 内的局部 async fn，可访问外部的共享状态。
    // ==========================================================================
    async fn send_request(
        tx: &mpsc::Sender<String>,
        responses: &Arc<Mutex<std::collections::HashMap<u64, serde_json::Value>>>,
        notifies: &Arc<Mutex<std::collections::HashMap<u64, Arc<tokio::sync::Notify>>>>,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        use std::sync::atomic::{AtomicU64, Ordering};
        // 全局单调递增的请求 ID，用于匹配请求与响应
        static REQUEST_ID: AtomicU64 = AtomicU64::new(1);
        let id = REQUEST_ID.fetch_add(1, Ordering::Relaxed);

        // 在 notifies map 中注册 Notify，reader task 收到对应 id 的响应后会调用 notify_waiters()
        let notify = Arc::new(tokio::sync::Notify::new());
        if let Some(mut notifs) = lock_or_warn(notifies, "send_request notify insert") {
            notifs.insert(id, notify.clone());
        } else {
            return Err("failed to register request notify".to_string());
        }

        // 序列化并发送请求消息到 writer task
        let msg = CodexMessage::new_request(id, method, params);
        tx.send(msg.to_json()).await.map_err(|e| e.to_string())?;

        // 使用 Notify 异步等待响应，避免忙等轮询，超时时间 30 秒
        let timeout = tokio::time::timeout(tokio::time::Duration::from_secs(30), async {
            notify.notified().await;
        });

        match timeout.await {
            Ok(_) => {
                // Response arrived, get it from the map
                let response =
                    if let Some(mut resp) = lock_or_warn(responses, "send_request response read") {
                        resp.remove(&id)
                    } else {
                        None
                    };
                if let Some(response) = response {
                    if let Some(mut notifs) = lock_or_warn(notifies, "send_request notify cleanup")
                    {
                        notifs.remove(&id);
                    }
                    if let Some(error_obj) = response.get("error") {
                        return Err(format!("Error: {}", error_obj));
                    }
                    return Ok(response.get("result").cloned().unwrap_or(response));
                }
                Err("Response not found".to_string())
            }
            Err(_) => {
                // Timeout - clean up
                if let Some(mut notifs) =
                    lock_or_warn(notifies, "send_request notify cleanup timeout")
                {
                    notifs.remove(&id);
                }
                Err("Timeout".to_string())
            }
        }
    }

    // ==========================================================================
    // 第六节：初始化握手 + 启动 Thread（Initialize + Start Thread）
    // 向 Codex app-server 发送两条初始化 RPC 请求，完成连接握手：
    //   1. "initialize"：协议握手，传递 clientInfo、工作目录、模型/provider/tier 配置
    //   2. "thread/start"：创建 Codex 执行线程，传递 approvalPolicy、沙盒模式、
    //      plan_mode 相关的 developer/base instructions 等。
    //      成功后从响应中提取 thread_id，后续所有 turn/start 请求都需携带此 ID。
    // ==========================================================================

    // 第一步：发送 initialize 请求，完成 MCP/WebSocket 协议握手
    match send_request(
        &tx,
        &responses,
        &notifies,
        "initialize",
        serde_json::json!({
            "capabilities": serde_json::Value::Null,
            "clientInfo": { "name": "libra", "version": env!("CARGO_PKG_VERSION") },
            "cliVersion": env!("CARGO_PKG_VERSION"),
            "cwd": args.cwd,
            "modelProvider": args.model_provider,
            "serviceTier": args.service_tier,
            "personality": args.personality
        }),
    )
    .await
    {
        Ok(_) => println!("Initialized"),
        Err(e) => {
            eprintln!("Init failed: {}", e);
            return Err(anyhow::anyhow!("initialization failed: {}", e));
        }
    }

    // 第二步：发送 thread/start 请求，创建 Codex 执行线程
    // approvalPolicy 映射规则：
    //   "accept" → "never"（从不请求审批）
    //   "ask"/"decline" → "on-request"（仅在需要时请求审批）
    match send_request(
        &tx,
        &responses,
        &notifies,
        "thread/start",
        serde_json::json!({
            "cwd": args.cwd,
            "approvalPolicy": match args.approval.as_str() {
                "ask" => serde_json::json!("on-request"),
                "accept" => serde_json::json!("never"),
                "decline" => serde_json::json!("on-request"),
                _ => serde_json::json!("on-request"),
            },
            "serviceTier": args.service_tier,
            "model": args.model,
            "modelProvider": args.model_provider,
            "personality": args.personality,
            "sandbox": SandboxMode::WorkspaceWrite,
            "developerInstructions": if args.plan_mode {
                serde_json::json!(plan_mode_developer_instructions())
            } else {
                serde_json::Value::Null
            },
            "baseInstructions": if args.plan_mode {
                serde_json::json!(plan_mode_base_instructions())
            } else {
                serde_json::Value::Null
            },
        }),
    )
    .await
    {
        Ok(resp) => {
            // Fallback chain: thread.id -> resp.threadId -> resp.thread_id
            let thread_id_from_response = resp
                .get("thread")
                .and_then(|t| t.get("id"))
                .and_then(|v| v.as_str())
                .or_else(|| resp.get("threadId").and_then(|v| v.as_str()))
                .or_else(|| resp.get("thread_id").and_then(|v| v.as_str()));

            if let Some(id) = thread_id_from_response {
                thread_id = id.to_string();
                println!("Thread: {}", id);
            }
        }
        Err(e) => {
            eprintln!("Thread start failed: {}", e);
            return Err(anyhow::anyhow!("thread start failed: {}", e));
        }
    }

    println!("\n=== Ready! Type your message ===\n");

    // ==========================================================================
    // 第七节：主循环（Main Loop）
    // 从 stdin 读取用户输入，并通过 `turn/start` RPC 发送给 Codex；
    // 同时处理来自 reader task 的审批请求（通过 approval_rx channel）。
    //
    // stdin 在独立 task 中以阻塞式逐行读取，通过 mpsc channel 传递给主循环，
    // 避免阻塞 tokio 运行时。
    //
    // waiting_for_approval 标志用于区分当前的 stdin 输入是：
    //   - 普通对话消息（发送为 turn/start）
    //   - 审批决策输入（转发给 approval_rx 的 oneshot handler）
    // ==========================================================================

    // 用于从 stdin 读取用户输入的 channel
    let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(100);
    // 标记当前是否正在等待用户的审批输入，防止审批输入被误当作对话消息发送
    let waiting_for_approval = Arc::new(Mutex::new(false));
    let waiting_for_approval_clone = waiting_for_approval.clone();

    // 在独立 task 中以阻塞方式读取 stdin，通过 channel 传递给主循环
    // （必须独立 task，否则阻塞式 stdin 读取会阻塞整个 tokio 运行时）
    tokio::spawn(async move {
        use std::io::{BufRead, BufReader};
        let stdin = BufReader::new(std::io::stdin());
        for line in stdin.lines().map_while(Result::ok) {
            let _ = stdin_tx.send(line).await;
        }
    });

    // 主循环：同时监听 stdin 输入和审批请求，以及 1 秒超时心跳
    loop {
        tokio::select! {
            msg = stdin_rx.recv() => {
                if let Some(line) = msg {
                    // 检查当前是否处于审批等待状态；
                    // 若是，则此输入应由审批 handler 消费，主循环跳过
                    let is_approval = lock_or_warn(
                        &waiting_for_approval_clone,
                        "waiting_for_approval read",
                    )
                    .map(|v| *v)
                    .unwrap_or(false);

                    if is_approval {
                        // 审批模式下的 stdin 输入由 approval_rx 处理，这里忽略
                        continue;
                    }

                    if line.trim().is_empty() {
                        continue;
                    }

                    // 将用户输入通过 turn/start RPC 发送给 Codex，开始新一轮 LLM 推理
                    match send_request(&tx, &responses, &notifies, "turn/start", serde_json::json!({
                        "input": [{ "type": "text", "text": line }],
                        "threadId": thread_id,
                        "cwd": args.cwd,
                        "model": args.model,
                        "modelProvider": args.model_provider,
                        "serviceTier": args.service_tier,
                        "personality": args.personality,
                        "approvalPolicy": match lock_or_warn(
                            &approval_mode_for_turn,
                            "approval mode read (turn/start)",
                        )
                        .as_ref()
                        .map(|v| v.as_str())
                        .unwrap_or("ask") {
                            "ask" => serde_json::json!("on-request"),
                            "accept" => serde_json::json!("never"),
                            "decline" => serde_json::json!("on-request"),
                            _ => serde_json::json!("on-request"),
                        }
                    })).await {
                        Ok(resp) => println!("Response: {:?}", resp),
                        Err(e) => eprintln!("Error: {}", e),
                    }
                }
            }
            // 接收来自 reader task 的交互式审批请求（仅在 approval_mode == "ask" 时触发）
            approval_req = approval_rx.recv() => {
                if let Some((params, response_tx)) = approval_req {
                    // 设置审批等待标志，使后续 stdin 输入被路由到审批分支而非 turn/start
                    if let Some(mut flag) = lock_or_warn(
                        &waiting_for_approval_clone,
                        "waiting_for_approval set true",
                    ) {
                        *flag = true;
                    }

                    // 向用户展示审批请求详情（类型、描述、标题、额外细节）
                    println!("\n⚠️  Approval Request:");
                    if let Some(approval_type) = params.get("type").and_then(|t| t.as_str()) {
                        println!("  Type: {}", approval_type);
                    }
                    if let Some(description) = params.get("description").and_then(|d| d.as_str()) {
                        println!("  Description: {}", description);
                    }
                    if let Some(title) = params.get("title").and_then(|t| t.as_str()) {
                        println!("  Title: {}", title);
                    }
                    // Show more details if available
                    if let Some(details) = params.get("details") {
                        println!("  Details: {}", details);
                    }

                    println!("\n  [a]ccept / [d]ecline / [A]ccept All / [D]ecline All: ");

                    // Read user input from the shared stdin channel instead of creating a new reader
                    let approved = if let Some(input) = stdin_rx.recv().await {
                        let choice = input.trim().to_lowercase();
                        match choice.as_str() {
                            "a" | "accept" => {
                                println!("  → Accepted");
                                true
                            }
                            "d" | "decline" => {
                                println!("  → Declined");
                                false
                            }
                            "aa" | "accept all" => {
                                println!("  → Accepted (will auto-accept future)");
                                if let Some(mut mode) = lock_or_warn(
                                    &approval_mode_clone,
                                    "approval mode set accept",
                                ) {
                                    *mode = "accept".to_string();
                                }
                                true
                            }
                            "dd" | "decline all" => {
                                println!("  → Declined (will auto-decline future)");
                                if let Some(mut mode) = lock_or_warn(
                                    &approval_mode_clone,
                                    "approval mode set decline",
                                ) {
                                    *mode = "decline".to_string();
                                }
                                false
                            }
                            _ => {
                                println!("  → Default accept");
                                true
                            }
                        }
                    } else {
                        println!("  → Default accept (no input)");
                        true
                    };

                    let _ = response_tx.send(approved);
                    println!();

                    // Clear flag to resume chat input
                    if let Some(mut flag) = lock_or_warn(
                        &waiting_for_approval_clone,
                        "waiting_for_approval set false",
                    ) {
                        *flag = false;
                    }
                }
            }
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(1)) => {}
        }
    }
    #[allow(unreachable_code)]
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{is_streaming_delta_method, truncate_for_log};
    use crate::internal::ai::codex::protocol::MethodKind;

    #[test]
    fn truncate_for_log_escapes_whitespace_and_caps_length() {
        let truncated = truncate_for_log("line1\nline2\rtab\there", 12);
        assert!(
            truncated.starts_with("line1\\nline2"),
            "expected escaped newline prefix, got: {truncated}"
        );
        assert!(
            truncated.ends_with("…(truncated)"),
            "expected truncation marker, got: {truncated}"
        );
    }

    #[test]
    fn truncate_for_log_returns_full_string_when_under_limit() {
        let truncated = truncate_for_log("hi", 200);
        assert_eq!(truncated, "hi");
    }

    #[test]
    fn truncate_for_log_handles_multi_byte_unicode() {
        // 4 graphemes / 4 char codepoints / >= 8 bytes — we want the truncation
        // boundary to fall on a Unicode char boundary, not in the middle of a
        // UTF-8 byte sequence.
        let truncated = truncate_for_log("résumé你好", 3);
        assert!(
            truncated.starts_with("rés"),
            "expected first 3 chars preserved, got: {truncated}"
        );
        assert!(truncated.ends_with("…(truncated)"));
    }

    #[test]
    fn is_streaming_delta_method_covers_all_four_token_streams() {
        for method in [
            MethodKind::AgentMessageDelta,
            MethodKind::CommandExecutionOutputDelta,
            MethodKind::FileChangeOutputDelta,
            MethodKind::PlanDelta,
        ] {
            assert!(
                is_streaming_delta_method(method),
                "expected {method:?} to be classified as a streaming delta"
            );
        }
    }

    #[test]
    fn is_streaming_delta_method_excludes_lifecycle_events() {
        for method in [
            MethodKind::ThreadStarted,
            MethodKind::TurnStarted,
            MethodKind::TurnCompleted,
            MethodKind::ItemStarted,
            MethodKind::ItemCompleted,
            MethodKind::PlanUpdated,
            MethodKind::RequestApprovalCommandExecution,
        ] {
            assert!(
                !is_streaming_delta_method(method),
                "expected {method:?} to remain a publish-triggering event"
            );
        }
    }

    /// Mirrors the reader-task publish book-keeping that lives in
    /// `start_code_ui_runtime`.
    ///
    /// Each non-delta method publishes once at the post-match coalescing branch
    /// (and clears `delta_skipped_since_publish`). Streaming-delta methods set
    /// the flag without publishing. After the loop exits, if the flag is still
    /// set, the final flush adds one publish.
    ///
    /// Approval-request methods have an **extra** pre-publish inside the
    /// approval handler — but **only** when the operator-facing approval mode
    /// is "ask" (i.e. neither auto-accept nor auto-decline). When the mode is
    /// "accept" / "decline" (e.g. `--approval-policy allow-all`) the pre-publish
    /// is skipped. The `ask_mode_for_approvals` parameter models this branch
    /// so the helper stays a faithful mirror of the production reader.
    fn simulate_reader_publish_count(methods: &[MethodKind], ask_mode_for_approvals: bool) -> u32 {
        let mut delta_skipped_since_publish = false;
        let mut publish_count: u32 = 0;
        for &m in methods {
            // Pre-publish: in ask mode the approval handler publishes once
            // before awaiting the operator decision so the approval prompt
            // appears in the broadcast snapshot. The post-match branch below
            // unconditionally overwrites `delta_skipped_since_publish` for
            // this same iteration (every approval method is non-streaming),
            // so we don't need to clear the flag here — that would be a
            // dead store.
            if ask_mode_for_approvals
                && matches!(
                    m,
                    MethodKind::RequestApproval
                        | MethodKind::RequestApprovalCommandExecution
                        | MethodKind::RequestApprovalFileChange
                        | MethodKind::RequestApprovalApplyPatch
                        | MethodKind::RequestApprovalExec
                )
            {
                publish_count += 1;
            }

            if is_streaming_delta_method(m) {
                delta_skipped_since_publish = true;
            } else {
                publish_count += 1;
                delta_skipped_since_publish = false;
            }
        }
        if delta_skipped_since_publish {
            publish_count += 1;
        }
        publish_count
    }

    #[test]
    fn final_flush_runs_after_socket_close_with_dangling_deltas() {
        // Two streaming deltas with no lifecycle event between them and
        // socket close: the loop body would skip both publishes, so the
        // final-flush branch must publish exactly once.
        let methods = [MethodKind::AgentMessageDelta, MethodKind::AgentMessageDelta];
        assert_eq!(simulate_reader_publish_count(&methods, false), 1);
    }

    #[test]
    fn final_flush_does_not_duplicate_when_completion_already_flushed() {
        // Delta then ItemCompleted: ItemCompleted publishes once and clears
        // the flag, so the post-loop flush must NOT add a second publish.
        let methods = [MethodKind::AgentMessageDelta, MethodKind::ItemCompleted];
        assert_eq!(simulate_reader_publish_count(&methods, false), 1);
    }

    #[test]
    fn no_publish_at_all_when_stream_is_empty() {
        assert_eq!(simulate_reader_publish_count(&[], false), 0);
        assert_eq!(simulate_reader_publish_count(&[], true), 0);
    }

    #[test]
    fn lifecycle_events_publish_once_per_event_with_no_extra_flush() {
        let methods = [
            MethodKind::ThreadStarted,
            MethodKind::TurnStarted,
            MethodKind::ItemStarted,
            MethodKind::ItemCompleted,
            MethodKind::TurnCompleted,
        ];
        assert_eq!(simulate_reader_publish_count(&methods, false), 5);
    }

    #[test]
    fn long_streaming_burst_followed_by_completion_publishes_twice() {
        let mut methods = vec![MethodKind::AgentMessageDelta; 100];
        methods.push(MethodKind::ItemCompleted);
        methods.extend(std::iter::repeat_n(MethodKind::AgentMessageDelta, 100));
        // First completion publishes (and clears the flag); the trailing
        // 100-delta burst sets the flag and the final flush publishes once.
        assert_eq!(simulate_reader_publish_count(&methods, false), 2);
    }

    #[test]
    fn ask_mode_approval_event_publishes_twice_when_sandwiched_in_deltas() {
        // [delta, RequestApprovalCommandExecution, delta, close] under ask
        // mode: pre-publish for the prompt + post-publish for the event +
        // final flush for the dangling delta = 3 publishes total. This
        // regression-tests the gap Codex flagged in round 4.
        let methods = [
            MethodKind::AgentMessageDelta,
            MethodKind::RequestApprovalCommandExecution,
            MethodKind::AgentMessageDelta,
        ];
        assert_eq!(simulate_reader_publish_count(&methods, true), 3);
    }

    #[test]
    fn auto_approve_mode_skips_the_approval_pre_publish() {
        // Same sequence as the ask-mode test, but the operator selected
        // --approval-policy=allow-all so codex maps to "accept" and the
        // pre-publish is skipped. Result: post-publish for the event +
        // final flush = 2 publishes total.
        let methods = [
            MethodKind::AgentMessageDelta,
            MethodKind::RequestApprovalCommandExecution,
            MethodKind::AgentMessageDelta,
        ];
        assert_eq!(simulate_reader_publish_count(&methods, false), 2);
    }
}
