pub mod parse;

use serde::Deserialize;
use serde_json::Value;

/// Fields present on all event types (error tracking).
#[derive(Deserialize, Default, Debug)]
pub struct EventBaseData {
    #[serde(default)]
    pub error: Option<Value>,
    #[serde(default, rename = "apiError")]
    pub api_error: Option<Value>,
    #[serde(default, rename = "isApiErrorMessage")]
    pub is_api_error_message: Option<bool>,
}

/// Fields present on core events (assistant, user, system, progress).
#[derive(Deserialize, Debug)]
pub struct CoreEventData {
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

#[derive(Deserialize, Debug)]
pub struct AssistantEventData {
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

#[derive(Deserialize, Debug)]
pub struct UserEventData {
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
    #[serde(default, rename = "isCompactSummary")]
    pub is_compact_summary: Option<bool>,
    #[serde(default, rename = "isVisibleInTranscriptOnly")]
    pub is_visible_in_transcript_only: Option<bool>,
}

#[derive(Deserialize, Debug)]
pub struct SystemEventData {
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
    #[serde(default)]
    pub subtype: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct ProgressEventData {
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

#[derive(Deserialize, Debug)]
pub struct FileHistoryEventData {
    #[serde(flatten)]
    pub base: EventBaseData,
    #[serde(rename = "messageId")]
    pub message_id: String,
    pub snapshot: Value,
    #[serde(rename = "isSnapshotUpdate")]
    pub is_snapshot_update: bool,
}

#[derive(Deserialize, Debug)]
pub struct QueueOperationEventData {
    #[serde(flatten)]
    pub base: EventBaseData,
    pub timestamp: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub operation: String,
    #[serde(default)]
    pub content: Option<String>,
}

/// Parsed event enum — internal, not sent across the wire.
#[derive(Debug)]
pub enum Event {
    Assistant(AssistantEventData),
    User(UserEventData),
    System(SystemEventData),
    Progress(ProgressEventData),
    FileHistory(FileHistoryEventData),
    QueueOperation(QueueOperationEventData),
    Unknown(Value),
}
