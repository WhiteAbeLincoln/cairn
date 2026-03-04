use std::collections::HashMap;
use std::sync::Arc;

use async_graphql::{InputObject, Interface, Json, Object, SimpleObject};
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
    pub file_path: String,
}

impl SessionData {
    pub fn new(events: Vec<Value>, file_path: String) -> Self {
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
            file_path,
        }
    }

    /// Construct a typed Event from the event at the given index.
    pub fn make_event(self: &Arc<Self>, i: usize) -> Event {
        let ev = &self.events[i];
        let event_type = ev.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let event_uuid = ev.get("uuid").and_then(|v| v.as_str()).unwrap_or("");
        let line = i + 1;
        let _span = tracing::warn_span!(
            "event",
            event_type,
            line,
            uuid = event_uuid,
            file = %self.file_path,
        )
        .entered();

        let d = Arc::clone(self);
        match event_type {
            "file-history-snapshot" => {
                if !validate_event(
                    ev,
                    &[
                        ("messageId", JsonType::String),
                        ("snapshot", JsonType::Object),
                        ("isSnapshotUpdate", JsonType::Bool),
                    ],
                    &["messageId", "snapshot", "isSnapshotUpdate"],
                ) {
                    return Event::UnknownEvent(UnknownEvent { data: d, index: i });
                }
                Event::FileHistoryEvent(FileHistoryEvent { data: d, index: i })
            }
            "queue_operation" => {
                if !validate_event(
                    ev,
                    &[
                        ("timestamp", JsonType::String),
                        ("sessionId", JsonType::String),
                        ("operation", JsonType::String),
                    ],
                    &["timestamp", "sessionId", "operation", "content"],
                ) {
                    return Event::UnknownEvent(UnknownEvent { data: d, index: i });
                }
                Event::QueueOperationEvent(QueueOperationEvent { data: d, index: i })
            }
            "progress" => {
                if !validate_event(
                    ev,
                    CORE_EVENT_REQUIRED,
                    &[CORE_EVENT_FIELDS, &["agentId", "data", "parentToolUseID", "toolUseID"]].concat(),
                ) {
                    return Event::UnknownEvent(UnknownEvent { data: d, index: i });
                }
                Event::ProgressEvent(ProgressEvent { data: d, index: i })
            }
            "assistant" => {
                if !validate_event(
                    ev,
                    &[CORE_EVENT_REQUIRED, &[("message", JsonType::Object)]].concat(),
                    &[CORE_EVENT_FIELDS, &["message", "requestId", "agentId"]].concat(),
                ) {
                    return Event::UnknownEvent(UnknownEvent { data: d, index: i });
                }
                Event::AssistantEvent(AssistantEvent { data: d, index: i })
            }
            "user" => {
                if !validate_event(
                    ev,
                    &[CORE_EVENT_REQUIRED, &[("message", JsonType::Object)]].concat(),
                    &[
                        CORE_EVENT_FIELDS,
                        &[
                            "message",
                            "sourceToolAssistantUUID",
                            "toolUseResult",
                            "agentId",
                            "permissionMode",
                            "todos",
                            "thinkingMetadata",
                            "isVisibleInTranscriptOnly",
                            "isCompactSummary",
                            "imagePasteIds",
                            "isMeta",
                            "planContent",
                        ],
                    ]
                    .concat(),
                ) {
                    return Event::UnknownEvent(UnknownEvent { data: d, index: i });
                }
                Event::UserEvent(UserEvent { data: d, index: i })
            }
            "system" => {
                if !validate_event(
                    ev,
                    CORE_EVENT_REQUIRED,
                    &[
                        CORE_EVENT_FIELDS,
                        &[
                            "durationMs",
                            "level",
                            "content",
                            "compactMetadata",
                            "logicalParentUuid",
                            "url",
                            "retryInMs",
                            "retryAttempt",
                            "maxRetries",
                            "cause",
                            "isMeta",
                            "subtype",
                        ],
                    ]
                    .concat(),
                ) {
                    return Event::UnknownEvent(UnknownEvent { data: d, index: i });
                }
                Event::SystemEvent(SystemEvent { data: d, index: i })
            }
            _ => Event::UnknownEvent(UnknownEvent { data: d, index: i }),
        }
    }
}

// --- Event field validation ---

#[derive(Clone)]
enum JsonType {
    String,
    Bool,
    Number,
    Object,
    Array,
}

impl JsonType {
    fn matches(&self, v: &Value) -> bool {
        match self {
            JsonType::String => v.is_string(),
            JsonType::Bool => v.is_boolean(),
            JsonType::Number => v.is_number(),
            JsonType::Object => v.is_object(),
            JsonType::Array => v.is_array(),
        }
    }

    fn label(&self) -> &'static str {
        match self {
            JsonType::String => "string",
            JsonType::Bool => "bool",
            JsonType::Number => "number",
            JsonType::Object => "object",
            JsonType::Array => "array",
        }
    }
}


/// Event interface fields present on every event type.
const EVENT_INTERFACE_FIELDS: &[&str] = &["type", "error", "apiError", "isApiErrorMessage"];

/// Validate an event's required fields and warn about unexpected fields.
/// `required` lists (field_name, expected_type) pairs that must be present and correctly typed.
/// `known` lists type-specific field names (Event interface fields are included automatically).
/// Returns `true` if all required fields are valid.
/// Validate an event's fields within an already-entered tracing span.
/// Returns `true` if all required fields are valid.
fn validate_event(ev: &Value, required: &[(&str, JsonType)], known: &[&str]) -> bool {
    let invalid: Vec<String> = required
        .iter()
        .filter_map(|(key, expected)| match ev.get(*key) {
            None | Some(Value::Null) => Some(format!("{key}: missing")),
            Some(v) if !expected.matches(v) => Some(format!(
                "{key}: expected {}, got {}",
                expected.label(),
                json_type_name(v),
            )),
            _ => None,
        })
        .collect();

    if !invalid.is_empty() {
        tracing::warn!(?invalid, "invalid fields, falling back to UnknownEvent");
        return false;
    }

    if let Some(obj) = ev.as_object() {
        let excess: Vec<&String> = obj
            .keys()
            .filter(|k| {
                !known.contains(&k.as_str()) && !EVENT_INTERFACE_FIELDS.contains(&k.as_str())
            })
            .collect();
        if !excess.is_empty() {
            tracing::warn!(?excess, "unexpected fields on event");
        }
    }

    true
}

fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

// --- Event interface ---

#[derive(Interface)]
#[graphql(
    field(name = "type", method = "event_type", ty = "String"),
    field(name = "raw", ty = "Json<Value>"),
    field(name = "error", ty = "Option<Json<Value>>"),
    field(name = "apiError", method = "api_error", ty = "Option<Json<Value>>"),
    field(name = "isApiErrorMessage", method = "is_api_error_message", ty = "Option<bool>")
)]
pub enum Event {
    UnknownEvent(UnknownEvent),
    FileHistoryEvent(FileHistoryEvent),
    QueueOperationEvent(QueueOperationEvent),
    ProgressEvent(ProgressEvent),
    AssistantEvent(AssistantEvent),
    UserEvent(UserEvent),
    SystemEvent(SystemEvent),
}

// --- CoreEvent interface ---

#[derive(Interface)]
#[graphql(
    field(name = "cwd", ty = "String"),
    field(name = "gitBranch", method = "git_branch", ty = "Option<String>"),
    field(name = "isSidechain", method = "is_sidechain", ty = "bool"),
    field(name = "parentUuid", method = "parent_uuid", ty = "Option<String>"),
    field(name = "sessionId", method = "session_id", ty = "String"),
    field(name = "slug", ty = "Option<String>"),
    field(name = "timestamp", ty = "String"),
    field(name = "userType", method = "user_type", ty = "String"),
    field(name = "uuid", ty = "String"),
    field(name = "version", ty = "String"),
    field(name = "parent", ty = "Option<Event>"),
    field(name = "children", ty = "Vec<Event>")
)]
pub enum CoreEvent {
    ProgressEvent(ProgressEvent),
    AssistantEvent(AssistantEvent),
    UserEvent(UserEvent),
    SystemEvent(SystemEvent),
}

// --- CoreEvent shared fields ---

/// Required fields for CoreEvent types.
const CORE_EVENT_REQUIRED: &[(&str, JsonType)] = &[
    ("cwd", JsonType::String),
    ("isSidechain", JsonType::Bool),
    ("sessionId", JsonType::String),
    ("timestamp", JsonType::String),
    ("userType", JsonType::String),
    ("uuid", JsonType::String),
    ("version", JsonType::String),
];

/// All known CoreEvent field names.
const CORE_EVENT_FIELDS: &[&str] = &[
    "cwd",
    "gitBranch",
    "isSidechain",
    "parentUuid",
    "sessionId",
    "slug",
    "timestamp",
    "userType",
    "uuid",
    "version",
];

/// Helper trait for reading common fields from Arc<SessionData> + index.
trait EventValue {
    fn data(&self) -> &Arc<SessionData>;
    fn index(&self) -> usize;

    fn value(&self) -> &Value {
        &self.data().events[self.index()]
    }

    fn str_field(&self, key: &str) -> Option<String> {
        self.value()
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    fn bool_field(&self, key: &str) -> Option<bool> {
        self.value().get(key).and_then(|v| v.as_bool())
    }

    fn int_field(&self, key: &str) -> Option<i32> {
        self.value()
            .get(key)
            .and_then(|v| v.as_i64())
            .map(|n| n as i32)
    }

    fn json_field(&self, key: &str) -> Option<Json<Value>> {
        self.value().get(key).map(|v| Json(v.clone()))
    }

    // Event interface helpers
    fn raw_value(&self) -> Json<Value> {
        Json(self.value().clone())
    }

    fn error_value(&self) -> Option<Json<Value>> {
        self.json_field("error")
    }

    fn api_error_value(&self) -> Option<Json<Value>> {
        self.json_field("apiError")
    }

    fn is_api_error_message_value(&self) -> Option<bool> {
        self.bool_field("isApiErrorMessage")
    }

    // Relational helpers
    fn parent_event(&self) -> Option<Event> {
        let parent_uuid = self.value().get("parentUuid").and_then(|v| v.as_str())?;
        let &idx = self.data().uuid_to_idx.get(parent_uuid)?;
        Some(self.data().make_event(idx))
    }

    fn children_events(&self) -> Vec<Event> {
        let uuid = match self.value().get("uuid").and_then(|v| v.as_str()) {
            Some(u) => u,
            None => return vec![],
        };
        self.data()
            .parent_to_children
            .get(uuid)
            .map(|indices| indices.iter().map(|&idx| self.data().make_event(idx)).collect())
            .unwrap_or_default()
    }
}

/// Macro to define a concrete event struct with EventValue impl.
macro_rules! define_event_struct {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        pub struct $name {
            pub data: Arc<SessionData>,
            pub index: usize,
        }

        impl EventValue for $name {
            fn data(&self) -> &Arc<SessionData> {
                &self.data
            }
            fn index(&self) -> usize {
                self.index
            }
        }
    };
}

// --- Concrete event types ---

// --- Concrete event types ---

define_event_struct!(
    /// A catch-all event type for any events that don't match the known types.
    UnknownEvent
);

#[Object]
impl UnknownEvent {
    #[graphql(name = "type")]
    async fn event_type(&self) -> String { self.str_field("type").unwrap_or_default() }
    async fn raw(&self) -> Json<Value> { self.raw_value() }
    async fn error(&self) -> Option<Json<Value>> { self.error_value() }
    async fn api_error(&self) -> Option<Json<Value>> { self.api_error_value() }
    async fn is_api_error_message(&self) -> Option<bool> { self.is_api_error_message_value() }
}

define_event_struct!(
    /// File history snapshot event.
    FileHistoryEvent
);

#[Object]
impl FileHistoryEvent {
    #[graphql(name = "type")]
    async fn event_type(&self) -> String { "file-history-snapshot".into() }
    async fn raw(&self) -> Json<Value> { self.raw_value() }
    async fn error(&self) -> Option<Json<Value>> { self.error_value() }
    async fn api_error(&self) -> Option<Json<Value>> { self.api_error_value() }
    async fn is_api_error_message(&self) -> Option<bool> { self.is_api_error_message_value() }

    async fn message_id(&self) -> String { self.str_field("messageId").unwrap_or_default() }
    async fn snapshot(&self) -> Json<Value> {
        Json(self.value().get("snapshot").cloned().unwrap_or(Value::Object(Default::default())))
    }
    async fn is_snapshot_update(&self) -> bool { self.bool_field("isSnapshotUpdate").unwrap_or(false) }
}

define_event_struct!(
    /// Queue operation event.
    QueueOperationEvent
);

#[Object]
impl QueueOperationEvent {
    #[graphql(name = "type")]
    async fn event_type(&self) -> String { "queue_operation".into() }
    async fn raw(&self) -> Json<Value> { self.raw_value() }
    async fn error(&self) -> Option<Json<Value>> { self.error_value() }
    async fn api_error(&self) -> Option<Json<Value>> { self.api_error_value() }
    async fn is_api_error_message(&self) -> Option<bool> { self.is_api_error_message_value() }

    async fn timestamp(&self) -> String { self.str_field("timestamp").unwrap_or_default() }
    async fn session_id(&self) -> String { self.str_field("sessionId").unwrap_or_default() }
    async fn operation(&self) -> String { self.str_field("operation").unwrap_or_default() }
    async fn content(&self) -> Option<String> { self.str_field("content") }
}

define_event_struct!(
    /// Progress event.
    ProgressEvent
);

#[Object]
impl ProgressEvent {
    #[graphql(name = "type")]
    async fn event_type(&self) -> String { "progress".into() }
    async fn raw(&self) -> Json<Value> { self.raw_value() }
    async fn error(&self) -> Option<Json<Value>> { self.error_value() }
    async fn api_error(&self) -> Option<Json<Value>> { self.api_error_value() }
    async fn is_api_error_message(&self) -> Option<bool> { self.is_api_error_message_value() }

    // CoreEvent fields
    async fn cwd(&self) -> String { self.str_field("cwd").unwrap_or_default() }
    async fn git_branch(&self) -> Option<String> { self.str_field("gitBranch") }
    async fn is_sidechain(&self) -> bool { self.bool_field("isSidechain").unwrap_or(false) }
    async fn parent_uuid(&self) -> Option<String> { self.str_field("parentUuid") }
    async fn session_id(&self) -> String { self.str_field("sessionId").unwrap_or_default() }
    async fn slug(&self) -> Option<String> { self.str_field("slug") }
    async fn timestamp(&self) -> String { self.str_field("timestamp").unwrap_or_default() }
    async fn user_type(&self) -> String { self.str_field("userType").unwrap_or_default() }
    async fn uuid(&self) -> String { self.str_field("uuid").unwrap_or_default() }
    async fn version(&self) -> String { self.str_field("version").unwrap_or_default() }

    // Relational
    async fn parent(&self) -> Option<Event> { self.parent_event() }
    async fn children(&self) -> Vec<Event> { self.children_events() }

    // ProgressEvent-specific
    async fn agent_id(&self) -> Option<String> { self.str_field("agentId") }
    async fn data(&self) -> Option<Json<Value>> { self.json_field("data") }
    #[graphql(name = "parentToolUseID")]
    async fn parent_tool_use_id(&self) -> Option<String> { self.str_field("parentToolUseID") }
    #[graphql(name = "toolUseID")]
    async fn tool_use_id(&self) -> Option<String> { self.str_field("toolUseID") }
}

define_event_struct!(
    /// Assistant message event.
    AssistantEvent
);

#[Object]
impl AssistantEvent {
    #[graphql(name = "type")]
    async fn event_type(&self) -> String { "assistant".into() }
    async fn raw(&self) -> Json<Value> { self.raw_value() }
    async fn error(&self) -> Option<Json<Value>> { self.error_value() }
    async fn api_error(&self) -> Option<Json<Value>> { self.api_error_value() }
    async fn is_api_error_message(&self) -> Option<bool> { self.is_api_error_message_value() }

    // CoreEvent fields
    async fn cwd(&self) -> String { self.str_field("cwd").unwrap_or_default() }
    async fn git_branch(&self) -> Option<String> { self.str_field("gitBranch") }
    async fn is_sidechain(&self) -> bool { self.bool_field("isSidechain").unwrap_or(false) }
    async fn parent_uuid(&self) -> Option<String> { self.str_field("parentUuid") }
    async fn session_id(&self) -> String { self.str_field("sessionId").unwrap_or_default() }
    async fn slug(&self) -> Option<String> { self.str_field("slug") }
    async fn timestamp(&self) -> String { self.str_field("timestamp").unwrap_or_default() }
    async fn user_type(&self) -> String { self.str_field("userType").unwrap_or_default() }
    async fn uuid(&self) -> String { self.str_field("uuid").unwrap_or_default() }
    async fn version(&self) -> String { self.str_field("version").unwrap_or_default() }

    // Relational
    async fn parent(&self) -> Option<Event> { self.parent_event() }
    async fn children(&self) -> Vec<Event> { self.children_events() }

    // AssistantEvent-specific
    async fn message(&self) -> Json<Value> {
        Json(self.value().get("message").cloned().unwrap_or(Value::Object(Default::default())))
    }
    async fn request_id(&self) -> Option<String> { self.str_field("requestId") }
    async fn agent_id(&self) -> Option<String> { self.str_field("agentId") }
}

define_event_struct!(
    /// User message event.
    UserEvent
);

#[Object]
impl UserEvent {
    #[graphql(name = "type")]
    async fn event_type(&self) -> String { "user".into() }
    async fn raw(&self) -> Json<Value> { self.raw_value() }
    async fn error(&self) -> Option<Json<Value>> { self.error_value() }
    async fn api_error(&self) -> Option<Json<Value>> { self.api_error_value() }
    async fn is_api_error_message(&self) -> Option<bool> { self.is_api_error_message_value() }

    // CoreEvent fields
    async fn cwd(&self) -> String { self.str_field("cwd").unwrap_or_default() }
    async fn git_branch(&self) -> Option<String> { self.str_field("gitBranch") }
    async fn is_sidechain(&self) -> bool { self.bool_field("isSidechain").unwrap_or(false) }
    async fn parent_uuid(&self) -> Option<String> { self.str_field("parentUuid") }
    async fn session_id(&self) -> String { self.str_field("sessionId").unwrap_or_default() }
    async fn slug(&self) -> Option<String> { self.str_field("slug") }
    async fn timestamp(&self) -> String { self.str_field("timestamp").unwrap_or_default() }
    async fn user_type(&self) -> String { self.str_field("userType").unwrap_or_default() }
    async fn uuid(&self) -> String { self.str_field("uuid").unwrap_or_default() }
    async fn version(&self) -> String { self.str_field("version").unwrap_or_default() }

    // Relational
    async fn parent(&self) -> Option<Event> { self.parent_event() }
    async fn children(&self) -> Vec<Event> { self.children_events() }

    // UserEvent-specific
    async fn message(&self) -> Json<Value> {
        Json(self.value().get("message").cloned().unwrap_or(Value::Object(Default::default())))
    }
    #[graphql(name = "sourceToolAssistantUUID")]
    async fn source_tool_assistant_uuid(&self) -> Option<String> {
        self.str_field("sourceToolAssistantUUID")
    }
    async fn tool_use_result(&self) -> Option<Json<Value>> { self.json_field("toolUseResult") }
    async fn agent_id(&self) -> Option<String> { self.str_field("agentId") }
    async fn permission_mode(&self) -> Option<String> { self.str_field("permissionMode") }
    async fn todos(&self) -> Option<Vec<Json<Value>>> {
        self.value().get("todos").and_then(|v| v.as_array())
            .map(|arr| arr.iter().map(|v| Json(v.clone())).collect())
    }
    async fn thinking_metadata(&self) -> Option<Json<Value>> { self.json_field("thinkingMetadata") }
    async fn is_visible_in_transcript_only(&self) -> Option<bool> { self.bool_field("isVisibleInTranscriptOnly") }
    async fn is_compact_summary(&self) -> Option<bool> { self.bool_field("isCompactSummary") }
    async fn image_paste_ids(&self) -> Option<Vec<String>> {
        self.value().get("imagePasteIds").and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
    }
    async fn is_meta(&self) -> Option<bool> { self.bool_field("isMeta") }
    async fn plan_content(&self) -> Option<String> { self.str_field("planContent") }
}

define_event_struct!(
    /// System event.
    SystemEvent
);

#[Object]
impl SystemEvent {
    #[graphql(name = "type")]
    async fn event_type(&self) -> String { "system".into() }
    async fn raw(&self) -> Json<Value> { self.raw_value() }
    async fn error(&self) -> Option<Json<Value>> { self.error_value() }
    async fn api_error(&self) -> Option<Json<Value>> { self.api_error_value() }
    async fn is_api_error_message(&self) -> Option<bool> { self.is_api_error_message_value() }

    // CoreEvent fields
    async fn cwd(&self) -> String { self.str_field("cwd").unwrap_or_default() }
    async fn git_branch(&self) -> Option<String> { self.str_field("gitBranch") }
    async fn is_sidechain(&self) -> bool { self.bool_field("isSidechain").unwrap_or(false) }
    async fn parent_uuid(&self) -> Option<String> { self.str_field("parentUuid") }
    async fn session_id(&self) -> String { self.str_field("sessionId").unwrap_or_default() }
    async fn slug(&self) -> Option<String> { self.str_field("slug") }
    async fn timestamp(&self) -> String { self.str_field("timestamp").unwrap_or_default() }
    async fn user_type(&self) -> String { self.str_field("userType").unwrap_or_default() }
    async fn uuid(&self) -> String { self.str_field("uuid").unwrap_or_default() }
    async fn version(&self) -> String { self.str_field("version").unwrap_or_default() }

    // Relational
    async fn parent(&self) -> Option<Event> { self.parent_event() }
    async fn children(&self) -> Vec<Event> { self.children_events() }

    // SystemEvent-specific
    async fn duration_ms(&self) -> Option<i32> { self.int_field("durationMs") }
    async fn level(&self) -> Option<String> { self.str_field("level") }
    async fn content(&self) -> Option<String> { self.str_field("content") }
    async fn compact_metadata(&self) -> Option<Json<Value>> { self.json_field("compactMetadata") }
    async fn logical_parent_uuid(&self) -> Option<String> { self.str_field("logicalParentUuid") }
    async fn url(&self) -> Option<String> { self.str_field("url") }
    async fn retry_in_ms(&self) -> Option<i32> { self.int_field("retryInMs") }
    async fn retry_attempt(&self) -> Option<i32> { self.int_field("retryAttempt") }
    async fn max_retries(&self) -> Option<i32> { self.int_field("maxRetries") }
    async fn cause(&self) -> Option<Json<Value>> { self.json_field("cause") }
    async fn is_meta(&self) -> Option<bool> { self.bool_field("isMeta") }
    async fn subtype(&self) -> Option<String> { self.str_field("subtype") }
}

/// Paginated session events result.
pub struct SessionEventsData {
    pub events: Vec<Event>,
    pub total: i32,
}

#[Object]
impl SessionEventsData {
    async fn events(&self) -> &[Event] {
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
        let data = Arc::new(SessionData::new(
            all_events,
            self.path.display().to_string(),
        ));

        let events = match page {
            Some(p) => {
                let start = (p.offset as usize).min(data.events.len());
                let end = if p.limit > 0 {
                    (start + p.limit as usize).min(data.events.len())
                } else {
                    data.events.len()
                };
                (start..end).map(|i| data.make_event(i)).collect()
            }
            None => (0..data.events.len()).map(|i| data.make_event(i)).collect(),
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
