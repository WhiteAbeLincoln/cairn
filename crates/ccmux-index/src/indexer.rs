use std::collections::HashMap;
use std::io::{BufRead, Seek, SeekFrom};
use std::path::Path;
use std::time::Instant;

use chrono::Utc;
use rusqlite::Connection;
use serde_json::Value;

use ccmux_core::display::pipeline::events_to_display_items;
use ccmux_core::display::{DisplayItem, DisplayModeF, DisplayOpts};
use ccmux_core::events::parse::parse_events;
use ccmux_core::session::SessionInfo;
use ccmux_core::session::loader::discover_sessions;

use crate::IndexStats;

/// Load raw JSONL events starting from a byte offset, returning (offset, value) pairs.
/// After the loop, `final_offset` is the byte position after the last line read.
fn load_from_offset(path: &Path, start_offset: u64) -> std::io::Result<(Vec<(u64, Value)>, u64)> {
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    reader.seek(SeekFrom::Start(start_offset))?;

    let mut events = Vec::new();
    let mut current_offset = start_offset;
    let mut line = String::new();
    let mut line_num = 0u64;

    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line)?;
        if bytes_read == 0 {
            break;
        }
        line_num += 1;
        let line_offset = current_offset;
        current_offset += bytes_read as u64;

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match serde_json::from_str::<Value>(trimmed) {
            Ok(value) => events.push((line_offset, value)),
            Err(e) => {
                tracing::warn!(line = line_num, offset = line_offset, error = %e, "Failed to parse JSONL line");
            }
        }
    }

    Ok((events, current_offset))
}

/// Build a map from event UUID to timestamp string.
fn build_timestamp_map(raw_events: &[Value]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for raw in raw_events {
        if let (Some(uuid), Some(ts)) = (
            raw.get("uuid").and_then(|v| v.as_str()),
            raw.get("timestamp").and_then(|v| v.as_str()),
        ) {
            map.insert(uuid.to_string(), ts.to_string());
        }
    }
    map
}

/// Extract file paths from file-history-snapshot events.
fn extract_file_paths(raw_events: &[Value]) -> Vec<(String, Option<String>)> {
    let mut paths = Vec::new();
    for raw in raw_events {
        let event_type = raw.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if event_type != "file-history-snapshot" {
            continue;
        }
        let message_id = raw
            .get("messageId")
            .and_then(|v| v.as_str())
            .map(String::from);
        if let Some(backups) = raw
            .pointer("/snapshot/trackedFileBackups")
            .and_then(|v| v.as_object())
        {
            for file_path in backups.keys() {
                paths.push((file_path.clone(), message_id.clone()));
            }
        }
    }
    paths
}

/// Counts of items added during a single session indexing operation.
pub(crate) struct SessionIndexResult {
    messages_added: usize,
    files_added: usize,
}

/// Index a single session, reading only new events since the last indexed offset.
/// Returns counts of messages and files added.
pub(crate) fn index_session(
    conn: &Connection,
    info: &SessionInfo,
) -> Result<SessionIndexResult, Box<dyn std::error::Error>> {
    // Get last indexed offset for this session
    let last_offset: u64 = conn
        .query_row(
            "SELECT last_offset FROM session_index WHERE session_id = ?1",
            [&info.id],
            |row| row.get(0),
        )
        .unwrap_or(0);

    // Load new events from the offset
    let (offset_events, final_offset) = load_from_offset(&info.path, last_offset)?;

    if offset_events.is_empty() {
        return Ok(SessionIndexResult {
            messages_added: 0,
            files_added: 0,
        });
    }

    let raw_events: Vec<Value> = offset_events.iter().map(|(_, v)| v.clone()).collect();

    // Build timestamp map
    let timestamp_map = build_timestamp_map(&raw_events);

    // Parse events and run through display pipeline
    let parsed = parse_events(&raw_events);
    let opts = DisplayOpts::indexing();
    let display_items = events_to_display_items(&parsed, &raw_events, &opts);

    // Extract file paths
    let file_paths = extract_file_paths(&raw_events);

    // Insert everything in a transaction
    let tx = conn.unchecked_transaction()?;

    // Upsert session_index
    tx.execute(
        "INSERT INTO session_index (session_id, project, project_path, slug, first_message, created_at, updated_at, file_path, last_offset, indexed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(session_id) DO UPDATE SET
             last_offset = excluded.last_offset,
             updated_at = excluded.updated_at,
             indexed_at = excluded.indexed_at",
        rusqlite::params![
            info.id,
            info.project,
            info.project_path,
            info.slug,
            info.first_message,
            info.created_at.map(|dt| dt.to_rfc3339()),
            info.updated_at.map(|dt| dt.to_rfc3339()),
            info.path.to_string_lossy().to_string(),
            final_offset as i64,
            Utc::now().to_rfc3339(),
        ],
    )?;

    // Insert messages for Full UserMessage and AssistantMessage items
    let mut insert_msg = tx.prepare(
        "INSERT OR IGNORE INTO messages (session_id, event_uuid, role, content, timestamp, chunk_index)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;

    let mut messages_added = 0usize;
    for item_with_mode in &display_items {
        match item_with_mode {
            DisplayModeF::Full(DisplayItem::UserMessage { content, meta, .. }) => {
                if let Some(uuid) = &meta.uuid {
                    let ts = timestamp_map.get(uuid).cloned().unwrap_or_default();
                    messages_added += insert_msg
                        .execute(rusqlite::params![info.id, uuid, "user", content, ts, 0])?;
                }
            }
            DisplayModeF::Full(DisplayItem::AssistantMessage { text, meta, .. }) => {
                if let Some(uuid) = &meta.uuid {
                    let ts = timestamp_map.get(uuid).cloned().unwrap_or_default();
                    messages_added += insert_msg.execute(rusqlite::params![
                        info.id,
                        uuid,
                        "assistant",
                        text,
                        ts,
                        0
                    ])?;
                }
            }
            _ => {}
        }
    }

    drop(insert_msg);

    // Insert file paths
    let mut insert_file = tx.prepare(
        "INSERT OR IGNORE INTO session_files (session_id, file_path, message_id)
         VALUES (?1, ?2, ?3)",
    )?;

    let mut files_added = 0usize;
    for (file_path, message_id) in &file_paths {
        files_added += insert_file.execute(rusqlite::params![info.id, file_path, message_id])?;
    }

    drop(insert_file);
    tx.commit()?;

    Ok(SessionIndexResult {
        messages_added,
        files_added,
    })
}

/// Index all discovered sessions under the given base path.
pub(crate) fn index_all(
    conn: &Connection,
    base_path: &Path,
) -> Result<IndexStats, Box<dyn std::error::Error>> {
    let start = Instant::now();
    let sessions = discover_sessions(base_path)?;

    let mut sessions_indexed = 0usize;
    let mut messages_indexed = 0usize;
    let mut files_indexed = 0usize;

    for info in &sessions {
        // Skip sidechains
        if info.is_sidechain {
            continue;
        }

        // Skip if file doesn't exist
        if !info.path.exists() {
            continue;
        }

        match index_session(conn, info) {
            Ok(result) => {
                if result.messages_added > 0 || result.files_added > 0 {
                    sessions_indexed += 1;
                }
                messages_indexed += result.messages_added;
                files_indexed += result.files_added;
            }
            Err(e) => {
                tracing::warn!(session_id = %info.id, error = %e, "Failed to index session");
            }
        }
    }

    Ok(IndexStats {
        sessions_indexed,
        messages_indexed,
        files_indexed,
        duration: start.elapsed(),
    })
}
