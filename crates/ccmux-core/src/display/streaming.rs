use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{DisplayItem, DisplayMode, DisplayOpts, TaskItem, ToolResultData};

/// Protocol message sent over SSE from server to client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum StreamEvent {
    Append {
        item: DisplayItem,
    },
    MergeWithPrevious {
        item: DisplayItem,
    },
    UpdateToolResult {
        tool_use_id: String,
        result: ToolResultData,
    },
    UpdateTask {
        task: TaskItem,
    },
}

/// Minimal server-side state for streaming decisions.
pub struct StreamPipelineState {
    pub last_mode: Option<DisplayMode>,
    pub opts: DisplayOpts,
    pub tool_results: HashMap<String, ToolResultData>,
}

impl StreamPipelineState {
    pub fn new(opts: DisplayOpts) -> Self {
        Self {
            last_mode: None,
            opts,
            tool_results: HashMap::new(),
        }
    }

    /// Decide how to emit a new display item.
    pub fn emit(&mut self, item: DisplayItem, mode: DisplayMode) -> Option<StreamEvent> {
        match mode {
            DisplayMode::Hidden => None,
            DisplayMode::Grouped => {
                let event = if self.last_mode == Some(DisplayMode::Grouped) {
                    StreamEvent::MergeWithPrevious { item }
                } else {
                    StreamEvent::Append { item }
                };
                self.last_mode = Some(DisplayMode::Grouped);
                Some(event)
            }
            DisplayMode::TaskList => {
                let event = if self.last_mode == Some(DisplayMode::TaskList) {
                    StreamEvent::MergeWithPrevious { item }
                } else {
                    StreamEvent::Append { item }
                };
                self.last_mode = Some(DisplayMode::TaskList);
                Some(event)
            }
            _ => {
                self.last_mode = Some(mode);
                Some(StreamEvent::Append { item })
            }
        }
    }

    /// Index a tool result. Returns an UpdateToolResult event.
    pub fn index_tool_result(
        &mut self,
        tool_use_id: String,
        result: ToolResultData,
    ) -> StreamEvent {
        self.tool_results
            .insert(tool_use_id.clone(), result.clone());
        StreamEvent::UpdateToolResult {
            tool_use_id,
            result,
        }
    }
}
