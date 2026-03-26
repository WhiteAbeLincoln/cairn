//! Markdown rendering for session data. Produces plain-text markdown
//! views suitable for consumption by AI agents.

use serde_json::Value;

use super::{DisplayItem, DisplayItemWithMode, DisplayModeF};

/// Input data for rendering the session list markdown view.
pub struct SessionListGroup {
    pub project: String,
    pub sessions: Vec<SessionListEntry>,
}

pub struct SessionListEntry {
    pub id: String,
    pub first_message: Option<String>,
    pub updated_at: Option<String>,
}

/// Render a list of sessions grouped by project as markdown.
pub fn render_session_list(groups: &[SessionListGroup]) -> String {
    let mut out = String::from("# Sessions\n");

    for group in groups {
        out.push_str(&format!("\n## {}\n\n", group.project));
        for session in &group.sessions {
            let label = session.first_message.as_deref().unwrap_or("(no message)");
            let timestamp = session.updated_at.as_deref().unwrap_or("");
            if timestamp.is_empty() {
                out.push_str(&format!("- [{}](/session/{}.md)\n", label, session.id));
            } else {
                out.push_str(&format!(
                    "- [{}](/session/{}.md) — {}\n",
                    label, session.id, timestamp
                ));
            }
        }
    }

    out
}

/// Generate a short label for a display item, used in bullet lists.
pub fn bullet_label(item: &DisplayItem) -> String {
    match item {
        DisplayItem::ToolUse { name, input, .. } => {
            let summary = tool_summary(name, input);
            if summary.is_empty() {
                name.clone()
            } else {
                format!("{name}: {summary}")
            }
        }
        DisplayItem::Thinking { .. } => "Thinking".to_string(),
        DisplayItem::Compaction { .. } => "Compaction".to_string(),
        DisplayItem::UserMessage { .. } => "User".to_string(),
        DisplayItem::AssistantMessage { .. } => "Assistant".to_string(),
        DisplayItem::TurnDuration { .. } => "Turn Duration".to_string(),
        DisplayItem::Other { .. } => "Event".to_string(),
    }
}

/// Extract a short summary from tool input for the bullet label.
fn tool_summary(name: &str, input: &Value) -> String {
    match name {
        "Bash" => input
            .get("command")
            .and_then(|v| v.as_str())
            .map(|s| {
                let truncated = if s.len() > 60 {
                    format!("{}...", &s[..s.floor_char_boundary(60)])
                } else {
                    s.to_string()
                };
                format!("`{truncated}`")
            })
            .unwrap_or_default(),
        "Read" | "Write" | "Edit" => input
            .get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "Grep" => input
            .get("pattern")
            .and_then(|v| v.as_str())
            .map(|s| format!("`{s}`"))
            .unwrap_or_default(),
        "Glob" => input
            .get("pattern")
            .and_then(|v| v.as_str())
            .map(|s| format!("`{s}`"))
            .unwrap_or_default(),
        "Agent" => input
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    }
}

/// Render a paginated session view as markdown.
///
/// `items` is the full list of display items (already processed through the pipeline).
/// `page` is 1-indexed. `per_page` is items per page.
pub fn render_session_markdown(
    session_id: &str,
    items: &[DisplayItemWithMode],
    page: usize,
    per_page: usize,
) -> String {
    let mut out = format!("# Session {session_id}\n\n[sessions](/sessions.md)\n");

    if items.is_empty() {
        return out;
    }

    let total_items = items.len();
    let total_pages = total_items.div_ceil(per_page);
    let start = ((page - 1) * per_page).min(total_items);
    let end = (start + per_page).min(total_items);
    let page_items = &items[start..end];

    for item in page_items {
        render_display_item(&mut out, session_id, item);
    }

    // Pagination footer
    if total_pages > 1 {
        out.push_str("\n---\n");
        let mut footer = format!("Page {page} of {total_pages}");
        if page > 1 {
            footer = format!(
                "[← Prev](/session/{session_id}.md?page={}) | {footer}",
                page - 1
            );
        }
        if page < total_pages {
            footer = format!(
                "{footer} | [Next →](/session/{session_id}.md?page={})",
                page + 1
            );
        }
        out.push_str(&footer);
        out.push('\n');
    }

    out
}

fn render_display_item(out: &mut String, session_id: &str, item: &DisplayItemWithMode) {
    match item {
        DisplayModeF::Full(display_item) => {
            render_full_item(out, session_id, display_item);
        }
        DisplayModeF::Collapsed(display_item) => {
            render_bullet_item(out, session_id, display_item);
        }
        DisplayModeF::Grouped(items) => {
            for display_item in items {
                render_bullet_item(out, session_id, display_item);
            }
        }
        DisplayModeF::Hidden(_) => {}
    }
}

fn render_full_item(out: &mut String, session_id: &str, item: &DisplayItem) {
    let cursor = item.cursor().unwrap_or("0");
    let details_link = format!("[details](/session/{session_id}/event/{cursor}.md)");

    match item {
        DisplayItem::UserMessage { content, .. } => {
            out.push_str(&format!("\n## User\n{details_link}\n\n{content}\n"));
        }
        DisplayItem::AssistantMessage { text, .. } => {
            out.push_str(&format!("\n## Assistant\n{details_link}\n\n{text}\n"));
        }
        _ => {
            // Other Full items rendered as bullets (shouldn't happen with markdown DisplayOpts)
            render_bullet_item(out, session_id, item);
        }
    }
}

fn render_bullet_item(out: &mut String, session_id: &str, item: &DisplayItem) {
    let cursor = item.cursor().unwrap_or("0");
    let label = bullet_label(item);
    out.push_str(&format!(
        "- [{label}](/session/{session_id}/event/{cursor}.md)\n"
    ));
}

/// Input data for rendering search results.
pub struct SearchResultGroup {
    pub session_id: String,
    pub slug: Option<String>,
    pub project_path: Option<String>,
    pub created_at: Option<String>,
    pub items: Vec<DisplayItemWithMode>,
}

/// Render search results as markdown.
/// Delegates individual message rendering to the existing `render_full_item` format.
pub fn render_search_results(
    query: &str,
    groups: &[SearchResultGroup],
    total_matches: usize,
    total_sessions: usize,
) -> String {
    let mut out = format!(
        "# Search: \"{query}\"\n{total_matches} results across {total_sessions} session{}\n",
        if total_sessions == 1 { "" } else { "s" }
    );

    for group in groups {
        let label = group.slug.as_deref().unwrap_or(&group.session_id);
        let date = group.created_at.as_deref().unwrap_or("");
        if date.is_empty() {
            out.push_str(&format!("\n## {label}\n"));
        } else {
            out.push_str(&format!("\n## {label} ({date})\n"));
        }
        if let Some(ref project) = group.project_path {
            out.push_str(&format!("Project: {project}\n"));
        }
        out.push_str(&format!("Session: {}\n", group.session_id));

        for item in &group.items {
            render_display_item(&mut out, &group.session_id, item);
        }

        out.push_str("\n---\n");
    }

    out
}

/// Render a single raw JSONL event as a markdown detail view.
pub fn render_event_detail(raw: &Value, show_metadata: bool, session_id: &str) -> String {
    let mut out = String::new();

    // YAML front matter for metadata
    if show_metadata {
        out.push_str("---\n");
        if let Some(ts) = raw.get("timestamp").and_then(|v| v.as_str()) {
            out.push_str(&format!("timestamp: {ts}\n"));
        }
        if let Some(model) = raw.pointer("/message/model").and_then(|v| v.as_str()) {
            out.push_str(&format!("model: {model}\n"));
        }
        if let Some(tokens_in) = raw
            .pointer("/message/usage/input_tokens")
            .and_then(|v| v.as_u64())
        {
            out.push_str(&format!("tokens_in: {tokens_in}\n"));
        }
        if let Some(tokens_out) = raw
            .pointer("/message/usage/output_tokens")
            .and_then(|v| v.as_u64())
        {
            out.push_str(&format!("tokens_out: {tokens_out}\n"));
        }
        if let Some(uuid) = raw.get("uuid").and_then(|v| v.as_str()) {
            out.push_str(&format!("uuid: {uuid}\n"));
        }
        out.push_str("---\n\n");
    }

    // Back link
    out.push_str(&format!("[back to session](/session/{session_id}.md)\n"));

    let event_type = raw.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match event_type {
        "user" => render_user_event_detail(&mut out, raw),
        "assistant" => render_assistant_event_detail(&mut out, raw),
        "system" => render_system_event_detail(&mut out, raw),
        _ => {
            out.push_str("\n## Event\n\n```json\n");
            out.push_str(&serde_json::to_string_pretty(raw).unwrap_or_default());
            out.push_str("\n```\n");
        }
    }

    out
}

fn render_user_event_detail(out: &mut String, raw: &Value) {
    let content = raw.pointer("/message/content");
    match content {
        Some(Value::String(text)) => {
            out.push_str(&format!("\n## User Message\n\n{text}\n"));
        }
        Some(Value::Array(items)) => {
            for item in items {
                let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match item_type {
                    "tool_result" => {
                        let tool_id = item
                            .get("tool_use_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        out.push_str(&format!("\n## Tool Result ({tool_id})\n\n```json\n"));
                        out.push_str(&serde_json::to_string_pretty(item).unwrap_or_default());
                        out.push_str("\n```\n");
                    }
                    _ => {
                        out.push_str("\n## Content\n\n```json\n");
                        out.push_str(&serde_json::to_string_pretty(item).unwrap_or_default());
                        out.push_str("\n```\n");
                    }
                }
            }
        }
        _ => {
            out.push_str("\n## User Event\n\n```json\n");
            out.push_str(&serde_json::to_string_pretty(raw).unwrap_or_default());
            out.push_str("\n```\n");
        }
    }
}

fn render_assistant_event_detail(out: &mut String, raw: &Value) {
    let content = raw.pointer("/message/content").and_then(|v| v.as_array());
    let Some(items) = content else {
        out.push_str("\n## Assistant Event\n\n```json\n");
        out.push_str(&serde_json::to_string_pretty(raw).unwrap_or_default());
        out.push_str("\n```\n");
        return;
    };

    for item in items {
        let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match item_type {
            "text" => {
                let text = item.get("text").and_then(|v| v.as_str()).unwrap_or("");
                out.push_str(&format!("\n## Text\n\n{text}\n"));
            }
            "thinking" => {
                let text = item.get("thinking").and_then(|v| v.as_str()).unwrap_or("");
                out.push_str(&format!("\n## Thinking\n\n{text}\n"));
            }
            "tool_use" => {
                let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("Tool");
                let input = item.get("input").unwrap_or(&Value::Null);
                out.push_str(&format!("\n## {name}\n\n### Input\n\n```json\n"));
                out.push_str(&serde_json::to_string_pretty(input).unwrap_or_default());
                out.push_str("\n```\n");
            }
            _ => {
                out.push_str("\n## Content Block\n\n```json\n");
                out.push_str(&serde_json::to_string_pretty(item).unwrap_or_default());
                out.push_str("\n```\n");
            }
        }
    }
}

fn render_system_event_detail(out: &mut String, raw: &Value) {
    let subtype = raw.get("subtype").and_then(|v| v.as_str());
    match subtype {
        Some("turn_duration") => {
            let ms = raw.get("durationMs").and_then(|v| v.as_u64()).unwrap_or(0);
            let secs = ms as f64 / 1000.0;
            out.push_str(&format!("\n## Turn Duration\n\n{secs:.1}s ({ms}ms)\n"));
        }
        _ => {
            out.push_str("\n## System Event\n\n```json\n");
            out.push_str(&serde_json::to_string_pretty(raw).unwrap_or_default());
            out.push_str("\n```\n");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::ItemMeta;

    fn user_item(content: &str, cursor: &str) -> DisplayItemWithMode {
        DisplayModeF::Full(DisplayItem::UserMessage {
            content: content.to_string(),
            meta: ItemMeta::default(),
            raw: Value::Null,
            cursor: Some(cursor.to_string()),
        })
    }

    fn assistant_item(text: &str, cursor: &str) -> DisplayItemWithMode {
        DisplayModeF::Full(DisplayItem::AssistantMessage {
            text: text.to_string(),
            meta: ItemMeta::default(),
            raw: Value::Null,
            cursor: Some(cursor.to_string()),
        })
    }

    fn tool_item(name: &str, cursor: &str) -> DisplayItem {
        DisplayItem::ToolUse {
            name: name.to_string(),
            tool_use_id: "t1".to_string(),
            input: serde_json::json!({"command": "ls"}),
            result: None,
            meta: ItemMeta::default(),
            raw: Value::Null,
            cursor: Some(cursor.to_string()),
        }
    }

    #[test]
    fn test_render_session_basic() {
        let items = vec![user_item("hello", "0"), assistant_item("hi there", "64")];
        let result = render_session_markdown("sess1", &items, 1, 50);
        assert!(result.contains("# Session sess1"));
        assert!(result.contains("## User"));
        assert!(result.contains("[details](/session/sess1/event/0.md)"));
        assert!(result.contains("hello"));
        assert!(result.contains("## Assistant"));
        assert!(result.contains("hi there"));
    }

    fn read_item(cursor: &str) -> DisplayItem {
        DisplayItem::ToolUse {
            name: "Read".to_string(),
            tool_use_id: "t1".to_string(),
            input: serde_json::json!({"file_path": "/src/main.rs"}),
            result: None,
            meta: ItemMeta::default(),
            raw: Value::Null,
            cursor: Some(cursor.to_string()),
        }
    }

    #[test]
    fn test_render_session_with_grouped_tools() {
        let items = vec![
            user_item("hello", "0"),
            assistant_item("let me check", "64"),
            DisplayModeF::Grouped(vec![read_item("c8"), tool_item("Bash", "12c")]),
            assistant_item("done", "190"),
        ];
        let result = render_session_markdown("sess1", &items, 1, 50);
        assert!(result.contains("- [Read:"));
        assert!(result.contains("- [Bash:"));
    }

    #[test]
    fn test_render_session_pagination() {
        let items = vec![
            user_item("msg1", "0"),
            assistant_item("reply1", "64"),
            user_item("msg2", "c8"),
            assistant_item("reply2", "12c"),
        ];
        let result = render_session_markdown("sess1", &items, 1, 2);
        assert!(result.contains("msg1"));
        assert!(result.contains("reply1"));
        assert!(!result.contains("msg2"));
        assert!(result.contains("Page 1 of 2"));
        assert!(result.contains("[Next"));
    }

    #[test]
    fn test_render_session_empty() {
        let items: Vec<DisplayItemWithMode> = vec![];
        let result = render_session_markdown("sess1", &items, 1, 50);
        assert_eq!(result, "# Session sess1\n\n[sessions](/sessions.md)\n");
    }

    #[test]
    fn test_render_session_single_page_no_footer() {
        let items = vec![user_item("hello", "0")];
        let result = render_session_markdown("sess1", &items, 1, 50);
        assert!(!result.contains("Page"));
    }

    #[test]
    fn test_render_session_list_empty() {
        let result = render_session_list(&[]);
        assert_eq!(result, "# Sessions\n");
    }

    #[test]
    fn test_render_session_list_with_groups() {
        let groups = vec![SessionListGroup {
            project: "/Users/alice/myproject".to_string(),
            sessions: vec![SessionListEntry {
                id: "abc123".to_string(),
                first_message: Some("Fix auth bug".to_string()),
                updated_at: Some("2026-03-14 10:30".to_string()),
            }],
        }];
        let result = render_session_list(&groups);
        assert!(result.contains("## /Users/alice/myproject"));
        assert!(result.contains("[Fix auth bug](/session/abc123.md)"));
        assert!(result.contains("2026-03-14 10:30"));
    }

    #[test]
    fn test_bullet_label_bash() {
        let item = DisplayItem::ToolUse {
            name: "Bash".to_string(),
            tool_use_id: "t1".to_string(),
            input: serde_json::json!({"command": "ls src/"}),
            result: None,
            meta: Default::default(),
            raw: Value::Null,
            cursor: None,
        };
        assert_eq!(bullet_label(&item), "Bash: `ls src/`");
    }

    #[test]
    fn test_bullet_label_read() {
        let item = DisplayItem::ToolUse {
            name: "Read".to_string(),
            tool_use_id: "t1".to_string(),
            input: serde_json::json!({"file_path": "/src/main.rs"}),
            result: None,
            meta: Default::default(),
            raw: Value::Null,
            cursor: None,
        };
        assert_eq!(bullet_label(&item), "Read: /src/main.rs");
    }

    #[test]
    fn test_bullet_label_thinking() {
        let item = DisplayItem::Thinking {
            text: "let me think...".to_string(),
            meta: Default::default(),
            raw: Value::Null,
            cursor: None,
        };
        assert_eq!(bullet_label(&item), "Thinking");
    }

    #[test]
    fn test_bullet_label_grep() {
        let item = DisplayItem::ToolUse {
            name: "Grep".to_string(),
            tool_use_id: "t1".to_string(),
            input: serde_json::json!({"pattern": "fn main"}),
            result: None,
            meta: Default::default(),
            raw: Value::Null,
            cursor: None,
        };
        assert_eq!(bullet_label(&item), "Grep: `fn main`");
    }

    #[test]
    fn test_bullet_label_edit() {
        let item = DisplayItem::ToolUse {
            name: "Edit".to_string(),
            tool_use_id: "t1".to_string(),
            input: serde_json::json!({"file_path": "/src/lib.rs", "old_string": "foo", "new_string": "bar"}),
            result: None,
            meta: Default::default(),
            raw: Value::Null,
            cursor: None,
        };
        assert_eq!(bullet_label(&item), "Edit: /src/lib.rs");
    }

    #[test]
    fn test_bullet_label_compaction() {
        let item = DisplayItem::Compaction {
            content: "summary".to_string(),
            meta: Default::default(),
            raw: Value::Null,
            cursor: None,
        };
        assert_eq!(bullet_label(&item), "Compaction");
    }

    #[test]
    fn test_render_event_detail_tool_use() {
        let raw = serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-03-14T10:30:00Z",
            "uuid": "abc-123",
            "message": {
                "model": "claude-opus-4-6",
                "usage": {"input_tokens": 100, "output_tokens": 50},
                "content": [
                    {
                        "type": "tool_use",
                        "name": "Bash",
                        "id": "t1",
                        "input": {"command": "ls src/"}
                    }
                ]
            }
        });
        let result = render_event_detail(&raw, false, "sess1");
        assert!(result.contains("[back to session](/session/sess1.md)"));
        assert!(result.contains("## Bash"));
        assert!(result.contains("### Input"));
        assert!(result.contains("```json"));
        assert!(!result.contains("---\ntimestamp:"));
    }

    #[test]
    fn test_render_event_detail_with_metadata() {
        let raw = serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-03-14T10:30:00Z",
            "uuid": "abc-123",
            "message": {
                "model": "claude-opus-4-6",
                "usage": {"input_tokens": 100, "output_tokens": 50},
                "content": [
                    {"type": "text", "text": "hello world"}
                ]
            }
        });
        let result = render_event_detail(&raw, true, "sess1");
        assert!(result.contains("---\n"));
        assert!(result.contains("timestamp: 2026-03-14T10:30:00Z"));
        assert!(result.contains("model: claude-opus-4-6"));
        assert!(result.contains("tokens_in: 100"));
        assert!(result.contains("tokens_out: 50"));
        assert!(result.contains("uuid: abc-123"));
    }

    #[test]
    fn test_render_event_detail_user_message() {
        let raw = serde_json::json!({
            "type": "user",
            "timestamp": "2026-03-14T10:30:00Z",
            "uuid": "u1",
            "message": {"role": "user", "content": "What does this do?"}
        });
        let result = render_event_detail(&raw, false, "sess1");
        assert!(result.contains("## User Message"));
        assert!(result.contains("What does this do?"));
    }

    #[test]
    fn test_render_search_results_basic() {
        let results = vec![SearchResultGroup {
            session_id: "abc123".to_string(),
            slug: Some("fix-auth-bug".to_string()),
            project_path: Some("/Users/test/myproject".to_string()),
            created_at: Some("2026-03-20".to_string()),
            items: vec![
                user_item("How do I fix the authentication bug?", "0"),
                assistant_item("I'll help you fix the auth middleware.", "3a"),
            ],
        }];

        let output = render_search_results("authentication", &results, 2, 1);
        assert!(output.contains("# Search: \"authentication\""));
        assert!(output.contains("2 results across 1 session"));
        assert!(output.contains("## fix-auth-bug (2026-03-20)"));
        assert!(output.contains("Project: /Users/test/myproject"));
        assert!(output.contains("## User"));
        assert!(output.contains("authentication bug"));
        assert!(output.contains("## Assistant"));
    }

    #[test]
    fn test_render_search_results_empty() {
        let output = render_search_results("nothing", &[], 0, 0);
        assert!(output.contains("# Search: \"nothing\""));
        assert!(output.contains("0 results"));
    }

    #[test]
    fn test_render_event_detail_thinking() {
        let raw = serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-03-14T10:30:00Z",
            "uuid": "u1",
            "message": {
                "content": [
                    {"type": "thinking", "thinking": "Let me consider..."},
                    {"type": "text", "text": "Here is my answer"}
                ]
            }
        });
        let result = render_event_detail(&raw, false, "sess1");
        assert!(result.contains("## Thinking"));
        assert!(result.contains("Let me consider..."));
        assert!(result.contains("## Text"));
        assert!(result.contains("Here is my answer"));
    }
}
