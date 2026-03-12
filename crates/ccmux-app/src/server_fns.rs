use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use ccmux_core::display::DisplayItem;

/// Wire type for session metadata, serializable across the network boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub project: String,
    pub slug: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
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

fn base_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    std::path::PathBuf::from(home)
        .join(".claude")
        .join("projects")
}

impl SessionMeta {
    fn from_info(info: &ccmux_core::session::loader::SessionInfo) -> Self {
        Self {
            id: info.id.clone(),
            project: info.project.clone(),
            slug: info.slug.clone(),
            created_at: info.created_at.map(|dt| dt.to_rfc3339()),
            updated_at: info.updated_at.map(|dt| dt.to_rfc3339()),
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
