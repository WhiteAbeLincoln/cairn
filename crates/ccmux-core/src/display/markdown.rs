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
        "Read" | "Write" => input
            .get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "Edit" => input
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
    let start = (page - 1) * per_page;
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
}
