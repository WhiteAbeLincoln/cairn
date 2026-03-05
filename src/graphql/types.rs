// clippy gets confused by the #[graphql(...)] attributes on both the interface and concrete types
#![allow(clippy::duplicated_attributes)]
use std::collections::HashMap;
use std::sync::Arc;

use async_graphql::{InputObject, Interface, Json, Object, SimpleObject};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

// --- Pagination ---

#[derive(InputObject)]
pub struct PageInput {
    #[graphql(default)]
    pub offset: i32,
    #[graphql(default)]
    pub limit: i32,
}

// --- Serde data structs ---

/// Fields present on the Event GraphQL interface (shared across all event types).
#[derive(Deserialize, Default)]
pub(crate) struct EventBaseData {
    #[serde(default)]
    pub error: Option<Value>,
    #[serde(default, rename = "apiError")]
    pub api_error: Option<Value>,
    #[serde(default, rename = "isApiErrorMessage")]
    pub is_api_error_message: Option<bool>,
}

/// Fields present on the CoreEvent GraphQL interface.
#[derive(Deserialize)]
pub(crate) struct CoreEventData {
    pub cwd: String,
    #[serde(default, rename = "gitBranch")]
    pub git_branch: Option<String>,
    #[serde(rename = "isSidechain")]
    pub is_sidechain: bool,
    #[serde(default, rename = "parentUuid")]
    pub parent_uuid: Option<String>,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(default)]
    pub slug: Option<String>,
    pub timestamp: String,
    #[serde(rename = "userType")]
    pub user_type: String,
    pub uuid: String,
    pub version: String,
}

#[derive(Deserialize)]
pub(crate) struct FileHistoryEventData {
    #[serde(flatten)]
    pub base: EventBaseData,
    #[serde(rename = "messageId")]
    pub message_id: String,
    pub snapshot: Value,
    #[serde(rename = "isSnapshotUpdate")]
    pub is_snapshot_update: bool,
}

#[derive(Deserialize)]
pub(crate) struct QueueOperationEventData {
    #[serde(flatten)]
    pub base: EventBaseData,
    pub timestamp: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub operation: String,
    #[serde(default)]
    pub content: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct ProgressEventData {
    #[serde(flatten)]
    pub base: EventBaseData,
    #[serde(flatten)]
    pub core: CoreEventData,
    #[serde(default, rename = "agentId")]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub data: Option<Value>,
    #[serde(default, rename = "parentToolUseID")]
    pub parent_tool_use_id: Option<String>,
    #[serde(default, rename = "toolUseID")]
    pub tool_use_id: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct AssistantEventData {
    #[serde(flatten)]
    pub base: EventBaseData,
    #[serde(flatten)]
    pub core: CoreEventData,
    pub message: Value,
    #[serde(default, rename = "requestId")]
    pub request_id: Option<String>,
    #[serde(default, rename = "agentId")]
    pub agent_id: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct UserEventData {
    #[serde(flatten)]
    pub base: EventBaseData,
    #[serde(flatten)]
    pub core: CoreEventData,
    pub message: Value,
    #[serde(default, rename = "sourceToolAssistantUUID")]
    pub source_tool_assistant_uuid: Option<String>,
    #[serde(default, rename = "toolUseResult")]
    pub tool_use_result: Option<Value>,
    #[serde(default, rename = "agentId")]
    pub agent_id: Option<String>,
    #[serde(default, rename = "permissionMode")]
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub todos: Option<Vec<Value>>,
    #[serde(default, rename = "thinkingMetadata")]
    pub thinking_metadata: Option<Value>,
    #[serde(default, rename = "isVisibleInTranscriptOnly")]
    pub is_visible_in_transcript_only: Option<bool>,
    #[serde(default, rename = "isCompactSummary")]
    pub is_compact_summary: Option<bool>,
    #[serde(default, rename = "imagePasteIds")]
    pub image_paste_ids: Option<Vec<String>>,
    #[serde(default, rename = "isMeta")]
    pub is_meta: Option<bool>,
    #[serde(default, rename = "planContent")]
    pub plan_content: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct SystemEventData {
    #[serde(flatten)]
    pub base: EventBaseData,
    #[serde(flatten)]
    pub core: CoreEventData,
    #[serde(default, rename = "durationMs")]
    pub duration_ms: Option<i64>,
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default, rename = "compactMetadata")]
    pub compact_metadata: Option<Value>,
    #[serde(default, rename = "logicalParentUuid")]
    pub logical_parent_uuid: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default, rename = "retryInMs")]
    pub retry_in_ms: Option<i64>,
    #[serde(default, rename = "retryAttempt")]
    pub retry_attempt: Option<i64>,
    #[serde(default, rename = "maxRetries")]
    pub max_retries: Option<i64>,
    #[serde(default)]
    pub cause: Option<Value>,
    #[serde(default, rename = "isMeta")]
    pub is_meta: Option<bool>,
    #[serde(default)]
    pub subtype: Option<String>,
}

/// Pre-parsed event data. Stored once in SessionData, referenced by index.
pub(crate) enum ParsedEvent {
    Unknown,
    FileHistory(FileHistoryEventData),
    QueueOperation(QueueOperationEventData),
    Progress(ProgressEventData),
    Assistant(AssistantEventData),
    User(UserEventData),
    System(SystemEventData),
}

// --- Session events ---

/// Pre-indexed session data shared across all events in a query response.
pub struct SessionData {
    pub raw: Vec<Value>,
    pub(crate) parsed: Vec<ParsedEvent>,
    pub uuid_to_idx: HashMap<String, usize>,
    pub parent_to_children: HashMap<String, Vec<usize>>,
    pub file_path: String,
}

impl SessionData {
    pub fn new(raw: Vec<Value>, file_path: String) -> Self {
        let mut uuid_to_idx = HashMap::new();
        let mut parent_to_children: HashMap<String, Vec<usize>> = HashMap::new();

        let parsed: Vec<ParsedEvent> = raw
            .iter()
            .enumerate()
            .map(|(i, ev)| {
                if let Some(uuid) = ev.get("uuid").and_then(|v| v.as_str()) {
                    uuid_to_idx.insert(uuid.to_string(), i);
                }
                if let Some(parent) = ev.get("parentUuid").and_then(|v| v.as_str()) {
                    parent_to_children
                        .entry(parent.to_string())
                        .or_default()
                        .push(i);
                }
                Self::parse_event(ev, i, &file_path)
            })
            .collect();

        SessionData {
            raw,
            parsed,
            uuid_to_idx,
            parent_to_children,
            file_path,
        }
    }

    /// Parse a single raw event into a typed ParsedEvent.
    fn parse_event(ev: &Value, i: usize, file_path: &str) -> ParsedEvent {
        let event_type = ev.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let event_uuid = ev.get("uuid").and_then(|v| v.as_str()).unwrap_or("");
        let line = i + 1;
        let _span = tracing::warn_span!(
            "event",
            event_type,
            line,
            uuid = event_uuid,
            file = %file_path,
        )
        .entered();

        macro_rules! try_parse {
            ($variant:ident, $data_type:ty) => {
                match <$data_type>::deserialize(ev) {
                    Ok(data) => ParsedEvent::$variant(data),
                    Err(e) => {
                        tracing::warn!(%e, "failed to parse, falling back to UnknownEvent");
                        ParsedEvent::Unknown
                    }
                }
            };
        }

        match event_type {
            "file-history-snapshot" => try_parse!(FileHistory, FileHistoryEventData),
            "queue_operation" => try_parse!(QueueOperation, QueueOperationEventData),
            "progress" => try_parse!(Progress, ProgressEventData),
            "assistant" => try_parse!(Assistant, AssistantEventData),
            "user" => try_parse!(User, UserEventData),
            "system" => try_parse!(System, SystemEventData),
            _ => ParsedEvent::Unknown,
        }
    }

    /// Construct a typed Event from the event at the given index.
    pub fn make_event(self: &Arc<Self>, i: usize) -> Event {
        let d = Arc::clone(self);
        match &self.parsed[i] {
            ParsedEvent::Unknown => Event::UnknownEvent(UnknownEvent { data: d, index: i }),
            ParsedEvent::FileHistory(_) => {
                Event::FileHistoryEvent(FileHistoryEvent { data: d, index: i })
            }
            ParsedEvent::QueueOperation(_) => {
                Event::QueueOperationEvent(QueueOperationEvent { data: d, index: i })
            }
            ParsedEvent::Progress(_) => Event::ProgressEvent(ProgressEvent { data: d, index: i }),
            ParsedEvent::Assistant(_) => {
                Event::AssistantEvent(AssistantEvent { data: d, index: i })
            }
            ParsedEvent::User(_) => Event::UserEvent(UserEvent { data: d, index: i }),
            ParsedEvent::System(_) => Event::SystemEvent(SystemEvent { data: d, index: i }),
        }
    }
}

// --- Event interface ---

#[derive(Interface)]
#[graphql(
    field(name = "type", method = "event_type", ty = "&str"),
    field(name = "raw", ty = "Json<&Value>"),
    field(name = "error", ty = "Option<Json<&Value>>"),
    field(name = "apiError", method = "api_error", ty = "Option<Json<&Value>>"),
    field(
        name = "isApiErrorMessage",
        method = "is_api_error_message",
        ty = "Option<bool>"
    )
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
    field(name = "cwd", ty = "&str"),
    field(name = "gitBranch", method = "git_branch", ty = "Option<&str>"),
    field(name = "isSidechain", method = "is_sidechain", ty = "bool"),
    field(name = "parentUuid", method = "parent_uuid", ty = "Option<&str>"),
    field(name = "sessionId", method = "session_id", ty = "&str"),
    field(name = "slug", ty = "Option<&str>"),
    field(name = "timestamp", ty = "&str"),
    field(name = "userType", method = "user_type", ty = "&str"),
    field(name = "uuid", ty = "&str"),
    field(name = "version", ty = "&str"),
    field(name = "parent", ty = "Option<Event>"),
    field(name = "children", ty = "Vec<Event>")
)]
pub enum CoreEvent {
    ProgressEvent(ProgressEvent),
    AssistantEvent(AssistantEvent),
    UserEvent(UserEvent),
    SystemEvent(SystemEvent),
}

// --- Helpers for accessing parsed data from Arc<SessionData> ---

/// Helper to resolve parent/children from CoreEventData.
trait CoreEventResolvers {
    fn session_data(&self) -> &Arc<SessionData>;
    fn core(&self) -> &CoreEventData;

    fn parent_event(&self) -> Option<Event> {
        let parent_uuid = self.core().parent_uuid.as_deref()?;
        let &idx = self.session_data().uuid_to_idx.get(parent_uuid)?;
        Some(self.session_data().make_event(idx))
    }

    fn children_events(&self) -> Vec<Event> {
        self.session_data()
            .parent_to_children
            .get(&self.core().uuid)
            .map(|indices| {
                indices
                    .iter()
                    .map(|&idx| self.session_data().make_event(idx))
                    .collect()
            })
            .unwrap_or_default()
    }
}

// --- Concrete event types ---

pub struct UnknownEvent {
    pub data: Arc<SessionData>,
    pub index: usize,
}

#[Object]
impl UnknownEvent {
    #[graphql(name = "type")]
    async fn event_type(&self) -> &str {
        self.data.raw[self.index]
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
    }
    async fn raw(&self) -> Json<&Value> {
        Json(&self.data.raw[self.index])
    }
    async fn error(&self) -> Option<Json<&Value>> {
        self.data.raw[self.index].get("error").map(Json)
    }
    async fn api_error(&self) -> Option<Json<&Value>> {
        self.data.raw[self.index].get("apiError").map(Json)
    }
    async fn is_api_error_message(&self) -> Option<bool> {
        self.data.raw[self.index]
            .get("isApiErrorMessage")
            .and_then(|v| v.as_bool())
    }
}

pub struct FileHistoryEvent {
    data: Arc<SessionData>,
    index: usize,
}

impl FileHistoryEvent {
    fn inner(&self) -> &FileHistoryEventData {
        match &self.data.parsed[self.index] {
            ParsedEvent::FileHistory(d) => d,
            _ => unreachable!(),
        }
    }
}

#[Object]
impl FileHistoryEvent {
    #[graphql(name = "type")]
    async fn event_type(&self) -> &str {
        "file-history-snapshot"
    }
    async fn raw(&self) -> Json<&Value> {
        Json(&self.data.raw[self.index])
    }
    async fn error(&self) -> Option<Json<&Value>> {
        self.inner().base.error.as_ref().map(Json)
    }
    async fn api_error(&self) -> Option<Json<&Value>> {
        self.inner().base.api_error.as_ref().map(Json)
    }
    async fn is_api_error_message(&self) -> Option<bool> {
        self.inner().base.is_api_error_message
    }

    async fn message_id(&self) -> &str {
        &self.inner().message_id
    }
    async fn snapshot(&self) -> Json<&Value> {
        Json(&self.inner().snapshot)
    }
    async fn is_snapshot_update(&self) -> bool {
        self.inner().is_snapshot_update
    }
}

pub struct QueueOperationEvent {
    data: Arc<SessionData>,
    index: usize,
}

impl QueueOperationEvent {
    fn inner(&self) -> &QueueOperationEventData {
        match &self.data.parsed[self.index] {
            ParsedEvent::QueueOperation(d) => d,
            _ => unreachable!(),
        }
    }
}

#[Object]
impl QueueOperationEvent {
    #[graphql(name = "type")]
    async fn event_type(&self) -> &str {
        "queue_operation"
    }
    async fn raw(&self) -> Json<&Value> {
        Json(&self.data.raw[self.index])
    }
    async fn error(&self) -> Option<Json<&Value>> {
        self.inner().base.error.as_ref().map(Json)
    }
    async fn api_error(&self) -> Option<Json<&Value>> {
        self.inner().base.api_error.as_ref().map(Json)
    }
    async fn is_api_error_message(&self) -> Option<bool> {
        self.inner().base.is_api_error_message
    }

    async fn timestamp(&self) -> &str {
        &self.inner().timestamp
    }
    async fn session_id(&self) -> &str {
        &self.inner().session_id
    }
    async fn operation(&self) -> &str {
        &self.inner().operation
    }
    async fn content(&self) -> Option<&str> {
        self.inner().content.as_deref()
    }
}

pub struct ProgressEvent {
    data: Arc<SessionData>,
    index: usize,
}

impl ProgressEvent {
    fn inner(&self) -> &ProgressEventData {
        match &self.data.parsed[self.index] {
            ParsedEvent::Progress(d) => d,
            _ => unreachable!(),
        }
    }
}

impl CoreEventResolvers for ProgressEvent {
    fn session_data(&self) -> &Arc<SessionData> {
        &self.data
    }
    fn core(&self) -> &CoreEventData {
        &self.inner().core
    }
}

#[Object]
impl ProgressEvent {
    #[graphql(name = "type")]
    async fn event_type(&self) -> &str {
        "progress"
    }
    async fn raw(&self) -> Json<&Value> {
        Json(&self.data.raw[self.index])
    }
    async fn error(&self) -> Option<Json<&Value>> {
        self.inner().base.error.as_ref().map(Json)
    }
    async fn api_error(&self) -> Option<Json<&Value>> {
        self.inner().base.api_error.as_ref().map(Json)
    }
    async fn is_api_error_message(&self) -> Option<bool> {
        self.inner().base.is_api_error_message
    }

    // CoreEvent fields
    async fn cwd(&self) -> &str {
        &self.inner().core.cwd
    }
    async fn git_branch(&self) -> Option<&str> {
        self.inner().core.git_branch.as_deref()
    }
    async fn is_sidechain(&self) -> bool {
        self.inner().core.is_sidechain
    }
    async fn parent_uuid(&self) -> Option<&str> {
        self.inner().core.parent_uuid.as_deref()
    }
    async fn session_id(&self) -> &str {
        &self.inner().core.session_id
    }
    async fn slug(&self) -> Option<&str> {
        self.inner().core.slug.as_deref()
    }
    async fn timestamp(&self) -> &str {
        &self.inner().core.timestamp
    }
    async fn user_type(&self) -> &str {
        &self.inner().core.user_type
    }
    async fn uuid(&self) -> &str {
        &self.inner().core.uuid
    }
    async fn version(&self) -> &str {
        &self.inner().core.version
    }

    // Relational
    async fn parent(&self) -> Option<Event> {
        self.parent_event()
    }
    async fn children(&self) -> Vec<Event> {
        self.children_events()
    }

    // ProgressEvent-specific
    async fn agent_id(&self) -> Option<&str> {
        self.inner().agent_id.as_deref()
    }
    async fn data(&self) -> Option<Json<&Value>> {
        self.inner().data.as_ref().map(Json)
    }
    #[graphql(name = "parentToolUseID")]
    async fn parent_tool_use_id(&self) -> Option<&str> {
        self.inner().parent_tool_use_id.as_deref()
    }
    #[graphql(name = "toolUseID")]
    async fn tool_use_id(&self) -> Option<&str> {
        self.inner().tool_use_id.as_deref()
    }
}

pub struct AssistantEvent {
    data: Arc<SessionData>,
    index: usize,
}

impl AssistantEvent {
    fn inner(&self) -> &AssistantEventData {
        match &self.data.parsed[self.index] {
            ParsedEvent::Assistant(d) => d,
            _ => unreachable!(),
        }
    }
}

impl CoreEventResolvers for AssistantEvent {
    fn session_data(&self) -> &Arc<SessionData> {
        &self.data
    }
    fn core(&self) -> &CoreEventData {
        &self.inner().core
    }
}

#[Object]
impl AssistantEvent {
    #[graphql(name = "type")]
    async fn event_type(&self) -> &str {
        "assistant"
    }
    async fn raw(&self) -> Json<&Value> {
        Json(&self.data.raw[self.index])
    }
    async fn error(&self) -> Option<Json<&Value>> {
        self.inner().base.error.as_ref().map(Json)
    }
    async fn api_error(&self) -> Option<Json<&Value>> {
        self.inner().base.api_error.as_ref().map(Json)
    }
    async fn is_api_error_message(&self) -> Option<bool> {
        self.inner().base.is_api_error_message
    }

    // CoreEvent fields
    async fn cwd(&self) -> &str {
        &self.inner().core.cwd
    }
    async fn git_branch(&self) -> Option<&str> {
        self.inner().core.git_branch.as_deref()
    }
    async fn is_sidechain(&self) -> bool {
        self.inner().core.is_sidechain
    }
    async fn parent_uuid(&self) -> Option<&str> {
        self.inner().core.parent_uuid.as_deref()
    }
    async fn session_id(&self) -> &str {
        &self.inner().core.session_id
    }
    async fn slug(&self) -> Option<&str> {
        self.inner().core.slug.as_deref()
    }
    async fn timestamp(&self) -> &str {
        &self.inner().core.timestamp
    }
    async fn user_type(&self) -> &str {
        &self.inner().core.user_type
    }
    async fn uuid(&self) -> &str {
        &self.inner().core.uuid
    }
    async fn version(&self) -> &str {
        &self.inner().core.version
    }

    // Relational
    async fn parent(&self) -> Option<Event> {
        self.parent_event()
    }
    async fn children(&self) -> Vec<Event> {
        self.children_events()
    }

    // AssistantEvent-specific
    async fn message(&self) -> Json<&Value> {
        Json(&self.inner().message)
    }
    async fn request_id(&self) -> Option<&str> {
        self.inner().request_id.as_deref()
    }
    async fn agent_id(&self) -> Option<&str> {
        self.inner().agent_id.as_deref()
    }
}

pub struct UserEvent {
    data: Arc<SessionData>,
    index: usize,
}

impl UserEvent {
    fn inner(&self) -> &UserEventData {
        match &self.data.parsed[self.index] {
            ParsedEvent::User(d) => d,
            _ => unreachable!(),
        }
    }
}

impl CoreEventResolvers for UserEvent {
    fn session_data(&self) -> &Arc<SessionData> {
        &self.data
    }
    fn core(&self) -> &CoreEventData {
        &self.inner().core
    }
}

#[Object]
impl UserEvent {
    #[graphql(name = "type")]
    async fn event_type(&self) -> &str {
        "user"
    }
    async fn raw(&self) -> Json<&Value> {
        Json(&self.data.raw[self.index])
    }
    async fn error(&self) -> Option<Json<&Value>> {
        self.inner().base.error.as_ref().map(Json)
    }
    async fn api_error(&self) -> Option<Json<&Value>> {
        self.inner().base.api_error.as_ref().map(Json)
    }
    async fn is_api_error_message(&self) -> Option<bool> {
        self.inner().base.is_api_error_message
    }

    // CoreEvent fields
    async fn cwd(&self) -> &str {
        &self.inner().core.cwd
    }
    async fn git_branch(&self) -> Option<&str> {
        self.inner().core.git_branch.as_deref()
    }
    async fn is_sidechain(&self) -> bool {
        self.inner().core.is_sidechain
    }
    async fn parent_uuid(&self) -> Option<&str> {
        self.inner().core.parent_uuid.as_deref()
    }
    async fn session_id(&self) -> &str {
        &self.inner().core.session_id
    }
    async fn slug(&self) -> Option<&str> {
        self.inner().core.slug.as_deref()
    }
    async fn timestamp(&self) -> &str {
        &self.inner().core.timestamp
    }
    async fn user_type(&self) -> &str {
        &self.inner().core.user_type
    }
    async fn uuid(&self) -> &str {
        &self.inner().core.uuid
    }
    async fn version(&self) -> &str {
        &self.inner().core.version
    }

    // Relational
    async fn parent(&self) -> Option<Event> {
        self.parent_event()
    }
    async fn children(&self) -> Vec<Event> {
        self.children_events()
    }

    // UserEvent-specific
    async fn message(&self) -> Json<&Value> {
        Json(&self.inner().message)
    }
    #[graphql(name = "sourceToolAssistantUUID")]
    async fn source_tool_assistant_uuid(&self) -> Option<&str> {
        self.inner().source_tool_assistant_uuid.as_deref()
    }
    async fn tool_use_result(&self) -> Option<Json<&Value>> {
        self.inner().tool_use_result.as_ref().map(Json)
    }
    async fn agent_id(&self) -> Option<&str> {
        self.inner().agent_id.as_deref()
    }
    async fn permission_mode(&self) -> Option<&str> {
        self.inner().permission_mode.as_deref()
    }
    async fn todos(&self) -> Option<Vec<Json<&Value>>> {
        self.inner()
            .todos
            .as_ref()
            .map(|arr| arr.iter().map(Json).collect())
    }
    async fn thinking_metadata(&self) -> Option<Json<&Value>> {
        self.inner().thinking_metadata.as_ref().map(Json)
    }
    async fn is_visible_in_transcript_only(&self) -> Option<bool> {
        self.inner().is_visible_in_transcript_only
    }
    async fn is_compact_summary(&self) -> Option<bool> {
        self.inner().is_compact_summary
    }
    async fn image_paste_ids(&self) -> Option<&[String]> {
        self.inner().image_paste_ids.as_deref()
    }
    async fn is_meta(&self) -> Option<bool> {
        self.inner().is_meta
    }
    async fn plan_content(&self) -> Option<&str> {
        self.inner().plan_content.as_deref()
    }
}

pub struct SystemEvent {
    data: Arc<SessionData>,
    index: usize,
}

impl SystemEvent {
    fn inner(&self) -> &SystemEventData {
        match &self.data.parsed[self.index] {
            ParsedEvent::System(d) => d,
            _ => unreachable!(),
        }
    }
}

impl CoreEventResolvers for SystemEvent {
    fn session_data(&self) -> &Arc<SessionData> {
        &self.data
    }
    fn core(&self) -> &CoreEventData {
        &self.inner().core
    }
}

#[Object]
impl SystemEvent {
    #[graphql(name = "type")]
    async fn event_type(&self) -> &str {
        "system"
    }
    async fn raw(&self) -> Json<&Value> {
        Json(&self.data.raw[self.index])
    }
    async fn error(&self) -> Option<Json<&Value>> {
        self.inner().base.error.as_ref().map(Json)
    }
    async fn api_error(&self) -> Option<Json<&Value>> {
        self.inner().base.api_error.as_ref().map(Json)
    }
    async fn is_api_error_message(&self) -> Option<bool> {
        self.inner().base.is_api_error_message
    }

    // CoreEvent fields
    async fn cwd(&self) -> &str {
        &self.inner().core.cwd
    }
    async fn git_branch(&self) -> Option<&str> {
        self.inner().core.git_branch.as_deref()
    }
    async fn is_sidechain(&self) -> bool {
        self.inner().core.is_sidechain
    }
    async fn parent_uuid(&self) -> Option<&str> {
        self.inner().core.parent_uuid.as_deref()
    }
    async fn session_id(&self) -> &str {
        &self.inner().core.session_id
    }
    async fn slug(&self) -> Option<&str> {
        self.inner().core.slug.as_deref()
    }
    async fn timestamp(&self) -> &str {
        &self.inner().core.timestamp
    }
    async fn user_type(&self) -> &str {
        &self.inner().core.user_type
    }
    async fn uuid(&self) -> &str {
        &self.inner().core.uuid
    }
    async fn version(&self) -> &str {
        &self.inner().core.version
    }

    // Relational
    async fn parent(&self) -> Option<Event> {
        self.parent_event()
    }
    async fn children(&self) -> Vec<Event> {
        self.children_events()
    }

    // SystemEvent-specific
    async fn duration_ms(&self) -> Option<i32> {
        self.inner().duration_ms.map(|v| v as i32)
    }
    async fn level(&self) -> Option<&str> {
        self.inner().level.as_deref()
    }
    async fn content(&self) -> Option<&str> {
        self.inner().content.as_deref()
    }
    async fn compact_metadata(&self) -> Option<Json<&Value>> {
        self.inner().compact_metadata.as_ref().map(Json)
    }
    async fn logical_parent_uuid(&self) -> Option<&str> {
        self.inner().logical_parent_uuid.as_deref()
    }
    async fn url(&self) -> Option<&str> {
        self.inner().url.as_deref()
    }
    async fn retry_in_ms(&self) -> Option<i32> {
        self.inner().retry_in_ms.map(|v| v as i32)
    }
    async fn retry_attempt(&self) -> Option<i32> {
        self.inner().retry_attempt.map(|v| v as i32)
    }
    async fn max_retries(&self) -> Option<i32> {
        self.inner().max_retries.map(|v| v as i32)
    }
    async fn cause(&self) -> Option<Json<&Value>> {
        self.inner().cause.as_ref().map(Json)
    }
    async fn is_meta(&self) -> Option<bool> {
        self.inner().is_meta
    }
    async fn subtype(&self) -> Option<&str> {
        self.inner().subtype.as_deref()
    }
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
        std::fs::read_to_string(&self.path).map_err(|e| async_graphql::Error::new(e.to_string()))
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
                let start = (p.offset as usize).min(data.raw.len());
                let end = if p.limit > 0 {
                    (start + p.limit as usize).min(data.raw.len())
                } else {
                    data.raw.len()
                };
                (start..end).map(|i| data.make_event(i)).collect()
            }
            None => (0..data.raw.len()).map(|i| data.make_event(i)).collect(),
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
