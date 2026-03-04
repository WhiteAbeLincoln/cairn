use async_graphql::{InputObject, Json, SimpleObject};
use chrono::{DateTime, Utc};
use serde_json::Value;

// --- Pagination ---

#[derive(InputObject)]
pub struct PageInput {
    pub offset: i32,
    pub limit: i32,
}

#[derive(SimpleObject)]
pub struct SessionEvents {
    pub events: Vec<Json<Value>>,
    pub total: i32,
}

// --- Session ---

#[derive(SimpleObject)]
pub struct Session {
    pub id: String,
    pub project: String,
    pub slug: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub message_count: i32,
    pub first_message: Option<String>,
    pub project_path: Option<String>,
    /// Absolute path to the session's .jsonl file on disk.
    pub file_path: Option<String>,
    pub is_sidechain: bool,
    pub parent_session_id: Option<String>,
    pub agent_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct AgentMapping {
    pub tool_use_id: String,
    pub agent_id: String,
}

// --- Conversion from session types ---

impl From<&crate::session::loader::SessionInfo> for Session {
    fn from(s: &crate::session::loader::SessionInfo) -> Self {
        Session {
            id: s.id.clone(),
            project: s.project.clone(),
            slug: s.slug.clone(),
            created_at: s.created_at,
            updated_at: s.updated_at,
            message_count: s.message_count as i32,
            first_message: s.first_message.clone(),
            project_path: s.project_path.clone(),
            file_path: Some(s.path.to_string_lossy().into_owned()),
            is_sidechain: s.is_sidechain,
            parent_session_id: s.parent_session_id.clone(),
            agent_id: s.agent_id.clone(),
        }
    }
}
