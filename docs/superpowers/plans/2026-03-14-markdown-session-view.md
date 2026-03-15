# Markdown Session View Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add plain-text markdown API endpoints for AI agents to explore Claude Code session logs.

**Architecture:** Custom Axum routes registered alongside the Dioxus app via `dioxus_server::serve()`. Session loading and display pipeline in ccmux-core gain byte offset tracking and a new markdown renderer module. Three endpoints: session list, paginated session view, and event detail.

**Tech Stack:** Rust, Axum, Dioxus 0.7.3 (server), hex-encoded byte offsets for opaque cursors, serde_json for raw event access.

**Note:** The spec says "base64url-encoded" cursors but we use hex encoding (`format!("{offset:x}")`) to avoid adding a base64 dependency to ccmux-core (which also targets WASM). The cursor remains opaque to consumers. Update the spec to reflect this.

**Spec:** `docs/superpowers/specs/2026-03-14-markdown-session-view-design.md`

---

## Chunk 1: Core Data Layer — Byte Offsets and Cursors

### Task 1: Add `load_session_raw_with_offsets` to loader

**Files:**
- Modify: `crates/ccmux-core/src/session/loader.rs`

- [ ] **Step 1: Write the failing test**

Add to the existing `mod tests` block in `loader.rs`:

```rust
#[test]
fn test_load_session_raw_with_offsets() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.jsonl");
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, r#"{{"type":"user","msg":"hello"}}"#).unwrap();
    writeln!(f, r#"{{"type":"assistant","msg":"hi"}}"#).unwrap();

    let events = load_session_raw_with_offsets(&path).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].0, 0); // first line starts at byte 0
    assert!(events[1].0 > 0); // second line starts after first
    assert_eq!(events[0].1["type"], "user");
    assert_eq!(events[1].1["type"], "assistant");
}

#[test]
fn test_load_session_raw_with_offsets_skips_empty_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.jsonl");
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, r#"{{"type":"user","msg":"hello"}}"#).unwrap();
    writeln!(f).unwrap(); // empty line
    writeln!(f, r#"{{"type":"assistant","msg":"hi"}}"#).unwrap();

    let events = load_session_raw_with_offsets(&path).unwrap();
    assert_eq!(events.len(), 2);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ccmux-core -- test_load_session_raw_with_offsets`
Expected: FAIL with "cannot find function `load_session_raw_with_offsets`"

- [ ] **Step 3: Implement `load_session_raw_with_offsets`**

Add to `crates/ccmux-core/src/session/loader.rs`, near `load_session_raw` (line 336):

```rust
/// Load all events from a session JSONL file as raw JSON values, paired with byte offsets.
/// Each tuple contains (byte_offset, json_value) where byte_offset is the position
/// of the line's first byte in the file.
pub fn load_session_raw_with_offsets(
    path: &Path,
) -> std::io::Result<Vec<(u64, serde_json::Value)>> {
    use std::io::BufRead;
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);

    let mut events = Vec::new();
    let mut offset: u64 = 0;
    let mut line = String::new();
    let mut line_num = 0;

    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line)?;
        if bytes_read == 0 {
            break;
        }
        line_num += 1;
        let line_offset = offset;
        offset += bytes_read as u64;

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(value) => events.push((line_offset, value)),
            Err(e) => {
                tracing::warn!(line = line_num, error = %e, "Failed to parse JSONL line");
            }
        }
    }
    Ok(events)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ccmux-core -- test_load_session_raw_with_offsets`
Expected: PASS

- [ ] **Step 5: Run clippy and format**

Run: `cargo clippy --workspace && cargo fmt --all`

- [ ] **Step 6: Commit**

```bash
git add crates/ccmux-core/src/session/loader.rs
git commit -m "feat: add load_session_raw_with_offsets for byte-offset tracking"
```

### Task 2: Add `cursor` field to DisplayItem

**Files:**
- Modify: `crates/ccmux-core/src/display/mod.rs`
- Modify: `crates/ccmux-core/src/display/pipeline.rs` (add `cursor: None` to all construction sites)
- Modify: `crates/ccmux-app/src/components/blocks/display_item.rs` (add `..` to pattern matches)
- Modify: `crates/ccmux-app/src/components/blocks/group.rs` (add `..` to pattern matches)
- Modify: `crates/ccmux-app/src/components/session_view.rs` (add `..` to pattern matches)

- [ ] **Step 1: Add `cursor` field to every DisplayItem variant**

In `crates/ccmux-core/src/display/mod.rs`, add `#[serde(default, skip_serializing_if = "Option::is_none")] cursor: Option<String>` to each variant of `DisplayItem` (lines 26-67). The field goes after the existing fields in each variant:

```rust
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
```

- [ ] **Step 2: Add a helper method to set cursor on DisplayItem**

Add an `impl DisplayItem` block below the enum definition:

```rust
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
```

- [ ] **Step 3: Fix all existing code that constructs DisplayItem variants**

Update `crates/ccmux-core/src/display/pipeline.rs` — every place a `DisplayItem` variant is constructed needs `cursor: None` added. There are 12 construction sites across `user_event_items`, `assistant_event_items`, and `system_event_items`. Add `cursor: None,` to each. For example:

```rust
DisplayItem::UserMessage {
    content: text.to_string(),
    meta: ItemMeta { uuid, ..Default::default() },
    raw: raw.clone(),
    cursor: None,
}
```

- [ ] **Step 4: Fix all code that pattern-matches on DisplayItem variants**

Multiple files destructure `DisplayItem` variants without `..` and will fail to compile:

**`crates/ccmux-app/src/components/blocks/display_item.rs`:**
- Line 47: `DisplayItem::UserMessage { content, meta, raw }` → add `..` or `cursor: _`
- Line 58: `DisplayItem::AssistantMessage { text, meta, raw }` → same
- Line 69: `DisplayItem::Thinking { text, meta, raw }` → same
- Line 117: `DisplayItem::Compaction { content, meta, raw }` → same
- Line 128: `DisplayItem::Other { raw }` → same
- (Line 41 and 80 already use `..`, so they're fine)

**`crates/ccmux-app/src/components/blocks/group.rs`:**
- Check all pattern matches on DisplayItem and add `..` where needed.

**`crates/ccmux-app/src/components/session_view.rs`:**
- Check all pattern matches on DisplayItem and add `..` where needed.

The simplest fix: add `..` to every match arm that doesn't already have it.

- [ ] **Step 5: Run all tests to verify nothing is broken**

Run: `cargo test --workspace`
Expected: All tests PASS

- [ ] **Step 6: Run clippy and format**

Run: `cargo clippy --workspace && cargo fmt --all`

- [ ] **Step 7: Commit**

```bash
git add crates/ccmux-core/src/display/mod.rs crates/ccmux-core/src/display/pipeline.rs
git commit -m "feat: add cursor field to DisplayItem for byte-offset tracking"
```

### Task 3: Thread byte offsets through the display pipeline

**Files:**
- Modify: `crates/ccmux-core/src/display/pipeline.rs`
- Modify: `crates/ccmux-core/src/display/mod.rs` (add cursor encoding helper)

- [ ] **Step 1: Add cursor encoding helper to mod.rs**

Add to `crates/ccmux-core/src/display/mod.rs`:

```rust
/// Encode a byte offset as an opaque cursor string (hex-encoded).
pub fn encode_cursor(offset: u64) -> String {
    format!("{offset:x}")
}

/// Decode an opaque cursor string back to a byte offset.
pub fn decode_cursor(cursor: &str) -> Option<u64> {
    u64::from_str_radix(cursor, 16).ok()
}
```

- [ ] **Step 2: Write tests for cursor encoding/decoding**

Add to `mod.rs` (create a `#[cfg(test)] mod tests` block):

```rust
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
}
```

- [ ] **Step 3: Run cursor tests**

Run: `cargo test -p ccmux-core -- test_cursor`
Expected: PASS

- [ ] **Step 4: Add `events_to_display_items_with_offsets` function**

Add to `crates/ccmux-core/src/display/pipeline.rs`. First, add `encode_cursor` to the imports from `super::`:

```rust
use super::{
    DisplayItem, DisplayItemDiscriminant, DisplayItemWithMode, DisplayMode, DisplayModeF,
    DisplayOpts, ItemMeta, TaskItem, TaskStatus, ToolResultData, encode_cursor,
};
```

Then add the function:

```rust
/// Convert events + raw JSON + byte offsets into display items with cursors.
/// Like `events_to_display_items` but tags each item with its source line's cursor.
pub fn events_to_display_items_with_offsets(
    events: &[Event],
    raw_events_with_offsets: &[(u64, Value)],
    opts: &DisplayOpts,
) -> Vec<DisplayItemWithMode> {
    let raw_events: Vec<&Value> = raw_events_with_offsets.iter().map(|(_, v)| v).collect();
    let tool_results = pre_scan_tool_results_refs(&raw_events);

    let mut output: Vec<DisplayItemWithMode> = Vec::new();
    let mut grouped_acc: Vec<DisplayItem> = Vec::new();

    for (event, (offset, raw)) in events.iter().zip(raw_events_with_offsets.iter()) {
        let intermediates = event_to_intermediates(event, raw, opts, &tool_results);
        let cursor = encode_cursor(*offset);

        for (mut item, mode) in intermediates {
            item.set_cursor(cursor.clone());
            match mode {
                DisplayModeF::Grouped(()) => {
                    grouped_acc.push(item);
                }
                DisplayModeF::Hidden(()) => {
                    // skip
                }
                DisplayModeF::Full(()) => {
                    flush_grouped(&mut grouped_acc, &mut output);
                    output.push(DisplayModeF::Full(item));
                }
                DisplayModeF::Collapsed(()) => {
                    flush_grouped(&mut grouped_acc, &mut output);
                    output.push(DisplayModeF::Collapsed(item));
                }
            }
        }
    }

    flush_grouped(&mut grouped_acc, &mut output);
    output
}
```

Also add a helper to pre-scan from references:

```rust
fn pre_scan_tool_results_refs(raw_events: &[&Value]) -> HashMap<String, ToolResultData> {
    raw_events
        .iter()
        .flat_map(|v| extract_tool_results_from_event(v))
        .collect()
}
```

- [ ] **Step 5: Write test for offset pipeline**

```rust
#[test]
fn test_events_to_display_items_with_offsets() {
    let raw_with_offsets = vec![
        (0u64, json!({
            "type": "user",
            "cwd": "/tmp", "isSidechain": false, "sessionId": "s1",
            "timestamp": "2026-01-01T00:00:00Z", "userType": "external",
            "uuid": "u1", "version": "1",
            "message": {"role": "user", "content": "hello world"}
        })),
        (100u64, json!({
            "type": "assistant",
            "cwd": "/tmp", "isSidechain": false, "sessionId": "s1",
            "timestamp": "2026-01-01T00:00:01Z", "userType": "external",
            "uuid": "u2", "version": "1",
            "message": {"role": "assistant", "content": [
                {"type": "text", "text": "hi there"}
            ]}
        })),
    ];
    let raw_values: Vec<Value> = raw_with_offsets.iter().map(|(_, v)| v.clone()).collect();
    let events = parse_events(&raw_values);
    let items = events_to_display_items_with_offsets(&events, &raw_with_offsets, &make_opts());

    assert_eq!(items.len(), 2);
    // Check cursors are set
    match &items[0] {
        DisplayModeF::Full(item) => {
            assert_eq!(item.cursor(), Some(encode_cursor(0).as_str()));
        }
        other => panic!("Expected Full, got {:?}", other),
    }
    match &items[1] {
        DisplayModeF::Full(item) => {
            assert_eq!(item.cursor(), Some(encode_cursor(100).as_str()));
        }
        other => panic!("Expected Full, got {:?}", other),
    }
}
```

- [ ] **Step 6: Run tests**

Run: `cargo test -p ccmux-core`
Expected: All PASS

- [ ] **Step 7: Run clippy and format**

Run: `cargo clippy --workspace && cargo fmt --all`

- [ ] **Step 8: Commit**

```bash
git add crates/ccmux-core/src/display/mod.rs crates/ccmux-core/src/display/pipeline.rs
git commit -m "feat: thread byte offsets through display pipeline as cursors"
```

---

## Chunk 2: Markdown Rendering Module

### Task 4: Create markdown rendering module with session list

**Files:**
- Create: `crates/ccmux-core/src/display/markdown.rs`
- Modify: `crates/ccmux-core/src/display/mod.rs` (add `pub mod markdown;`)

- [ ] **Step 1: Add module declaration**

Add to `crates/ccmux-core/src/display/mod.rs` line 4 (after `pub mod streaming;`):

```rust
pub mod markdown;
```

- [ ] **Step 2: Write test for session list rendering**

Create `crates/ccmux-core/src/display/markdown.rs`:

```rust
//! Markdown rendering for session data. Produces plain-text markdown
//! views suitable for consumption by AI agents.

use serde_json::Value;

use super::{DisplayItem, DisplayItemWithMode, DisplayModeF, encode_cursor, decode_cursor};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_session_list_empty() {
        let result = render_session_list(&[]);
        assert_eq!(result, "# Sessions\n");
    }

    #[test]
    fn test_render_session_list_with_groups() {
        let groups = vec![
            SessionListGroup {
                project: "/Users/alice/myproject".to_string(),
                sessions: vec![
                    SessionListEntry {
                        id: "abc123".to_string(),
                        first_message: Some("Fix auth bug".to_string()),
                        updated_at: Some("2026-03-14 10:30".to_string()),
                    },
                ],
            },
        ];
        let result = render_session_list(&groups);
        assert!(result.contains("## /Users/alice/myproject"));
        assert!(result.contains("[Fix auth bug](/session/abc123.md)"));
        assert!(result.contains("2026-03-14 10:30"));
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p ccmux-core -- test_render_session_list`
Expected: FAIL

- [ ] **Step 4: Implement session list types and renderer**

Add to `crates/ccmux-core/src/display/markdown.rs`:

```rust
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
            let label = session
                .first_message
                .as_deref()
                .unwrap_or("(no message)");
            let timestamp = session
                .updated_at
                .as_deref()
                .unwrap_or("");
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
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p ccmux-core -- test_render_session_list`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/ccmux-core/src/display/markdown.rs crates/ccmux-core/src/display/mod.rs
git commit -m "feat: add markdown module with session list renderer"
```

### Task 5: Add bullet label generation for tool items

**Files:**
- Modify: `crates/ccmux-core/src/display/markdown.rs`

- [ ] **Step 1: Write tests for bullet labels**

Add to the `tests` module in `markdown.rs`:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ccmux-core -- test_bullet_label`
Expected: FAIL

- [ ] **Step 3: Implement bullet_label**

Add to `markdown.rs`:

```rust
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
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p ccmux-core -- test_bullet_label`
Expected: All PASS

- [ ] **Step 5: Commit**

```bash
git add crates/ccmux-core/src/display/markdown.rs
git commit -m "feat: add bullet label generation for markdown tool summaries"
```

### Task 6: Add paginated session markdown renderer

**Files:**
- Modify: `crates/ccmux-core/src/display/markdown.rs`

- [ ] **Step 1: Write tests for session markdown rendering**

Add to `tests` module:

```rust
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
    let items = vec![
        user_item("hello", "0"),
        assistant_item("hi there", "64"),
    ];
    let result = render_session_markdown("sess1", &items, 1, 50);
    assert!(result.contains("# Session sess1"));
    assert!(result.contains("## User"));
    assert!(result.contains("[details](/session/sess1/event/0.md)"));
    assert!(result.contains("hello"));
    assert!(result.contains("## Assistant"));
    assert!(result.contains("hi there"));
}

#[test]
fn test_render_session_with_grouped_tools() {
    let items = vec![
        user_item("hello", "0"),
        assistant_item("let me check", "64"),
        DisplayModeF::Grouped(vec![
            tool_item("Read", "c8"),
            tool_item("Bash", "12c"),
        ]),
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ccmux-core -- test_render_session`
Expected: FAIL

- [ ] **Step 3: Implement `render_session_markdown`**

Add to `markdown.rs`:

```rust
/// Pagination result for the session markdown view.
pub struct PaginationInfo {
    pub page: usize,
    pub total_pages: usize,
    pub total_items: usize,
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
    let total_pages = (total_items + per_page - 1) / per_page;
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
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p ccmux-core -- test_render_session`
Expected: All PASS

- [ ] **Step 5: Commit**

```bash
git add crates/ccmux-core/src/display/markdown.rs
git commit -m "feat: add paginated session markdown renderer"
```

### Task 7: Add event detail markdown renderer

**Files:**
- Modify: `crates/ccmux-core/src/display/markdown.rs`

- [ ] **Step 1: Write tests for event detail rendering**

Add to `tests` module:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ccmux-core -- test_render_event_detail`
Expected: FAIL

- [ ] **Step 3: Implement `render_event_detail`**

Add to `markdown.rs`:

```rust
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
        if let Some(tokens_in) = raw.pointer("/message/usage/input_tokens").and_then(|v| v.as_u64())
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
                let name = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Tool");
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
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p ccmux-core -- test_render_event_detail`
Expected: All PASS

- [ ] **Step 5: Run clippy and format**

Run: `cargo clippy --workspace && cargo fmt --all`

- [ ] **Step 6: Commit**

```bash
git add crates/ccmux-core/src/display/markdown.rs
git commit -m "feat: add event detail markdown renderer"
```

### Task 8: Add markdown-specific DisplayOpts

**Files:**
- Modify: `crates/ccmux-core/src/display/mod.rs`

- [ ] **Step 1: Write test**

Add to `mod tests` in `mod.rs`:

```rust
#[test]
fn test_markdown_display_opts() {
    let opts = DisplayOpts::markdown();
    assert_eq!(
        opts.defaults.get(&DisplayItemDiscriminant::UserMessage),
        Some(&DisplayModeF::Full(()))
    );
    assert_eq!(
        opts.defaults.get(&DisplayItemDiscriminant::AssistantMessage),
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
    // No tool overrides — all tools are grouped
    assert!(opts.tool_overrides.is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ccmux-core -- test_markdown_display_opts`
Expected: FAIL

- [ ] **Step 3: Implement `DisplayOpts::markdown()`**

Add to `impl DisplayOpts` (or add an `impl` block if there isn't one — currently there's only `impl Default`):

```rust
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
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p ccmux-core -- test_markdown_display_opts`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/ccmux-core/src/display/mod.rs
git commit -m "feat: add markdown-specific DisplayOpts"
```

---

## Chunk 3: Axum API Handlers

### Task 9: Add axum dependency and create api module

**Files:**
- Modify: `crates/ccmux-app/Cargo.toml`
- Create: `crates/ccmux-app/src/api.rs`
- Modify: `crates/ccmux-app/src/main.rs`

- [ ] **Step 1: Add axum dependency**

Add to `crates/ccmux-app/Cargo.toml` under `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]` (after the existing server-only deps):

```toml
axum = "0.8"
```

Note: Check what version of axum Dioxus 0.7.3 uses and match it. Run `cargo tree -p dioxus-server -i axum` to find the version.

- [ ] **Step 2: Create api.rs module**

Create `crates/ccmux-app/src/api.rs`:

```rust
//! Markdown API endpoints for session exploration by AI agents.
//!
//! Provides three endpoints:
//! - GET /sessions.md — session list
//! - GET /session/:id.md — paginated session markdown view
//! - GET /session/:id/event/:cursor.md — event detail view

#[cfg(not(target_arch = "wasm32"))]
mod handlers {
    use axum::{
        extract::{Path, Query},
        http::{header, StatusCode},
        response::{IntoResponse, Response},
        routing::get,
        Router,
    };
    use std::collections::HashMap;

    use ccmux_core::display::markdown::{
        render_event_detail, render_session_list, render_session_markdown, SessionListEntry,
        SessionListGroup,
    };
    use ccmux_core::display::{decode_cursor, DisplayOpts};
    use ccmux_core::display::pipeline::events_to_display_items_with_offsets;
    use ccmux_core::events::parse::parse_events;
    use ccmux_core::session::loader;

    // Note: this duplicates base_path() in server_fns.rs. Consider extracting
    // to a shared module if more server-only code accumulates.
    fn base_path() -> std::path::PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        std::path::PathBuf::from(home)
            .join(".claude")
            .join("projects")
    }

    fn markdown_response(body: String) -> Response {
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
            body,
        )
            .into_response()
    }

    fn error_response(status: StatusCode, msg: &str) -> Response {
        (
            status,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            msg.to_string(),
        )
            .into_response()
    }

    async fn session_list_handler() -> Response {
        let base = base_path();
        let sessions = match loader::discover_sessions(&base) {
            Ok(s) => s,
            Err(e) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &format!("Failed to discover sessions: {e}")),
        };

        let mut groups_map: Vec<SessionListGroup> = Vec::new();
        for session in sessions.iter().filter(|s| !s.is_sidechain && s.first_message.is_some()) {
            let updated_str = session.updated_at.map(|dt| dt.format("%Y-%m-%d %H:%M").to_string());
            let entry = SessionListEntry {
                id: session.id.clone(),
                first_message: session.first_message.clone(),
                updated_at: updated_str,
            };
            if let Some(group) = groups_map.iter_mut().find(|g| g.project == session.project) {
                group.sessions.push(entry);
            } else {
                groups_map.push(SessionListGroup {
                    project: session.project.clone(),
                    sessions: vec![entry],
                });
            }
        }

        markdown_response(render_session_list(&groups_map))
    }

    async fn session_markdown_handler(
        Path(id_with_ext): Path<String>,
        Query(params): Query<HashMap<String, String>>,
    ) -> Response {
        let session_id = id_with_ext.strip_suffix(".md").unwrap_or(&id_with_ext);

        let page: usize = params
            .get("page")
            .and_then(|v| v.parse().ok())
            .unwrap_or(1);
        let per_page: usize = params
            .get("per_page")
            .and_then(|v| v.parse().ok())
            .unwrap_or(50);

        if page == 0 {
            return error_response(StatusCode::BAD_REQUEST, "Invalid parameter: page must be >= 1");
        }
        if per_page == 0 {
            return error_response(StatusCode::BAD_REQUEST, "Invalid parameter: per_page must be >= 1");
        }

        let base = base_path();
        let sessions = match loader::discover_sessions(&base) {
            Ok(s) => s,
            Err(e) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &format!("Failed to discover sessions: {e}")),
        };

        let info = match sessions.iter().find(|s| s.id == session_id) {
            Some(info) => info,
            None => return error_response(StatusCode::NOT_FOUND, &format!("Session not found: {session_id}")),
        };

        let raw_with_offsets = match loader::load_session_raw_with_offsets(&info.path) {
            Ok(r) => r,
            Err(e) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &format!("Failed to load session: {e}")),
        };

        let raw_values: Vec<serde_json::Value> = raw_with_offsets.iter().map(|(_, v)| v.clone()).collect();
        let events = parse_events(&raw_values);
        let opts = DisplayOpts::markdown();
        let items = events_to_display_items_with_offsets(&events, &raw_with_offsets, &opts);

        let total_items = items.len();
        let total_pages = if total_items == 0 { 1 } else { (total_items + per_page - 1) / per_page };
        if page > total_pages {
            return error_response(
                StatusCode::NOT_FOUND,
                &format!("Page {page} not found. Session has {total_pages} pages."),
            );
        }

        markdown_response(render_session_markdown(session_id, &items, page, per_page))
    }

    async fn event_detail_handler(
        Path((id, cursor_with_ext)): Path<(String, String)>,
        Query(params): Query<HashMap<String, String>>,
    ) -> Response {
        let cursor = cursor_with_ext.strip_suffix(".md").unwrap_or(&cursor_with_ext);

        let show_metadata = params
            .get("metadata")
            .map(|v| v == "true")
            .unwrap_or(false);

        let offset = match decode_cursor(cursor) {
            Some(o) => o,
            None => return error_response(StatusCode::BAD_REQUEST, "Invalid cursor"),
        };

        let base = base_path();
        let sessions = match loader::discover_sessions(&base) {
            Ok(s) => s,
            Err(e) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &format!("Failed to discover sessions: {e}")),
        };

        let info = match sessions.iter().find(|s| s.id == id) {
            Some(info) => info,
            None => return error_response(StatusCode::NOT_FOUND, &format!("Session not found: {id}")),
        };

        // Seek to offset and read one line
        use std::io::{BufRead, Seek, SeekFrom};
        let file = match std::fs::File::open(&info.path) {
            Ok(f) => f,
            Err(e) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &format!("Failed to open session file: {e}")),
        };

        let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);
        if offset >= file_len {
            return error_response(StatusCode::BAD_REQUEST, "Invalid cursor: offset out of range");
        }

        let mut reader = std::io::BufReader::new(file);
        if reader.seek(SeekFrom::Start(offset)).is_err() {
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to seek in session file");
        }

        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
            return error_response(StatusCode::BAD_REQUEST, "Invalid cursor: no data at offset");
        }

        let raw: serde_json::Value = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(_) => return error_response(StatusCode::BAD_REQUEST, "Invalid cursor: corrupt data at offset"),
        };

        markdown_response(render_event_detail(&raw, show_metadata, &id))
    }

    /// Build the Axum router for markdown API endpoints.
    pub fn build_api_router() -> Router {
        Router::new()
            .route("/sessions.md", get(session_list_handler))
            .route("/session/{id_with_ext}", get(session_markdown_handler))
            .route(
                "/session/{id}/event/{cursor_with_ext}",
                get(event_detail_handler),
            )
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use handlers::build_api_router;
```

- [ ] **Step 3: Add api module to main.rs and wire up with `dioxus_server::serve()`**

Update `crates/ccmux-app/src/main.rs`:

```rust
mod components;
mod routes;
mod server_fns;

#[cfg(not(target_arch = "wasm32"))]
mod api;

use dioxus::prelude::*;
use routes::Route;

fn main() {
    #[cfg(not(target_arch = "wasm32"))]
    {
        use dioxus_server::{DioxusRouterExt, ServeConfig};

        dioxus_server::serve(|| async {
            let api_routes = api::build_api_router();
            let dioxus_router = axum::Router::new()
                .serve_dioxus_application(ServeConfig::new(), App);
            Ok(api_routes.merge(dioxus_router))
        });
    }

    #[cfg(target_arch = "wasm32")]
    dioxus::launch(App);
}

static MAIN_CSS: Asset = asset!("/assets/style.scss");

#[component]
fn App() -> Element {
    rsx! {
        document::Stylesheet { href: MAIN_CSS }
        Router::<Route> {}
    }
}
```

Note: `dioxus_server` is the crate name (re-exported by dioxus). `DioxusRouterExt` and `ServeConfig` are exported at the crate root. The `serve()` function takes `FnMut() -> Future<Output = Result<Router, anyhow::Error>>` and never returns (`-> !`). The `#[cfg(not(target_arch = "wasm32"))]` block will always diverge via `serve()`, so the WASM block only runs on the WASM target.

**Important:** Verify that `dioxus_server` is accessible as a crate name. It should be, since `dioxus` depends on `dioxus-server` and re-exports it. If not, add `dioxus-server = "0.7"` as a direct dependency in `Cargo.toml` under server-only deps.

- [ ] **Step 4: Verify it compiles**

Run: `cargo clippy --workspace && cargo fmt --all`
Expected: No errors

- [ ] **Step 5: Commit**

```bash
git add crates/ccmux-app/Cargo.toml crates/ccmux-app/src/api.rs crates/ccmux-app/src/main.rs
git commit -m "feat: add markdown API endpoints with Axum routing"
```

---

## Chunk 4: Integration Testing

### Task 10: Manual integration test

**Files:** None (testing only)

- [ ] **Step 1: Start the dev server**

Run: `cd crates/ccmux-app && dx serve`

- [ ] **Step 2: Test sessions list endpoint**

Run: `curl http://localhost:8080/sessions.md`
Expected: Markdown listing of sessions grouped by project

- [ ] **Step 3: Test session markdown view**

Pick a session ID from the list output and test:
Run: `curl "http://localhost:8080/session/<session-id>.md"`
Expected: Paginated markdown with ## User / ## Assistant headers, tool bullets, and details links

- [ ] **Step 4: Test pagination**

Run: `curl "http://localhost:8080/session/<session-id>.md?page=1&per_page=5"`
Expected: Only 5 items, pagination footer with "Next" link

- [ ] **Step 5: Test event detail**

Pick a cursor from a details link in the session output and test:
Run: `curl "http://localhost:8080/session/<session-id>/event/<cursor>.md"`
Expected: Markdown detail of the event

- [ ] **Step 6: Test event detail with metadata**

Run: `curl "http://localhost:8080/session/<session-id>/event/<cursor>.md?metadata=true"`
Expected: YAML front matter with timestamp, model, tokens, uuid

- [ ] **Step 7: Test error cases**

Run: `curl "http://localhost:8080/session/nonexistent.md"` → 404
Run: `curl "http://localhost:8080/session/<id>/event/invalid!.md"` → 400
Run: `curl "http://localhost:8080/session/<id>.md?page=999"` → 404

- [ ] **Step 8: Verify the web UI still works**

Open `http://localhost:8080` in a browser.
Expected: Normal web UI loads and functions correctly.

- [ ] **Step 9: Commit any fixes**

```bash
git add -A
git commit -m "fix: address issues found in integration testing"
```

### Task 11: Verify existing tests still pass

- [ ] **Step 1: Run full test suite**

Run: `cargo test --workspace`
Expected: All tests PASS

- [ ] **Step 2: Run clippy one final time**

Run: `cargo clippy --workspace && cargo fmt --all`
Expected: Clean
