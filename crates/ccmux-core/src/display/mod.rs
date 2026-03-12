pub mod format;
pub mod highlight;
pub mod pipeline;
pub mod streaming;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Per-item metadata extracted from raw events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ItemMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
}

/// A display item sent to the client. The client never sees raw Event types.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum DisplayItem {
    UserMessage {
        content: String,
        meta: ItemMeta,
        raw: Value,
    },
    AssistantMessage {
        text: String,
        meta: ItemMeta,
        raw: Value,
    },
    Thinking {
        text: String,
        meta: ItemMeta,
        raw: Value,
    },
    ToolUse {
        name: String,
        tool_use_id: String,
        input: Value,
        result: Option<ToolResultData>,
        meta: ItemMeta,
        raw: Value,
    },
    // TaskList {
    //     tasks: Vec<TaskItem>,
    //     meta: ItemMeta,
    //     raw: Value,
    // },
    TurnDuration {
        duration_ms: u64,
        meta: ItemMeta,
        raw: Value,
    },
    Compaction {
        content: String,
        meta: ItemMeta,
        raw: Value,
    },
    Other {
        raw: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResultData {
    pub output: Option<String>,
    pub error: Option<String>,
    pub raw: Value,
    /// Top-level `toolUseResult` value from the enclosing user event, when present.
    /// Contains structured data for tools like AskUserQuestion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_result: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskItem {
    pub id: String,
    pub subject: String,
    pub status: TaskStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DisplayModeF<T, TArr> {
    Full(T),
    Collapsed(T),
    Grouped(TArr),
    // TaskList(TArr),
    Hidden(T),
}

pub type DisplayMode = DisplayModeF<(), ()>;
pub type DisplayItemWithMode = DisplayModeF<DisplayItem, Vec<DisplayItem>>;

/// Discriminant for DisplayItem variants, used as keys in DisplayOpts.defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplayItemDiscriminant {
    UserMessage,
    AssistantMessage,
    Thinking,
    ToolUse,
    ToolResult,
    TurnDuration,
    Compaction,
    Other,
}

/// Controls how events are mapped to display modes. Configurable per item kind
/// and per tool name. Use `Default::default()` for standard behavior.
#[derive(Debug, Clone)]
pub struct DisplayOpts {
    pub defaults: HashMap<DisplayItemDiscriminant, DisplayMode>,
    pub tool_overrides: HashMap<String, DisplayMode>,
}

impl Default for DisplayOpts {
    fn default() -> Self {
        use DisplayItemDiscriminant::*;
        use DisplayModeF::*;

        let defaults = HashMap::from([
            (UserMessage, Full(())),
            (AssistantMessage, Full(())),
            (Thinking, Grouped(())),
            (ToolUse, Grouped(())),
            (ToolResult, Hidden(())),
            (TurnDuration, Full(())),
            (Compaction, Grouped(())),
            (Other, Hidden(())),
        ]);

        let tool_overrides = HashMap::from([
            ("Bash".to_string(), Full(())),
            ("AskUserQuestion".to_string(), Full(())),
            // ("TaskCreate".to_string(), TaskList),
            // ("TaskUpdate".to_string(), TaskList),
            // ("TaskGet".to_string(), TaskList),
            // ("TaskList".to_string(), TaskList),
        ]);

        Self {
            defaults,
            tool_overrides,
        }
    }
}
