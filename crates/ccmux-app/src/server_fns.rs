use chrono::{DateTime, Utc};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use ccmux_core::display::DisplayItem;
use ccmux_core::display::streaming::StreamEvent;

/// Wire type for session metadata, serializable across the network boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub project: String,
    pub slug: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub message_count: usize,
    pub first_message: Option<String>,
    pub project_path: Option<String>,
    pub is_sidechain: bool,
    pub parent_session_id: Option<String>,
    pub agent_id: Option<String>,
}

/// Response type for get_session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionResponse {
    pub meta: SessionMeta,
    pub items: Vec<DisplayItem>,
}

#[cfg(not(target_arch = "wasm32"))]
fn base_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    std::path::PathBuf::from(home)
        .join(".claude")
        .join("projects")
}

#[cfg(not(target_arch = "wasm32"))]
impl SessionMeta {
    fn from_info(info: &ccmux_core::session::loader::SessionInfo) -> Self {
        Self {
            id: info.id.clone(),
            project: info.project.clone(),
            slug: info.slug.clone(),
            created_at: info.created_at,
            updated_at: info.updated_at,
            message_count: info.message_count,
            first_message: info.first_message.clone(),
            project_path: info.project_path.clone(),
            is_sidechain: info.is_sidechain,
            parent_session_id: info.parent_session_id.clone(),
            agent_id: info.agent_id.clone(),
        }
    }
}

#[server]
pub async fn list_sessions(project: Option<String>) -> Result<Vec<SessionMeta>, ServerFnError> {
    let base = base_path();
    let sessions = ccmux_core::session::loader::discover_sessions(&base)
        .map_err(|e| ServerFnError::new(format!("Failed to discover sessions: {e}")))?;

    let metas: Vec<SessionMeta> = sessions
        .iter()
        .filter(|s| !s.is_sidechain)
        .filter(|s| s.first_message.is_some())
        .filter(|s| project.as_ref().is_none_or(|p| &s.project == p))
        .map(SessionMeta::from_info)
        .collect();

    Ok(metas)
}

#[server]
pub async fn get_session(session_id: String) -> Result<SessionResponse, ServerFnError> {
    let base = base_path();
    let sessions = ccmux_core::session::loader::discover_sessions(&base)
        .map_err(|e| ServerFnError::new(format!("Failed to discover sessions: {e}")))?;

    let info = sessions
        .iter()
        .find(|s| s.id == session_id)
        .ok_or_else(|| ServerFnError::new(format!("Session not found: {session_id}")))?;

    let raw_events = ccmux_core::session::loader::load_session_raw(&info.path)
        .map_err(|e| ServerFnError::new(format!("Failed to load session: {e}")))?;

    let events = ccmux_core::events::parse::parse_events(&raw_events);
    let opts = ccmux_core::display::DisplayOpts::default();
    let items = ccmux_core::display::pipeline::events_to_display_items(&events, &raw_events, &opts);

    let meta = SessionMeta::from_info(info);

    Ok(SessionResponse { meta, items })
}

/// Stream new display events from a session file via SSE.
///
/// Watches the session JSONL file for appended lines and emits `StreamEvent`s
/// as new events are written by Claude Code.
#[server]
pub async fn stream_session_events(
    session_id: String,
) -> Result<dioxus_fullstack::ServerEvents<StreamEvent>, ServerFnError> {
    use std::io::{BufRead, Seek, SeekFrom};

    use dioxus_fullstack::ServerEvents;
    use dioxus_fullstack::SseTx;
    use notify::{EventKind, RecursiveMode, Watcher};

    use ccmux_core::display::DisplayOpts;
    use ccmux_core::display::pipeline::{
        extract_tool_results_from_event, single_event_to_display_items,
    };
    use ccmux_core::display::streaming::StreamPipelineState;
    use ccmux_core::events::parse::parse_event;

    let base = base_path();
    let sessions = ccmux_core::session::loader::discover_sessions(&base)
        .map_err(|e| ServerFnError::new(format!("Failed to discover sessions: {e}")))?;

    let info = sessions
        .iter()
        .find(|s| s.id == session_id)
        .ok_or_else(|| ServerFnError::new(format!("Session not found: {session_id}")))?;

    let file_path = info.path.clone();

    // Get initial file size so we only process new lines
    let initial_size = std::fs::metadata(&file_path).map(|m| m.len()).unwrap_or(0);

    Ok(ServerEvents::new(
        move |mut tx: SseTx<StreamEvent>| async move {
            let (notify_tx, mut notify_rx) = tokio::sync::mpsc::channel::<()>(16);

            // Set up file watcher
            let watch_path = file_path.clone();
            let mut watcher = match notify::recommended_watcher(
                move |res: Result<notify::Event, notify::Error>| {
                    if let Ok(event) = res
                        && matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_))
                    {
                        let _ = notify_tx.try_send(());
                    }
                },
            ) {
                Ok(w) => w,
                Err(e) => {
                    tracing::error!(%e, "Failed to create file watcher");
                    return;
                }
            };

            if let Err(e) = watcher.watch(&watch_path, RecursiveMode::NonRecursive) {
                tracing::error!(%e, "Failed to watch session file");
                return;
            }

            let mut cursor = initial_size;
            let opts = DisplayOpts::default();
            let mut state = StreamPipelineState::new(opts);

            // Pre-populate tool_results from existing content so streaming events
            // can reference results that were already in the file.
            if let Ok(existing_raw) = ccmux_core::session::loader::load_session_raw(&file_path) {
                for raw in &existing_raw {
                    for (id, result) in extract_tool_results_from_event(raw) {
                        state.tool_results.insert(id, result);
                    }
                }
            }

            loop {
                // Wait for a file change notification
                if notify_rx.recv().await.is_none() {
                    break;
                }

                // Drain any queued notifications
                while notify_rx.try_recv().is_ok() {}

                // Read new content from the file
                let file = match std::fs::File::open(&file_path) {
                    Ok(f) => f,
                    Err(_) => continue,
                };

                let mut reader = std::io::BufReader::new(file);
                if reader.seek(SeekFrom::Start(cursor)).is_err() {
                    continue;
                }

                let mut new_lines = Vec::new();
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line) {
                        Ok(0) => break,
                        Ok(n) => {
                            cursor += n as u64;
                            let trimmed = line.trim();
                            if !trimmed.is_empty()
                                && let Ok(raw) = serde_json::from_str::<serde_json::Value>(trimmed)
                            {
                                new_lines.push(raw);
                            }
                        }
                        Err(_) => break,
                    }
                }

                // Process each new line through the display pipeline
                for raw in new_lines {
                    // Extract tool results from user events
                    for (id, result) in extract_tool_results_from_event(&raw) {
                        let event = state.index_tool_result(id, result);
                        if tx.send(event).await.is_err() {
                            return;
                        }
                    }

                    let parsed = parse_event(&raw);
                    let intermediates = single_event_to_display_items(
                        &parsed,
                        &raw,
                        &state.opts,
                        &state.tool_results,
                    );

                    for (item, mode) in intermediates {
                        if let Some(event) = state.emit(item, mode)
                            && tx.send(event).await.is_err()
                        {
                            return;
                        }
                    }
                }
            }
        },
    ))
}
