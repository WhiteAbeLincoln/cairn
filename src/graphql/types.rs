use std::collections::HashMap;
use std::sync::Arc;

use async_graphql::{InputObject, Json, Object, SimpleObject};
use chrono::{DateTime, Utc};
use serde_json::Value;

// --- Pagination ---

#[derive(InputObject)]
pub struct PageInput {
    #[graphql(default)]
    pub offset: i32,
    #[graphql(default)]
    pub limit: i32,
}

// --- Session events ---

/// Pre-indexed session data shared across all events in a query response.
pub struct SessionData {
    pub events: Vec<Value>,
    pub uuid_to_idx: HashMap<String, usize>,
    pub parent_to_children: HashMap<String, Vec<usize>>,
}

impl SessionData {
    pub fn new(events: Vec<Value>) -> Self {
        let mut uuid_to_idx = HashMap::new();
        let mut parent_to_children: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, ev) in events.iter().enumerate() {
            if let Some(uuid) = ev.get("uuid").and_then(|v| v.as_str()) {
                uuid_to_idx.insert(uuid.to_string(), i);
            }
            if let Some(parent) = ev.get("parentUuid").and_then(|v| v.as_str()) {
                parent_to_children
                    .entry(parent.to_string())
                    .or_default()
                    .push(i);
            }
        }
        SessionData {
            events,
            uuid_to_idx,
            parent_to_children,
        }
    }
}

/// A single session event with known fields, raw JSON, and relational resolvers.
pub struct SessionEvent {
    pub data: Arc<SessionData>,
    pub index: usize,
}

impl SessionEvent {
    fn value(&self) -> &Value {
        &self.data.events[self.index]
    }

    fn str_field(&self, key: &str) -> Option<String> {
        self.value()
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }
}

#[Object]
impl SessionEvent {
    #[graphql(name = "type")]
    async fn event_type(&self) -> String {
        self.str_field("type").unwrap_or_default()
    }

    async fn uuid(&self) -> Option<String> {
        self.str_field("uuid")
    }

    async fn timestamp(&self) -> Option<String> {
        self.str_field("timestamp")
    }

    async fn session_id(&self) -> Option<String> {
        self.str_field("sessionId")
    }

    async fn parent_uuid(&self) -> Option<String> {
        self.str_field("parentUuid")
    }

    /// The full raw JSON event.
    async fn raw(&self) -> Json<Value> {
        Json(self.value().clone())
    }

    /// The event whose uuid matches this event's parentUuid.
    async fn parent(&self) -> Option<SessionEvent> {
        let parent_uuid = self.value().get("parentUuid").and_then(|v| v.as_str())?;
        let &idx = self.data.uuid_to_idx.get(parent_uuid)?;
        Some(SessionEvent {
            data: Arc::clone(&self.data),
            index: idx,
        })
    }

    /// All events whose parentUuid matches this event's uuid.
    async fn children(&self) -> Vec<SessionEvent> {
        let uuid = match self.value().get("uuid").and_then(|v| v.as_str()) {
            Some(u) => u,
            None => return vec![],
        };
        self.data
            .parent_to_children
            .get(uuid)
            .map(|indices| {
                indices
                    .iter()
                    .map(|&idx| SessionEvent {
                        data: Arc::clone(&self.data),
                        index: idx,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Paginated session events result.
pub struct SessionEventsData {
    pub events: Vec<SessionEvent>,
    pub total: i32,
}

#[Object]
impl SessionEventsData {
    async fn events(&self) -> &[SessionEvent] {
        &self.events
    }

    async fn total(&self) -> i32 {
        self.total
    }
}

// --- Session ---

#[derive(SimpleObject, Clone)]
pub struct SessionMeta {
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

/// A session with metadata and lazy-loaded events.
pub struct Session {
    pub meta: SessionMeta,
    pub path: std::path::PathBuf,
}

#[Object]
impl Session {
    async fn meta(&self) -> &SessionMeta {
        &self.meta
    }

    /// The raw JSONL content of the session file.
    async fn raw_log(&self) -> async_graphql::Result<String> {
        std::fs::read_to_string(&self.path)
            .map_err(|e| async_graphql::Error::new(e.to_string()))
    }

    /// Mapping from tool_use_id to agent_id for subagent calls.
    async fn agent_map(&self) -> async_graphql::Result<Vec<AgentMapping>> {
        let mappings = crate::session::loader::extract_agent_map(&self.path)
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        Ok(mappings
            .into_iter()
            .map(|(tool_use_id, agent_id)| AgentMapping {
                tool_use_id,
                agent_id,
            })
            .collect())
    }

    /// Load session events, optionally paginated.
    async fn events(&self, page: Option<PageInput>) -> async_graphql::Result<SessionEventsData> {
        let all_events = crate::session::loader::load_session_raw(&self.path)
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;

        let total = all_events.len() as i32;
        let data = Arc::new(SessionData::new(all_events));

        let events = match page {
            Some(p) => {
                let start = (p.offset as usize).min(data.events.len());
                let end = if p.limit > 0 {
                    (start + p.limit as usize).min(data.events.len())
                } else {
                    data.events.len()
                };
                (start..end)
                    .map(|i| SessionEvent {
                        data: Arc::clone(&data),
                        index: i,
                    })
                    .collect()
            }
            None => (0..data.events.len())
                .map(|i| SessionEvent {
                    data: Arc::clone(&data),
                    index: i,
                })
                .collect(),
        };

        Ok(SessionEventsData { events, total })
    }
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
            meta: SessionMeta {
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
            },
            path: s.path.clone(),
        }
    }
}
