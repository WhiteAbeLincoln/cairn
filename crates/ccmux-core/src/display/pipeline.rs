use std::collections::HashMap;

use serde_json::Value;

use super::{
    DisplayItem, DisplayItemDiscriminant, DisplayMode, DisplayOpts, TaskItem, TaskStatus,
    ToolResultData,
};
use crate::events::Event;

/// Convert events + raw JSON into display items, applying grouping logic.
pub fn events_to_display_items(
    events: &[Event],
    raw_events: &[Value],
    opts: &DisplayOpts,
) -> Vec<DisplayItem> {
    // Pre-scan: index tool results by tool_use_id from user events.
    let tool_results = pre_scan_tool_results(raw_events);

    let mut output: Vec<DisplayItem> = Vec::new();
    let mut grouped_acc: Vec<DisplayItem> = Vec::new();
    let mut task_acc: Vec<(DisplayItem, Value)> = Vec::new(); // (item, raw) for task extraction

    for (event, raw) in events.iter().zip(raw_events.iter()) {
        let intermediates = event_to_intermediates(event, raw, opts, &tool_results);

        for (item, mode) in intermediates {
            match mode {
                DisplayMode::TaskList => {
                    flush_grouped(&mut grouped_acc, &mut output);
                    task_acc.push((item, raw.clone()));
                }
                DisplayMode::Grouped => {
                    flush_tasks(&mut task_acc, &mut output);
                    grouped_acc.push(item);
                }
                DisplayMode::Hidden => {
                    // skip
                }
                _ => {
                    // Full or Collapsed — flush accumulators and emit directly
                    flush_grouped(&mut grouped_acc, &mut output);
                    flush_tasks(&mut task_acc, &mut output);
                    output.push(item);
                }
            }
        }
    }

    flush_grouped(&mut grouped_acc, &mut output);
    flush_tasks(&mut task_acc, &mut output);

    output
}

/// Convert a single event to display items (for streaming pipeline).
pub fn single_event_to_display_items(
    event: &Event,
    raw: &Value,
    opts: &DisplayOpts,
    tool_results: &HashMap<String, ToolResultData>,
) -> Vec<(DisplayItem, DisplayMode)> {
    event_to_intermediates(event, raw, opts, tool_results)
}

/// Extract tool results from a single raw event (for streaming pipeline).
pub fn extract_tool_results_from_event(raw: &Value) -> Vec<(String, ToolResultData)> {
    let event_type = raw.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if event_type != "user" {
        return vec![];
    }

    let content = raw.pointer("/message/content").and_then(|v| v.as_array());
    let Some(items) = content else {
        return vec![];
    };

    let mut results = Vec::new();
    for item in items {
        if item.get("type").and_then(|v| v.as_str()) == Some("tool_result") {
            let tool_use_id = item
                .get("tool_use_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            if tool_use_id.is_empty() {
                continue;
            }

            let content_val = item.get("content");
            let output = content_val.and_then(|v| v.as_str()).map(|s| s.to_string());
            let error = item
                .get("is_error")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
                .then(|| {
                    output
                        .clone()
                        .unwrap_or_else(|| "unknown error".to_string())
                });

            results.push((
                tool_use_id,
                ToolResultData {
                    output: if error.is_some() { None } else { output },
                    error,
                    raw: item.clone(),
                },
            ));
        }
    }
    results
}

/// Pre-scan all raw events for tool_result content blocks in user messages.
fn pre_scan_tool_results(raw_events: &[Value]) -> HashMap<String, ToolResultData> {
    raw_events
        .iter()
        .flat_map(extract_tool_results_from_event)
        .collect()
}

/// Convert a single event into intermediate (DisplayItem, DisplayMode) pairs.
fn event_to_intermediates(
    event: &Event,
    raw: &Value,
    opts: &DisplayOpts,
    tool_results: &HashMap<String, ToolResultData>,
) -> Vec<(DisplayItem, DisplayMode)> {
    match event {
        Event::User(data) => user_event_items(data, raw, opts),
        Event::Assistant(data) => assistant_event_items(data, raw, opts, tool_results),
        Event::System(data) => system_event_items(data, raw, opts),
        Event::Progress(_)
        | Event::FileHistory(_)
        | Event::QueueOperation(_)
        | Event::Unknown(_) => {
            vec![]
        }
    }
}

fn user_event_items(
    data: &crate::events::UserEventData,
    raw: &Value,
    opts: &DisplayOpts,
) -> Vec<(DisplayItem, DisplayMode)> {
    // Compaction check
    if data.is_compact_summary == Some(true) {
        let content = data
            .message
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let mode = mode_for(DisplayItemDiscriminant::Compaction, None, opts);
        return vec![(
            DisplayItem::Compaction {
                content,
                raw: raw.clone(),
            },
            mode,
        )];
    }

    let msg_content = data.message.get("content");

    // String content from external user without tool result
    if let Some(text) = msg_content.and_then(|v| v.as_str()) {
        let role = data.message.get("role").and_then(|v| v.as_str());
        if role == Some("user")
            && data.core.user_type == "external"
            && data.tool_use_result.is_none()
            && data.source_tool_assistant_uuid.is_none()
        {
            let mode = mode_for(DisplayItemDiscriminant::UserMessage, None, opts);
            return vec![(
                DisplayItem::UserMessage {
                    content: text.to_string(),
                    raw: raw.clone(),
                },
                mode,
            )];
        }
    }

    // Array content: extract tool_results (hidden), rest -> Other
    if let Some(items) = msg_content.and_then(|v| v.as_array()) {
        let mut result = Vec::new();
        let mut others = Vec::new();

        for item in items {
            if item.get("type").and_then(|v| v.as_str()) == Some("tool_result") {
                // tool_result items are hidden (they're pre-scanned and paired with tool_use)
                let mode = mode_for(DisplayItemDiscriminant::ToolResult, None, opts);
                result.push((DisplayItem::Other { raw: raw.clone() }, mode));
            } else {
                others.push(item.clone());
            }
        }

        if !others.is_empty() {
            let mode = mode_for(DisplayItemDiscriminant::Other, None, opts);
            result.push((DisplayItem::Other { raw: raw.clone() }, mode));
        }

        return result;
    }

    // Fallback
    let mode = mode_for(DisplayItemDiscriminant::Other, None, opts);
    vec![(DisplayItem::Other { raw: raw.clone() }, mode)]
}

fn assistant_event_items(
    data: &crate::events::AssistantEventData,
    raw: &Value,
    opts: &DisplayOpts,
    tool_results: &HashMap<String, ToolResultData>,
) -> Vec<(DisplayItem, DisplayMode)> {
    // Skip synthetic messages
    let model = data
        .message
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if model == "<synthetic>" {
        return vec![];
    }

    let content = data.message.get("content").and_then(|v| v.as_array());
    let Some(items) = content else {
        let mode = mode_for(DisplayItemDiscriminant::Other, None, opts);
        return vec![(DisplayItem::Other { raw: raw.clone() }, mode)];
    };

    let mut result = Vec::new();

    for item in items {
        let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match item_type {
            "text" => {
                let text = item.get("text").and_then(|v| v.as_str()).unwrap_or("");
                let mode = mode_for(DisplayItemDiscriminant::AssistantMessage, None, opts);
                result.push((
                    DisplayItem::AssistantMessage {
                        text: text.to_string(),
                        raw: raw.clone(),
                    },
                    mode,
                ));
            }
            "thinking" => {
                let text = item.get("thinking").and_then(|v| v.as_str()).unwrap_or("");
                let mode = mode_for(DisplayItemDiscriminant::Thinking, None, opts);
                result.push((
                    DisplayItem::Thinking {
                        text: text.to_string(),
                        raw: raw.clone(),
                    },
                    mode,
                ));
            }
            "tool_use" => {
                let name = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let id = item
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let input = item.get("input").cloned().unwrap_or(Value::Null);

                let mode = mode_for(DisplayItemDiscriminant::ToolUse, Some(&name), opts);

                // Check if this is a task-related tool
                if mode == DisplayMode::TaskList {
                    let tasks = extract_task_items(&name, &id, &input);
                    result.push((
                        DisplayItem::TaskList {
                            tasks,
                            raw: raw.clone(),
                        },
                        mode,
                    ));
                } else {
                    let tool_result = tool_results.get(&id).cloned();
                    result.push((
                        DisplayItem::ToolUse {
                            name,
                            tool_use_id: id,
                            input,
                            result: tool_result,
                            raw: raw.clone(),
                        },
                        mode,
                    ));
                }
            }
            _ => {
                let mode = mode_for(DisplayItemDiscriminant::Other, None, opts);
                result.push((DisplayItem::Other { raw: raw.clone() }, mode));
            }
        }
    }

    result
}

fn system_event_items(
    data: &crate::events::SystemEventData,
    raw: &Value,
    opts: &DisplayOpts,
) -> Vec<(DisplayItem, DisplayMode)> {
    if data.subtype.as_deref() == Some("turn_duration") {
        let duration_ms = data.duration_ms.unwrap_or(0) as u64;
        let mode = mode_for(DisplayItemDiscriminant::TurnDuration, None, opts);
        return vec![(
            DisplayItem::TurnDuration {
                duration_ms,
                raw: raw.clone(),
            },
            mode,
        )];
    }

    let mode = mode_for(DisplayItemDiscriminant::Other, None, opts);
    vec![(DisplayItem::Other { raw: raw.clone() }, mode)]
}

/// Determine display mode for an item, checking tool overrides first.
fn mode_for(
    discriminant: DisplayItemDiscriminant,
    tool_name: Option<&str>,
    opts: &DisplayOpts,
) -> DisplayMode {
    if discriminant == DisplayItemDiscriminant::ToolUse
        && let Some(name) = tool_name
        && let Some(&mode) = opts.tool_overrides.get(name)
    {
        return mode;
    }
    opts.defaults
        .get(&discriminant)
        .copied()
        .unwrap_or(DisplayMode::Hidden)
}

/// Extract task items from task-related tool input.
fn extract_task_items(tool_name: &str, tool_use_id: &str, input: &Value) -> Vec<TaskItem> {
    match tool_name {
        "TaskCreate" => {
            let subject = input
                .get("subject")
                .and_then(|v| v.as_str())
                .unwrap_or("(no subject)")
                .to_string();
            vec![TaskItem {
                id: tool_use_id.to_string(),
                subject,
                status: TaskStatus::Pending,
            }]
        }
        "TaskUpdate" => {
            let task_id = input
                .get("taskId")
                .and_then(|v| v.as_str())
                .unwrap_or(tool_use_id)
                .to_string();
            let status = match input.get("status").and_then(|v| v.as_str()) {
                Some("in_progress") => TaskStatus::InProgress,
                Some("completed") => TaskStatus::Completed,
                Some("cancelled") => TaskStatus::Cancelled,
                _ => TaskStatus::Pending,
            };
            let subject = input
                .get("subject")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            vec![TaskItem {
                id: task_id,
                subject,
                status,
            }]
        }
        _ => vec![],
    }
}

/// Flush grouped accumulator: single item unwrapped, multiple items wrapped in Group.
fn flush_grouped(acc: &mut Vec<DisplayItem>, output: &mut Vec<DisplayItem>) {
    if acc.is_empty() {
        return;
    }
    if acc.len() == 1 {
        output.push(acc.remove(0));
    } else {
        output.push(DisplayItem::Group {
            items: std::mem::take(acc),
        });
    }
}

/// Flush task accumulator: merge all task items into a single TaskList.
fn flush_tasks(acc: &mut Vec<(DisplayItem, Value)>, output: &mut Vec<DisplayItem>) {
    if acc.is_empty() {
        return;
    }

    let mut all_tasks = Vec::new();
    let mut last_raw = Value::Null;

    for (item, raw) in acc.drain(..) {
        last_raw = raw;
        if let DisplayItem::TaskList { tasks, .. } = item {
            all_tasks.extend(tasks);
        }
    }

    output.push(DisplayItem::TaskList {
        tasks: all_tasks,
        raw: last_raw,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::parse::parse_events;
    use serde_json::json;

    fn make_opts() -> DisplayOpts {
        DisplayOpts::default()
    }

    #[test]
    fn test_user_message_produces_full_item() {
        let raw = vec![json!({
            "type": "user",
            "cwd": "/tmp", "isSidechain": false, "sessionId": "s1",
            "timestamp": "2026-01-01T00:00:00Z", "userType": "external",
            "uuid": "u1", "version": "1",
            "message": {"role": "user", "content": "hello world"}
        })];
        let events = parse_events(&raw);
        let items = events_to_display_items(&events, &raw, &make_opts());
        assert_eq!(items.len(), 1);
        assert!(
            matches!(&items[0], DisplayItem::UserMessage { content, .. } if content == "hello world")
        );
    }

    #[test]
    fn test_assistant_text_and_thinking_grouped() {
        let raw = vec![json!({
            "type": "assistant",
            "cwd": "/tmp", "isSidechain": false, "sessionId": "s1",
            "timestamp": "2026-01-01T00:00:00Z", "userType": "external",
            "uuid": "u1", "version": "1",
            "message": {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "let me think..."},
                {"type": "text", "text": "here is my answer"}
            ]}
        })];
        let events = parse_events(&raw);
        let items = events_to_display_items(&events, &raw, &make_opts());
        // Thinking is Grouped (alone, so no Group wrapper), then text is Full
        assert_eq!(items.len(), 2);
        assert!(
            matches!(&items[0], DisplayItem::Thinking { text, .. } if text == "let me think...")
        );
        assert!(
            matches!(&items[1], DisplayItem::AssistantMessage { text, .. } if text == "here is my answer")
        );
    }

    #[test]
    fn test_consecutive_grouped_items_form_group() {
        let raw = vec![json!({
            "type": "assistant",
            "cwd": "/tmp", "isSidechain": false, "sessionId": "s1",
            "timestamp": "2026-01-01T00:00:00Z", "userType": "external",
            "uuid": "u1", "version": "1",
            "message": {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "thought 1"},
                {"type": "thinking", "thinking": "thought 2"},
                {"type": "tool_use", "name": "Read", "id": "t1", "input": {"path": "/tmp/foo"}},
                {"type": "text", "text": "done"}
            ]}
        })];
        let events = parse_events(&raw);
        let items = events_to_display_items(&events, &raw, &make_opts());
        // thinking1 + thinking2 + Read (Grouped) = one Group, then text = Full
        assert_eq!(items.len(), 2);
        assert!(matches!(&items[0], DisplayItem::Group { items } if items.len() == 3));
        assert!(matches!(&items[1], DisplayItem::AssistantMessage { .. }));
    }

    #[test]
    fn test_bash_tool_is_full_not_grouped() {
        let raw = vec![json!({
            "type": "assistant",
            "cwd": "/tmp", "isSidechain": false, "sessionId": "s1",
            "timestamp": "2026-01-01T00:00:00Z", "userType": "external",
            "uuid": "u1", "version": "1",
            "message": {"role": "assistant", "content": [
                {"type": "tool_use", "name": "Bash", "id": "t1", "input": {"command": "ls"}}
            ]}
        })];
        let events = parse_events(&raw);
        let items = events_to_display_items(&events, &raw, &make_opts());
        assert_eq!(items.len(), 1);
        assert!(matches!(&items[0], DisplayItem::ToolUse { name, .. } if name == "Bash"));
    }

    #[test]
    fn test_tool_use_paired_with_result() {
        let raw = vec![
            json!({
                "type": "assistant",
                "cwd": "/tmp", "isSidechain": false, "sessionId": "s1",
                "timestamp": "2026-01-01T00:00:00Z", "userType": "external",
                "uuid": "u1", "version": "1",
                "message": {"role": "assistant", "content": [
                    {"type": "tool_use", "name": "Bash", "id": "t1", "input": {"command": "echo hi"}}
                ]}
            }),
            json!({
                "type": "user",
                "cwd": "/tmp", "isSidechain": false, "sessionId": "s1",
                "timestamp": "2026-01-01T00:00:01Z", "userType": "external",
                "uuid": "u2", "version": "1",
                "message": {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "hi\n"}
                ]}
            }),
        ];
        let events = parse_events(&raw);
        let items = events_to_display_items(&events, &raw, &make_opts());
        assert_eq!(items.len(), 1);
        match &items[0] {
            DisplayItem::ToolUse { name, result, .. } => {
                assert_eq!(name, "Bash");
                assert!(result.is_some());
                assert_eq!(result.as_ref().unwrap().output.as_deref(), Some("hi\n"));
            }
            other => panic!("Expected ToolUse, got {:?}", other),
        }
    }

    #[test]
    fn test_turn_duration() {
        let raw = vec![json!({
            "type": "system",
            "cwd": "/tmp", "isSidechain": false, "sessionId": "s1",
            "timestamp": "2026-01-01T00:00:00Z", "userType": "external",
            "uuid": "u1", "version": "1",
            "subtype": "turn_duration",
            "durationMs": 1500
        })];
        let events = parse_events(&raw);
        let items = events_to_display_items(&events, &raw, &make_opts());
        assert_eq!(items.len(), 1);
        assert!(matches!(
            &items[0],
            DisplayItem::TurnDuration {
                duration_ms: 1500,
                ..
            }
        ));
    }

    #[test]
    fn test_progress_events_hidden() {
        let raw = vec![json!({
            "type": "progress",
            "cwd": "/tmp", "isSidechain": false, "sessionId": "s1",
            "timestamp": "2026-01-01T00:00:00Z", "userType": "external",
            "uuid": "u1", "version": "1"
        })];
        let events = parse_events(&raw);
        let items = events_to_display_items(&events, &raw, &make_opts());
        assert!(items.is_empty());
    }

    #[test]
    fn test_custom_display_opts() {
        let mut opts = DisplayOpts::default();
        opts.defaults
            .insert(DisplayItemDiscriminant::Thinking, DisplayMode::Full);
        let raw = vec![json!({
            "type": "assistant",
            "cwd": "/tmp", "isSidechain": false, "sessionId": "s1",
            "timestamp": "2026-01-01T00:00:00Z", "userType": "external",
            "uuid": "u1", "version": "1",
            "message": {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "thought 1"},
                {"type": "thinking", "thinking": "thought 2"}
            ]}
        })];
        let events = parse_events(&raw);
        let items = events_to_display_items(&events, &raw, &opts);
        assert_eq!(items.len(), 2);
        assert!(matches!(&items[0], DisplayItem::Thinking { .. }));
        assert!(matches!(&items[1], DisplayItem::Thinking { .. }));
    }
}
