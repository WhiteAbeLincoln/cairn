use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{
    DisplayItem, DisplayItemWithMode, DisplayMode, DisplayModeF, DisplayOpts, ToolResultData,
};

/// Protocol message sent over SSE from server to client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum StreamEvent {
    Append {
        item: DisplayItemWithMode,
    },
    UpdateToolResult {
        tool_use_id: String,
        result: ToolResultData,
    },
}

/// Minimal server-side state for streaming decisions.
pub struct StreamPipelineState {
    pub opts: DisplayOpts,
    pub tool_results: HashMap<String, ToolResultData>,
}

impl StreamPipelineState {
    pub fn new(opts: DisplayOpts) -> Self {
        Self {
            opts,
            tool_results: HashMap::new(),
        }
    }

    /// Decide how to emit a new display item.
    pub fn emit(&mut self, item: DisplayItem, mode: DisplayMode) -> Option<StreamEvent> {
        match mode {
            DisplayModeF::Hidden(()) => None,
            DisplayModeF::Grouped(()) => Some(StreamEvent::Append {
                item: DisplayModeF::Grouped(vec![item]),
            }),
            DisplayModeF::Full(()) => Some(StreamEvent::Append {
                item: DisplayModeF::Full(item),
            }),
            DisplayModeF::Collapsed(()) => Some(StreamEvent::Append {
                item: DisplayModeF::Collapsed(item),
            }),
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
