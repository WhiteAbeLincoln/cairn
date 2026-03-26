pub mod format;
pub mod markdown;
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
    },
    AssistantMessage {
        text: String,
        meta: ItemMeta,
        raw: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
    },
    Thinking {
        text: String,
        meta: ItemMeta,
        raw: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
    },
    ToolUse {
        name: String,
        tool_use_id: String,
        input: Value,
        result: Option<ToolResultData>,
        meta: ItemMeta,
        raw: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
    },
    Compaction {
        content: String,
        meta: ItemMeta,
        raw: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
    },
    Other {
        raw: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
    },
}

impl DisplayItem {
    /// Set the cursor value on this display item.
    pub fn set_cursor(&mut self, cursor_value: String) {
        match self {
            DisplayItem::UserMessage { cursor, .. }
            | DisplayItem::AssistantMessage { cursor, .. }
            | DisplayItem::Thinking { cursor, .. }
            | DisplayItem::ToolUse { cursor, .. }
            | DisplayItem::TurnDuration { cursor, .. }
            | DisplayItem::Compaction { cursor, .. }
            | DisplayItem::Other { cursor, .. } => {
                *cursor = Some(cursor_value);
            }
        }
    }

    /// Get the cursor value from this display item.
    pub fn cursor(&self) -> Option<&str> {
        match self {
            DisplayItem::UserMessage { cursor, .. }
            | DisplayItem::AssistantMessage { cursor, .. }
            | DisplayItem::Thinking { cursor, .. }
            | DisplayItem::ToolUse { cursor, .. }
            | DisplayItem::TurnDuration { cursor, .. }
            | DisplayItem::Compaction { cursor, .. }
            | DisplayItem::Other { cursor, .. } => cursor.as_deref(),
        }
    }
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

/// Encode a byte offset as an opaque cursor string (hex-encoded).
pub fn encode_cursor(offset: u64) -> String {
    format!("{offset:x}")
}

/// Decode an opaque cursor string back to a byte offset.
pub fn decode_cursor(cursor: &str) -> Option<u64> {
    u64::from_str_radix(cursor, 16).ok()
}

/// Controls how events are mapped to display modes. Configurable per item kind
/// and per tool name. Use `Default::default()` for standard behavior.
#[derive(Debug, Clone)]
pub struct DisplayOpts {
    pub defaults: HashMap<DisplayItemDiscriminant, DisplayMode>,
    pub tool_overrides: HashMap<String, DisplayMode>,
}

impl DisplayOpts {
    /// Display options for the markdown API. Only user and assistant messages are Full.
    /// All tools, thinking, and compaction are Grouped. TurnDuration and Other are Hidden.
    pub fn markdown() -> Self {
        use DisplayItemDiscriminant::*;
        use DisplayModeF::*;

        let defaults = HashMap::from([
            (UserMessage, Full(())),
            (AssistantMessage, Full(())),
            (Thinking, Grouped(())),
            (ToolUse, Grouped(())),
            (ToolResult, Hidden(())),
            (TurnDuration, Hidden(())),
            (Compaction, Grouped(())),
            (Other, Hidden(())),
        ]);

        Self {
            defaults,
            tool_overrides: HashMap::new(),
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cursor_roundtrip() {
        let offset = 12345u64;
        let cursor = encode_cursor(offset);
        assert_eq!(decode_cursor(&cursor), Some(offset));
    }

    #[test]
    fn test_cursor_zero() {
        assert_eq!(decode_cursor(&encode_cursor(0)), Some(0));
    }

    #[test]
    fn test_decode_invalid_cursor() {
        assert_eq!(decode_cursor("not_hex!"), None);
    }

    #[test]
    fn test_markdown_display_opts() {
        let opts = DisplayOpts::markdown();
        assert_eq!(
            opts.defaults.get(&DisplayItemDiscriminant::UserMessage),
            Some(&DisplayModeF::Full(()))
        );
        assert_eq!(
            opts.defaults
                .get(&DisplayItemDiscriminant::AssistantMessage),
            Some(&DisplayModeF::Full(()))
        );
        assert_eq!(
            opts.defaults.get(&DisplayItemDiscriminant::Thinking),
            Some(&DisplayModeF::Grouped(()))
        );
        assert_eq!(
            opts.defaults.get(&DisplayItemDiscriminant::ToolUse),
            Some(&DisplayModeF::Grouped(()))
        );
        assert_eq!(
            opts.defaults.get(&DisplayItemDiscriminant::TurnDuration),
            Some(&DisplayModeF::Hidden(()))
        );
        assert_eq!(
            opts.defaults.get(&DisplayItemDiscriminant::Other),
            Some(&DisplayModeF::Hidden(()))
        );
        // No tool overrides — all tools are grouped in markdown view
        assert!(opts.tool_overrides.is_empty());
    }
}
