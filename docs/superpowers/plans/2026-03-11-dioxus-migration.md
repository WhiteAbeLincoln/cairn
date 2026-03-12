# Dioxus Fullstack Migration Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Migrate ccmux from Rust/Axum + SolidJS + GraphQL to a unified Dioxus fullstack app with a shared `ccmux-core` library crate.

**Architecture:** Cargo workspace with `ccmux-core` (session parsing, event types, display pipeline) and `ccmux-app` (Dioxus fullstack). Server functions replace GraphQL. The server transforms JSONL logs into `DisplayItem`s; the client never sees raw log formats.

**Tech Stack:** Rust, Dioxus 0.6 (fullstack, SSR + WASM), pulldown-cmark, notify, serde, tokio

**Spec:** `docs/superpowers/specs/2026-03-11-dioxus-migration-design.md`

---

## Chunk 1: Workspace Setup & Core Crate Scaffolding

### Task 1: Create workspace structure

**Files:**
- Modify: `Cargo.toml` (root — convert to workspace)
- Create: `crates/ccmux-core/Cargo.toml`
- Create: `crates/ccmux-core/src/lib.rs`
- Create: `crates/ccmux-app/Cargo.toml`
- Create: `crates/ccmux-app/src/main.rs`

- [ ] **Step 1: Convert root Cargo.toml to workspace**

Replace the current root `Cargo.toml` (which defines the `ccmux` package) with a workspace manifest:

```toml
[workspace]
members = ["crates/ccmux-core", "crates/ccmux-app"]
resolver = "2"
```

- [ ] **Step 2: Create ccmux-core crate**

`crates/ccmux-core/Cargo.toml`:
```toml
[package]
name = "ccmux-core"
version = "0.1.0"
edition = "2024"

[dependencies]
chrono = { version = "0.4", features = ["serde"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tracing = "0.1"
```

`crates/ccmux-core/src/lib.rs`:
```rust
pub mod session;
pub mod events;
pub mod display;
```

Create empty module files so it compiles:
- `crates/ccmux-core/src/session/mod.rs` → `pub mod loader;`
- `crates/ccmux-core/src/session/loader.rs` → empty (will be populated in Task 2)
- `crates/ccmux-core/src/events/mod.rs` → `pub mod parse;`
- `crates/ccmux-core/src/events/parse.rs` → empty
- `crates/ccmux-core/src/display/mod.rs` → `pub mod pipeline;`
- `crates/ccmux-core/src/display/pipeline.rs` → empty

- [ ] **Step 3: Create ccmux-app crate with minimal Dioxus scaffold**

`crates/ccmux-app/Cargo.toml`:
```toml
[package]
name = "ccmux-app"
version = "0.1.0"
edition = "2024"

[dependencies]
ccmux-core = { path = "../ccmux-core" }
dioxus = { version = "0.6", features = ["fullstack"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["full"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

[features]
default = []
server = ["dioxus/server"]
web = ["dioxus/web"]
```

`crates/ccmux-app/src/main.rs`:
```rust
use dioxus::prelude::*;

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    rsx! {
        h1 { "ccmux" }
        p { "Migration in progress..." }
    }
}
```

Create `crates/ccmux-app/Dioxus.toml`:
```toml
[application]
name = "ccmux"

[web.app]
title = "ccmux"

[web.resource.dev]
style = []
```

- [ ] **Step 4: Verify workspace compiles**

Run: `cargo check --workspace`
Expected: Compiles with no errors (warnings from empty files are OK).

- [ ] **Step 5: Verify Dioxus app launches**

Run: `cd crates/ccmux-app && dx serve`
Expected: Dev server starts. Browser shows "ccmux / Migration in progress..."

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat: set up cargo workspace with ccmux-core and ccmux-app crates"
```

---

### Task 2: Move session loader into ccmux-core

**Files:**
- Copy from: `src/session/loader.rs`
- Create: `crates/ccmux-core/src/session/loader.rs`
- Modify: `crates/ccmux-core/src/session/mod.rs`

The existing `src/session/loader.rs` is already well-isolated with no GraphQL dependencies. It only depends on `std`, `chrono`, `serde_json`, and `tracing`.

- [ ] **Step 1: Copy loader.rs into ccmux-core**

Copy the contents of `src/session/loader.rs` into `crates/ccmux-core/src/session/loader.rs`. The file contains:
- `SessionInfo` struct (lines 7-24)
- `discover_sessions()` (lines 27-134)
- `scan_session_metadata()` (lines 147-203) — private
- `extract_user_content_string()` (lines 207-234) — private
- `extract_json_string()` (lines 237-243) — private
- `extract_agent_map()` (lines 246-270) — public
- `load_session_raw()` (lines 273-292) — public

No changes needed — these functions have no GraphQL imports.

Update `crates/ccmux-core/src/session/mod.rs`:
```rust
pub mod loader;
pub use loader::SessionInfo;
```

- [ ] **Step 2: Verify ccmux-core compiles**

Run: `cargo check -p ccmux-core`
Expected: Compiles with no errors.

- [ ] **Step 3: Write a test for discover_sessions with a temp directory**

Create `crates/ccmux-core/src/session/loader.rs` test module at the bottom of the file:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_discover_sessions_finds_jsonl_files() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = dir.path().join("my-project");
        std::fs::create_dir_all(&project_dir).unwrap();

        let session_path = project_dir.join("abc123.jsonl");
        let mut f = std::fs::File::create(&session_path).unwrap();
        writeln!(f, r#"{{"type":"user","timestamp":"2026-01-01T00:00:00Z","message":{{"role":"user","content":"hello"}},"cwd":"/tmp","sessionId":"abc123","userType":"external","uuid":"u1","version":"1","isSidechain":false}}"#).unwrap();

        let sessions = discover_sessions(dir.path()).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "abc123");
        assert_eq!(sessions[0].project, "my-project");
        assert_eq!(sessions[0].first_message, Some("hello".to_string()));
    }

    #[test]
    fn test_discover_sessions_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = discover_sessions(dir.path()).unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn test_discover_sessions_nonexistent_dir() {
        let sessions = discover_sessions(Path::new("/nonexistent/path")).unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn test_load_session_raw() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"type":"user","msg":"hello"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","msg":"hi"}}"#).unwrap();

        let events = load_session_raw(&path).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["type"], "user");
        assert_eq!(events[1]["type"], "assistant");
    }
}
```

Add `tempfile` as a dev dependency in `crates/ccmux-core/Cargo.toml`:
```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p ccmux-core`
Expected: All 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ccmux-core/
git commit -m "feat: move session loader into ccmux-core with tests"
```

---

### Task 3: Define event types in ccmux-core

**Files:**
- Create: `crates/ccmux-core/src/events/mod.rs`
- Create: `crates/ccmux-core/src/events/parse.rs`

Extract the serde data structs from `src/graphql/types.rs` (lines 24-179) into `ccmux-core`, stripping all GraphQL attributes. These are internal types — they don't need `Serialize`, only `Deserialize`.

- [ ] **Step 1: Define event data structs in events/mod.rs**

`crates/ccmux-core/src/events/mod.rs`:

```rust
pub mod parse;

use chrono::{DateTime, Utc};
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
```

- [ ] **Step 2: Implement JSONL line parsing in events/parse.rs**

`crates/ccmux-core/src/events/parse.rs`:

```rust
use serde::Deserialize;
use serde_json::Value;

use super::*;

/// Parse a single raw JSON value into a typed Event.
pub fn parse_event(raw: &Value) -> Event {
    let event_type = raw.get("type").and_then(|v| v.as_str()).unwrap_or("");

    macro_rules! try_parse {
        ($variant:ident, $data_type:ty) => {
            match <$data_type>::deserialize(raw) {
                Ok(data) => Event::$variant(data),
                Err(e) => {
                    tracing::warn!(
                        event_type,
                        %e,
                        "failed to parse event, falling back to Unknown"
                    );
                    Event::Unknown(raw.clone())
                }
            }
        };
    }

    match event_type {
        "assistant" => try_parse!(Assistant, AssistantEventData),
        "user" => try_parse!(User, UserEventData),
        "system" => try_parse!(System, SystemEventData),
        "progress" => try_parse!(Progress, ProgressEventData),
        "file-history-snapshot" => try_parse!(FileHistory, FileHistoryEventData),
        "queue_operation" => try_parse!(QueueOperation, QueueOperationEventData),
        _ => Event::Unknown(raw.clone()),
    }
}

/// Parse all raw JSON values into typed Events.
pub fn parse_events(raw_events: &[Value]) -> Vec<Event> {
    raw_events.iter().map(parse_event).collect()
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p ccmux-core`
Expected: Compiles.

- [ ] **Step 4: Write tests for event parsing**

Add to `crates/ccmux-core/src/events/parse.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_assistant_event() {
        let raw = json!({
            "type": "assistant",
            "cwd": "/tmp",
            "isSidechain": false,
            "sessionId": "s1",
            "timestamp": "2026-01-01T00:00:00Z",
            "userType": "external",
            "uuid": "u1",
            "version": "1",
            "message": {"role": "assistant", "content": [{"type": "text", "text": "hello"}]}
        });
        let event = parse_event(&raw);
        assert!(matches!(event, Event::Assistant(_)));
    }

    #[test]
    fn test_parse_user_event() {
        let raw = json!({
            "type": "user",
            "cwd": "/tmp",
            "isSidechain": false,
            "sessionId": "s1",
            "timestamp": "2026-01-01T00:00:00Z",
            "userType": "external",
            "uuid": "u2",
            "version": "1",
            "message": {"role": "user", "content": "hello"}
        });
        let event = parse_event(&raw);
        assert!(matches!(event, Event::User(_)));
    }

    #[test]
    fn test_parse_unknown_type() {
        let raw = json!({"type": "something_new", "data": 42});
        let event = parse_event(&raw);
        assert!(matches!(event, Event::Unknown(_)));
    }

    #[test]
    fn test_parse_malformed_falls_back_to_unknown() {
        // Missing required fields for assistant
        let raw = json!({"type": "assistant"});
        let event = parse_event(&raw);
        assert!(matches!(event, Event::Unknown(_)));
    }
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p ccmux-core`
Expected: All tests pass (session tests + event tests).

- [ ] **Step 6: Commit**

```bash
git add crates/ccmux-core/src/events/
git commit -m "feat: define event types and JSONL parser in ccmux-core"
```

---

### Task 4: Define display types and pipeline in ccmux-core

**Files:**
- Create: `crates/ccmux-core/src/display/mod.rs`
- Create: `crates/ccmux-core/src/display/pipeline.rs`

This is the Rust rewrite of `web/src/lib/display-item.ts`. The pipeline transforms `Vec<Event>` + raw JSON → `Vec<DisplayItem>`, applying `DisplayOpts` for mode assignment and grouping consecutive `Grouped` items.

- [ ] **Step 1: Define display types in display/mod.rs**

`crates/ccmux-core/src/display/mod.rs`:

```rust
pub mod pipeline;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A display item sent to the client. The client never sees raw Event types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum DisplayItem {
    UserMessage {
        content: String,
        raw: Value,
    },
    AssistantMessage {
        text: String,
        raw: Value,
    },
    Thinking {
        text: String,
        raw: Value,
    },
    ToolUse {
        name: String,
        tool_use_id: String,
        input: Value,
        result: Option<ToolResultData>,
        raw: Value,
    },
    TaskList {
        tasks: Vec<TaskItem>,
        raw: Value,
    },
    TurnDuration {
        duration_ms: u64,
        raw: Value,
    },
    Compaction {
        content: String,
        raw: Value,
    },
    /// Heterogeneous group of items with Grouped display mode.
    Group {
        items: Vec<DisplayItem>,
    },
    Other {
        raw: Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultData {
    pub output: Option<String>,
    pub error: Option<String>,
    pub raw: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplayMode {
    Full,
    Collapsed,
    Grouped,
    TaskList,
    Hidden,
}

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
        use DisplayMode::*;

        let defaults = HashMap::from([
            (UserMessage, Full),
            (AssistantMessage, Full),
            (Thinking, Grouped),
            (ToolUse, Grouped),
            (ToolResult, Hidden),
            (TurnDuration, Full),
            (Compaction, Grouped),
            (Other, Hidden),
        ]);

        let tool_overrides = HashMap::from([
            ("Bash".to_string(), Full),
            ("AskUserQuestion".to_string(), Full),
            ("TaskCreate".to_string(), TaskList),
            ("TaskUpdate".to_string(), TaskList),
            ("TaskGet".to_string(), TaskList),
            ("TaskList".to_string(), TaskList),
        ]);

        Self {
            defaults,
            tool_overrides,
        }
    }
}
```

- [ ] **Step 2: Implement the display pipeline in display/pipeline.rs**

`crates/ccmux-core/src/display/pipeline.rs`:

This implements the core transform: raw events → display items, following the same logic as `web/src/lib/display-item.ts:eventsToDisplayItems()`.

```rust
use std::collections::HashMap;

use serde_json::Value;

use crate::events::Event;

use super::{
    DisplayItem, DisplayItemDiscriminant, DisplayMode, DisplayOpts, TaskItem, TaskStatus,
    ToolResultData,
};

/// Transform parsed events + their raw JSON into display items.
///
/// The `raw_events` slice must correspond 1:1 with `events` (same indices).
pub fn events_to_display_items(
    events: &[Event],
    raw_events: &[Value],
    opts: &DisplayOpts,
) -> Vec<DisplayItem> {
    // Index tool results by tool_use_id for pairing with tool-use items.
    let mut tool_results: HashMap<String, ToolResultData> = HashMap::new();
    // Pre-scan for tool results
    for (event, raw) in events.iter().zip(raw_events.iter()) {
        if let Event::User(u) = event {
            if let Some(content) = u.message.get("content") {
                if let Some(arr) = content.as_array() {
                    for item in arr {
                        if item.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                            if let Some(id) = item.get("tool_use_id").and_then(|v| v.as_str()) {
                                let output = item
                                    .get("content")
                                    .and_then(|c| c.as_str())
                                    .map(|s| s.to_string());
                                let error = item
                                    .get("error")
                                    .and_then(|e| e.as_str())
                                    .map(|s| s.to_string());
                                tool_results.insert(
                                    id.to_string(),
                                    ToolResultData {
                                        output,
                                        error,
                                        raw: item.clone(),
                                    },
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    // Convert events to intermediate items with their modes.
    let mut intermediate: Vec<(DisplayItem, DisplayMode)> = Vec::new();

    for (event, raw) in events.iter().zip(raw_events.iter()) {
        let items = event_to_display_items(event, raw, opts, &tool_results);
        intermediate.extend(items);
    }

    // Group consecutive items with the same Grouped/TaskList mode.
    let mut result: Vec<DisplayItem> = Vec::new();
    let mut grouped_acc: Vec<DisplayItem> = Vec::new();
    let mut task_acc: Vec<DisplayItem> = Vec::new();

    for (item, mode) in intermediate {
        match mode {
            DisplayMode::Grouped => {
                flush_tasks(&mut task_acc, &mut result);
                grouped_acc.push(item);
            }
            DisplayMode::TaskList => {
                flush_grouped(&mut grouped_acc, &mut result);
                task_acc.push(item);
            }
            DisplayMode::Hidden => {
                // Skip hidden items entirely
            }
            _ => {
                flush_grouped(&mut grouped_acc, &mut result);
                flush_tasks(&mut task_acc, &mut result);
                result.push(item);
            }
        }
    }
    flush_grouped(&mut grouped_acc, &mut result);
    flush_tasks(&mut task_acc, &mut result);

    result
}

fn flush_grouped(acc: &mut Vec<DisplayItem>, result: &mut Vec<DisplayItem>) {
    if !acc.is_empty() {
        if acc.len() == 1 {
            result.push(acc.remove(0));
        } else {
            result.push(DisplayItem::Group {
                items: std::mem::take(acc),
            });
        }
    }
}

fn flush_tasks(acc: &mut Vec<DisplayItem>, result: &mut Vec<DisplayItem>) {
    if !acc.is_empty() {
        // Merge all task items into a single TaskList
        let mut tasks: Vec<TaskItem> = Vec::new();
        let mut raw = Value::Array(Vec::new());
        for item in acc.drain(..) {
            if let DisplayItem::TaskList {
                tasks: t,
                raw: r,
            } = item
            {
                tasks.extend(t);
                if let Value::Array(ref mut arr) = raw {
                    arr.push(r);
                }
            }
        }
        result.push(DisplayItem::TaskList { tasks, raw });
    }
}

/// Convert a single event + raw JSON into (DisplayItem, DisplayMode) pairs.
/// One event can produce multiple display items (e.g., an assistant event
/// with text + thinking + tool_use content blocks).
fn event_to_display_items(
    event: &Event,
    raw: &Value,
    opts: &DisplayOpts,
    tool_results: &HashMap<String, ToolResultData>,
) -> Vec<(DisplayItem, DisplayMode)> {
    match event {
        Event::User(u) => user_event_items(u, raw, opts, tool_results),
        Event::Assistant(a) => assistant_event_items(a, raw, opts, tool_results),
        Event::System(s) => system_event_items(s, raw, opts),
        Event::Progress(_) => vec![], // Hidden
        Event::FileHistory(_) => vec![], // Hidden
        Event::QueueOperation(_) => vec![], // Hidden
        Event::Unknown(_) => {
            let mode = opts
                .defaults
                .get(&DisplayItemDiscriminant::Other)
                .copied()
                .unwrap_or(DisplayMode::Hidden);
            vec![(DisplayItem::Other { raw: raw.clone() }, mode)]
        }
    }
}

fn user_event_items(
    u: &crate::events::UserEventData,
    raw: &Value,
    opts: &DisplayOpts,
    _tool_results: &HashMap<String, ToolResultData>,
) -> Vec<(DisplayItem, DisplayMode)> {
    // Compaction events
    if u.is_compact_summary == Some(true) {
        if let Some(content) = u.message.get("content").and_then(|c| c.as_str()) {
            let mode = opts
                .defaults
                .get(&DisplayItemDiscriminant::Compaction)
                .copied()
                .unwrap_or(DisplayMode::Grouped);
            return vec![(
                DisplayItem::Compaction {
                    content: content.to_string(),
                    raw: raw.clone(),
                },
                mode,
            )];
        }
    }

    // Plain user message (string content, external user, not a tool result)
    if let Some(content) = u.message.get("content").and_then(|c| c.as_str()) {
        if u.message.get("role").and_then(|r| r.as_str()) == Some("user")
            && u.core.user_type == "external"
            && u.tool_use_result.is_none()
            && u.source_tool_assistant_uuid.is_none()
            && u.is_compact_summary != Some(true)
        {
            let mode = opts
                .defaults
                .get(&DisplayItemDiscriminant::UserMessage)
                .copied()
                .unwrap_or(DisplayMode::Full);
            return vec![(
                DisplayItem::UserMessage {
                    content: content.to_string(),
                    raw: raw.clone(),
                },
                mode,
            )];
        }
    }

    // Array content — contains tool_results (which we index) and other items
    if let Some(arr) = u.message.get("content").and_then(|c| c.as_array()) {
        let mut items = Vec::new();
        for item in arr {
            if item.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                // Tool results are hidden — they get paired with tool-use items
                // via the pre-scan. Nothing to emit here.
                continue;
            }
        }
        // If there are non-tool-result items, emit as Other
        let non_tool_results: Vec<&Value> = arr
            .iter()
            .filter(|item| item.get("type").and_then(|t| t.as_str()) != Some("tool_result"))
            .collect();
        if !non_tool_results.is_empty() {
            let mode = opts
                .defaults
                .get(&DisplayItemDiscriminant::Other)
                .copied()
                .unwrap_or(DisplayMode::Hidden);
            items.push((DisplayItem::Other { raw: raw.clone() }, mode));
        }
        return items;
    }

    // Fallback
    let mode = opts
        .defaults
        .get(&DisplayItemDiscriminant::Other)
        .copied()
        .unwrap_or(DisplayMode::Hidden);
    vec![(DisplayItem::Other { raw: raw.clone() }, mode)]
}

fn assistant_event_items(
    a: &crate::events::AssistantEventData,
    raw: &Value,
    opts: &DisplayOpts,
    tool_results: &HashMap<String, ToolResultData>,
) -> Vec<(DisplayItem, DisplayMode)> {
    let model = a
        .message
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("");
    if model == "<synthetic>" {
        let mode = opts
            .defaults
            .get(&DisplayItemDiscriminant::Other)
            .copied()
            .unwrap_or(DisplayMode::Hidden);
        return vec![(DisplayItem::Other { raw: raw.clone() }, mode)];
    }

    if let Some(arr) = a.message.get("content").and_then(|c| c.as_array()) {
        let mut items = Vec::new();

        for block in arr {
            let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match block_type {
                "text" => {
                    let text = block.get("text").and_then(|t| t.as_str()).unwrap_or("");
                    let mode = opts
                        .defaults
                        .get(&DisplayItemDiscriminant::AssistantMessage)
                        .copied()
                        .unwrap_or(DisplayMode::Full);
                    items.push((
                        DisplayItem::AssistantMessage {
                            text: text.to_string(),
                            raw: raw.clone(),
                        },
                        mode,
                    ));
                }
                "thinking" => {
                    let thinking = block
                        .get("thinking")
                        .and_then(|t| t.as_str())
                        .unwrap_or("");
                    let mode = opts
                        .defaults
                        .get(&DisplayItemDiscriminant::Thinking)
                        .copied()
                        .unwrap_or(DisplayMode::Grouped);
                    items.push((
                        DisplayItem::Thinking {
                            text: thinking.to_string(),
                            raw: raw.clone(),
                        },
                        mode,
                    ));
                }
                "tool_use" => {
                    let name = block
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    let id = block
                        .get("id")
                        .and_then(|i| i.as_str())
                        .unwrap_or("")
                        .to_string();
                    let input = block.get("input").cloned().unwrap_or(Value::Null);

                    // Determine mode: check tool_overrides first, then defaults
                    let mode = opts
                        .tool_overrides
                        .get(&name)
                        .copied()
                        .unwrap_or_else(|| {
                            opts.defaults
                                .get(&DisplayItemDiscriminant::ToolUse)
                                .copied()
                                .unwrap_or(DisplayMode::Grouped)
                        });

                    // For task-related tools, emit as TaskList
                    if mode == DisplayMode::TaskList {
                        let task_items = extract_task_items(&name, &input, &id);
                        items.push((
                            DisplayItem::TaskList {
                                tasks: task_items,
                                raw: raw.clone(),
                            },
                            mode,
                        ));
                    } else {
                        let result = tool_results.get(&id).cloned();
                        items.push((
                            DisplayItem::ToolUse {
                                name,
                                tool_use_id: id,
                                input,
                                result,
                                raw: raw.clone(),
                            },
                            mode,
                        ));
                    }
                }
                _ => {
                    let mode = opts
                        .defaults
                        .get(&DisplayItemDiscriminant::Other)
                        .copied()
                        .unwrap_or(DisplayMode::Hidden);
                    items.push((DisplayItem::Other { raw: raw.clone() }, mode));
                }
            }
        }

        return items;
    }

    let mode = opts
        .defaults
        .get(&DisplayItemDiscriminant::Other)
        .copied()
        .unwrap_or(DisplayMode::Hidden);
    vec![(DisplayItem::Other { raw: raw.clone() }, mode)]
}

fn system_event_items(
    s: &crate::events::SystemEventData,
    raw: &Value,
    opts: &DisplayOpts,
) -> Vec<(DisplayItem, DisplayMode)> {
    if s.subtype.as_deref() == Some("turn_duration") {
        let duration_ms = s.duration_ms.unwrap_or(0) as u64;
        let mode = opts
            .defaults
            .get(&DisplayItemDiscriminant::TurnDuration)
            .copied()
            .unwrap_or(DisplayMode::Full);
        return vec![(DisplayItem::TurnDuration { duration_ms, raw: raw.clone() }, mode)];
    }

    let mode = opts
        .defaults
        .get(&DisplayItemDiscriminant::Other)
        .copied()
        .unwrap_or(DisplayMode::Hidden);
    vec![(DisplayItem::Other { raw: raw.clone() }, mode)]
}

/// Extract task items from TaskCreate/TaskUpdate tool inputs.
fn extract_task_items(tool_name: &str, input: &Value, tool_use_id: &str) -> Vec<TaskItem> {
    match tool_name {
        "TaskCreate" => {
            let subject = input
                .get("subject")
                .and_then(|s| s.as_str())
                .unwrap_or("Untitled")
                .to_string();
            vec![TaskItem {
                id: tool_use_id.to_string(),
                subject,
                status: TaskStatus::Pending,
            }]
        }
        "TaskUpdate" => {
            let id = input
                .get("taskId")
                .and_then(|i| i.as_str())
                .unwrap_or(tool_use_id)
                .to_string();
            let status = match input.get("status").and_then(|s| s.as_str()) {
                Some("in_progress") => TaskStatus::InProgress,
                Some("completed") => TaskStatus::Completed,
                Some("deleted") => TaskStatus::Cancelled,
                _ => TaskStatus::Pending,
            };
            let subject = input
                .get("subject")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            vec![TaskItem {
                id,
                subject,
                status,
            }]
        }
        _ => vec![],
    }
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p ccmux-core`
Expected: Compiles.

- [ ] **Step 4: Write tests for the display pipeline**

Add to `crates/ccmux-core/src/display/pipeline.rs`:

```rust
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
        assert!(matches!(&items[0], DisplayItem::UserMessage { content, .. } if content == "hello world"));
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
        assert!(matches!(&items[0], DisplayItem::Thinking { text, .. } if text == "let me think..."));
        assert!(matches!(&items[1], DisplayItem::AssistantMessage { text, .. } if text == "here is my answer"));
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
        assert!(matches!(&items[0], DisplayItem::TurnDuration { duration_ms: 1500, .. }));
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
        // Make thinking blocks Full instead of Grouped
        opts.defaults.insert(DisplayItemDiscriminant::Thinking, DisplayMode::Full);

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

        // Both should be Full (not grouped), so 2 separate items
        assert_eq!(items.len(), 2);
        assert!(matches!(&items[0], DisplayItem::Thinking { .. }));
        assert!(matches!(&items[1], DisplayItem::Thinking { .. }));
    }
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p ccmux-core`
Expected: All tests pass.

- [ ] **Step 6: Run clippy and fmt**

Run: `cargo clippy -p ccmux-core -- -D warnings && cargo fmt -p ccmux-core`
Expected: No warnings or errors.

- [ ] **Step 7: Commit**

```bash
git add crates/ccmux-core/src/display/
git commit -m "feat: implement display item types and pipeline with grouping logic"
```

---

## Chunk 2: Dioxus App — Server Functions & Basic UI

### Task 5: Implement server functions

**Files:**
- Create: `crates/ccmux-app/src/server/mod.rs`
- Create: `crates/ccmux-app/src/server/sessions.rs`
- Create: `crates/ccmux-app/src/server/streaming.rs`
- Modify: `crates/ccmux-app/src/main.rs`

- [ ] **Step 1: Implement list_sessions and get_session server functions**

`crates/ccmux-app/src/server/mod.rs`:
```rust
pub mod sessions;
pub mod streaming;
```

`crates/ccmux-app/src/server/sessions.rs`:
```rust
use ccmux_core::display::{DisplayItem, DisplayOpts};
use ccmux_core::display::pipeline::events_to_display_items;
use ccmux_core::events::parse::parse_events;
use ccmux_core::session::loader;
use ccmux_core::session::SessionInfo;
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub project: String,
    pub slug: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub message_count: i32,
    pub first_message: Option<String>,
    pub project_path: Option<String>,
    pub is_sidechain: bool,
    pub parent_session_id: Option<String>,
    pub agent_id: Option<String>,
}

impl From<&SessionInfo> for SessionMeta {
    fn from(s: &SessionInfo) -> Self {
        SessionMeta {
            id: s.id.clone(),
            project: s.project.clone(),
            slug: s.slug.clone(),
            created_at: s.created_at.map(|d| d.to_rfc3339()),
            updated_at: s.updated_at.map(|d| d.to_rfc3339()),
            message_count: s.message_count as i32,
            first_message: s.first_message.clone(),
            project_path: s.project_path.clone(),
            is_sidechain: s.is_sidechain,
            parent_session_id: s.parent_session_id.clone(),
            agent_id: s.agent_id.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionResponse {
    pub meta: SessionMeta,
    pub items: Vec<DisplayItem>,
}

fn base_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").expect("HOME not set");
    std::path::PathBuf::from(format!("{home}/.claude/projects"))
}

#[server]
pub async fn list_sessions(project: Option<String>) -> Result<Vec<SessionMeta>, ServerFnError> {
    let sessions = loader::discover_sessions(&base_path())
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    let filtered: Vec<SessionMeta> = sessions
        .iter()
        .filter(|s| {
            s.first_message.is_some()
                && !s.is_sidechain
                && project
                    .as_ref()
                    .is_none_or(|p| s.project.contains(p.as_str()))
        })
        .map(SessionMeta::from)
        .collect();

    Ok(filtered)
}

#[server]
pub async fn get_session(id: String) -> Result<SessionResponse, ServerFnError> {
    let sessions = loader::discover_sessions(&base_path())
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    let session = sessions
        .iter()
        .find(|s| s.id == id)
        .ok_or_else(|| ServerFnError::new(format!("Session {id} not found")))?;

    let raw_events = loader::load_session_raw(&session.path)
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    let events = parse_events(&raw_events);
    let opts = DisplayOpts::default();
    let items = events_to_display_items(&events, &raw_events, &opts);

    Ok(SessionResponse {
        meta: SessionMeta::from(session),
        items,
    })
}
```

- [ ] **Step 2: Create streaming server function stub**

`crates/ccmux-app/src/server/streaming.rs`:
```rust
// SSE streaming will be implemented in Task 8.
// Placeholder to keep the module structure in place.
```

- [ ] **Step 3: Wire server module into main.rs**

Update `crates/ccmux-app/src/main.rs`. Note: the server module must be cfg-gated because it imports `ccmux_core::session::loader` which does filesystem I/O and cannot compile to WASM:
```rust
#[cfg(feature = "server")]
mod server;

use dioxus::prelude::*;

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    rsx! {
        h1 { "ccmux" }
        p { "Migration in progress..." }
    }
}
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check --workspace`
Expected: Compiles. The `#[server]` macro generates both server and client stubs.

- [ ] **Step 5: Commit**

```bash
git add crates/ccmux-app/
git commit -m "feat: implement list_sessions and get_session server functions"
```

---

### Task 6: Build session list page

**Files:**
- Create: `crates/ccmux-app/src/components/mod.rs`
- Create: `crates/ccmux-app/src/components/app.rs`
- Create: `crates/ccmux-app/src/components/session_list.rs`
- Create: `crates/ccmux-app/src/routes.rs`
- Modify: `crates/ccmux-app/src/main.rs`
- Create: `crates/ccmux-app/assets/style.css`

- [ ] **Step 1: Create routes and app layout**

`crates/ccmux-app/src/routes.rs`:
```rust
use dioxus::prelude::*;

use crate::components::app::AppLayout;
use crate::components::session_list::SessionList;
use crate::components::session_view::SessionView;

#[derive(Routable, Clone, PartialEq)]
pub enum Route {
    #[layout(AppLayout)]
        #[route("/")]
        SessionList {},
        #[route("/session/:id")]
        SessionView { id: String },
}
```

`crates/ccmux-app/src/components/mod.rs`:
```rust
pub mod app;
pub mod session_list;
pub mod session_view;
pub mod blocks;
```

`crates/ccmux-app/src/components/app.rs`:
```rust
use dioxus::prelude::*;

use crate::routes::Route;

#[component]
pub fn AppLayout() -> Element {
    rsx! {
        nav { class: "nav",
            Link { to: Route::SessionList {}, "ccmux" }
        }
        main { class: "main",
            Outlet::<Route> {}
        }
    }
}
```

- [ ] **Step 2: Implement session list component**

`crates/ccmux-app/src/components/session_list.rs`:
Note on Dioxus 0.6 API: `use_server_future` returns a `Resource`. The `?` operator suspends the component if data hasn't loaded yet (returns `VNode` via `Suspend`). After `?`, `.read()` gives `ReadableRef` which derefs to `Option<Result<...>>`. Verify this pattern compiles against the exact Dioxus 0.6 version — if the API differs, use `match sessions.read().as_ref()` without `?` instead.

```rust
use std::collections::BTreeMap;

use dioxus::prelude::*;

use crate::routes::Route;
use crate::server::sessions::{list_sessions, SessionMeta};

#[component]
pub fn SessionList() -> Element {
    let sessions = use_server_future(move || list_sessions(None))?;

    match &*sessions.read() {
        Some(Ok(sessions)) => {
            // Group by project
            let mut by_project: BTreeMap<String, Vec<&SessionMeta>> = BTreeMap::new();
            for s in sessions {
                by_project.entry(s.project.clone()).or_default().push(s);
            }

            rsx! {
                div { class: "session-list",
                    for (project, sessions) in by_project {
                        div { class: "project-group",
                            h2 { class: "project-name", "{project}" }
                            for session in sessions {
                                Link {
                                    to: Route::SessionView { id: session.id.clone() },
                                    class: "session-card",
                                    div { class: "session-preview",
                                        {session.first_message.as_deref().unwrap_or("(no message)")}
                                    }
                                    div { class: "session-meta",
                                        span { class: "message-count", "{session.message_count} events" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        Some(Err(e)) => rsx! { p { class: "error", "Error: {e}" } },
        None => rsx! { p { "Loading..." } },
    }
}
```

- [ ] **Step 3: Create placeholder session view**

`crates/ccmux-app/src/components/session_view.rs`:
```rust
use dioxus::prelude::*;

#[component]
pub fn SessionView(id: String) -> Element {
    rsx! {
        div { class: "session-view",
            h2 { "Session: {id}" }
            p { "Session view coming in next task..." }
        }
    }
}
```

Create placeholder blocks module:
`crates/ccmux-app/src/components/blocks/mod.rs`:
```rust
pub mod display_item;
pub mod message;
pub mod prose;
pub mod thinking;
pub mod tool_use;
pub mod task_list;
```

Create empty placeholder files for each block component (will be implemented in Task 7).

- [ ] **Step 4: Create basic CSS**

`crates/ccmux-app/assets/style.css`:
```css
:root {
    --bg: #1a1a2e;
    --bg-surface: #16213e;
    --bg-hover: #1a2744;
    --text: #e0e0e0;
    --text-muted: #8892a4;
    --accent: #4a9eff;
    --border: #2a3a5e;
    --font-mono: 'SF Mono', 'Fira Code', 'Cascadia Code', monospace;
    --font-sans: -apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, sans-serif;
}

* { margin: 0; padding: 0; box-sizing: border-box; }

body {
    font-family: var(--font-sans);
    background: var(--bg);
    color: var(--text);
    line-height: 1.6;
}

.nav {
    padding: 0.75rem 1.5rem;
    border-bottom: 1px solid var(--border);
    background: var(--bg-surface);
}

.nav a {
    color: var(--accent);
    text-decoration: none;
    font-weight: 600;
    font-size: 1.1rem;
}

.main {
    max-width: 960px;
    margin: 0 auto;
    padding: 1.5rem;
}

.session-list { display: flex; flex-direction: column; gap: 1.5rem; }

.project-group { display: flex; flex-direction: column; gap: 0.5rem; }

.project-name {
    font-size: 0.85rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-muted);
    padding-bottom: 0.25rem;
    border-bottom: 1px solid var(--border);
}

.session-card {
    display: block;
    padding: 0.75rem 1rem;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: 6px;
    text-decoration: none;
    color: var(--text);
    transition: background 0.15s;
}

.session-card:hover { background: var(--bg-hover); }

.session-preview {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    margin-bottom: 0.25rem;
}

.session-meta {
    font-size: 0.8rem;
    color: var(--text-muted);
}

.error { color: #ff6b6b; }
```

- [ ] **Step 5: Wire everything together in main.rs**

Update `crates/ccmux-app/src/main.rs`:
```rust
mod components;
mod routes;
#[cfg(feature = "server")]
mod server;

use dioxus::prelude::*;
use routes::Route;

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    rsx! {
        document::Stylesheet { href: asset!("/assets/style.css") }
        Router::<Route> {}
    }
}
```

Update `crates/ccmux-app/Dioxus.toml` to include assets:
```toml
[application]
name = "ccmux"
default_platform = "fullstack"

[web.app]
title = "ccmux"

[web.resource.dev]
style = []
```

- [ ] **Step 6: Verify session list renders**

Run: `cd crates/ccmux-app && dx serve`
Expected: Browser shows session list grouped by project. Clicking a session navigates to `/session/:id` (placeholder view).

- [ ] **Step 7: Commit**

```bash
git add crates/ccmux-app/
git commit -m "feat: session list page with routing and basic styling"
```

---

### Task 7: Build session view and display item components

**Files:**
- Modify: `crates/ccmux-app/src/components/session_view.rs`
- Create: `crates/ccmux-app/src/components/blocks/display_item.rs`
- Create: `crates/ccmux-app/src/components/blocks/message.rs`
- Create: `crates/ccmux-app/src/components/blocks/prose.rs`
- Create: `crates/ccmux-app/src/components/blocks/thinking.rs`
- Create: `crates/ccmux-app/src/components/blocks/tool_use.rs`
- Create: `crates/ccmux-app/src/components/blocks/task_list.rs`
- Modify: `crates/ccmux-app/assets/style.css`

- [ ] **Step 1: Implement session view with server function call**

`crates/ccmux-app/src/components/session_view.rs` (same `use_server_future` API note as session list — verify pattern compiles):
```rust
use dioxus::prelude::*;

use crate::components::blocks::display_item::DisplayItemView;
use crate::server::sessions::get_session;

#[component]
pub fn SessionView(id: String) -> Element {
    let session = use_server_future(move || {
        let id = id.clone();
        async move { get_session(id).await }
    })?;

    match &*session.read() {
        Some(Ok(resp)) => {
            rsx! {
                div { class: "session-view",
                    div { class: "session-header",
                        h2 { {resp.meta.first_message.as_deref().unwrap_or(&resp.meta.id)} }
                        span { class: "session-meta", "{resp.meta.message_count} events" }
                    }
                    div { class: "display-items",
                        for (i, item) in resp.items.iter().enumerate() {
                            DisplayItemView { key: "{i}", item: item.clone(), index: i }
                        }
                    }
                }
            }
        }
        Some(Err(e)) => rsx! { p { class: "error", "Error: {e}" } },
        None => rsx! { p { "Loading session..." } },
    }
}
```

- [ ] **Step 2: Implement display item dispatcher**

`crates/ccmux-app/src/components/blocks/display_item.rs`:
```rust
use ccmux_core::display::DisplayItem;
use dioxus::prelude::*;

use super::message::MessageBlock;
use super::prose::Prose;
use super::thinking::ThinkingBlock;
use super::tool_use::ToolUseBlock;
use super::task_list::TaskListBlock;

#[component]
pub fn DisplayItemView(item: DisplayItem, index: usize) -> Element {
    match item {
        DisplayItem::UserMessage { content, .. } => rsx! {
            MessageBlock { label: "User", class: "user-message",
                Prose { text: content }
            }
        },
        DisplayItem::AssistantMessage { text, .. } => rsx! {
            MessageBlock { label: "Assistant", class: "assistant-message",
                Prose { text }
            }
        },
        DisplayItem::Thinking { text, .. } => rsx! {
            ThinkingBlock { text }
        },
        DisplayItem::ToolUse { name, input, result, .. } => rsx! {
            ToolUseBlock { name, input, result }
        },
        DisplayItem::TaskList { tasks, .. } => rsx! {
            TaskListBlock { tasks }
        },
        DisplayItem::TurnDuration { duration_ms, .. } => {
            let secs = duration_ms as f64 / 1000.0;
            rsx! {
                div { class: "turn-duration",
                    "Turn completed in {secs:.1}s"
                }
            }
        },
        DisplayItem::Compaction { content, .. } => rsx! {
            MessageBlock { label: "Compaction", class: "compaction",
                Prose { text: content }
            }
        },
        DisplayItem::Group { items } => rsx! {
            div { class: "group",
                for (i, child) in items.into_iter().enumerate() {
                    DisplayItemView { key: "{index}-{i}", item: child, index: i }
                }
            }
        },
        DisplayItem::Other { .. } => rsx! {
            div { class: "other", "Unknown event" }
        },
    }
}
```

- [ ] **Step 3: Implement message block (collapsible wrapper)**

`crates/ccmux-app/src/components/blocks/message.rs`:
```rust
use dioxus::prelude::*;

#[component]
pub fn MessageBlock(label: String, class: Option<String>, children: Element) -> Element {
    let mut collapsed = use_signal(|| false);
    let css_class = class.unwrap_or_default();

    rsx! {
        div { class: "message-block {css_class}",
            div {
                class: "message-header",
                onclick: move |_| collapsed.toggle(),
                span { class: "message-label", "{label}" }
                span { class: "collapse-indicator",
                    if collapsed() { "\u{25b6}" } else { "\u{25bc}" }
                }
            }
            if !collapsed() {
                div { class: "message-content", {children} }
            }
        }
    }
}
```

- [ ] **Step 4: Implement prose component (pulldown-cmark markdown)**

Add `pulldown-cmark` to `crates/ccmux-app/Cargo.toml`:
```toml
pulldown-cmark = "0.12"
```

`crates/ccmux-app/src/components/blocks/prose.rs`:
```rust
use dioxus::prelude::*;

#[component]
pub fn Prose(text: String) -> Element {
    // Compute markdown HTML directly — re-runs when props change.
    // No use_memo needed since the component only re-renders when text changes.
    let parser = pulldown_cmark::Parser::new(&text);
    let mut html_output = String::new();
    pulldown_cmark::html::push_html(&mut html_output, parser);

    rsx! {
        div {
            class: "prose",
            dangerous_inner_html: "{html_output}"
        }
    }
}
```

- [ ] **Step 5: Implement thinking block**

`crates/ccmux-app/src/components/blocks/thinking.rs`:
```rust
use dioxus::prelude::*;

#[component]
pub fn ThinkingBlock(text: String) -> Element {
    let mut collapsed = use_signal(|| true);
    let preview = if text.len() > 80 {
        format!("{}...", &text[..80])
    } else {
        text.clone()
    };

    rsx! {
        div { class: "thinking-block",
            div {
                class: "thinking-header",
                onclick: move |_| collapsed.toggle(),
                span { class: "thinking-label", "Thinking" }
                if collapsed() {
                    span { class: "thinking-preview", "{preview}" }
                }
            }
            if !collapsed() {
                pre { class: "thinking-content", "{text}" }
            }
        }
    }
}
```

- [ ] **Step 6: Implement tool use block**

`crates/ccmux-app/src/components/blocks/tool_use.rs`:
```rust
use ccmux_core::display::ToolResultData;
use dioxus::prelude::*;
use serde_json::Value;

#[component]
pub fn ToolUseBlock(name: String, input: Value, result: Option<ToolResultData>) -> Element {
    let mut collapsed = use_signal(|| false);

    let input_display = serde_json::to_string_pretty(&input).unwrap_or_default();

    rsx! {
        div { class: "tool-use-block",
            div {
                class: "tool-header",
                onclick: move |_| collapsed.toggle(),
                span { class: "tool-name", "{name}" }
                span { class: "collapse-indicator",
                    if collapsed() { "\u{25b6}" } else { "\u{25bc}" }
                }
            }
            if !collapsed() {
                pre { class: "tool-input", "{input_display}" }
                if let Some(ref res) = result {
                    div { class: "tool-result",
                        if let Some(ref output) = res.output {
                            pre { class: "tool-output", "{output}" }
                        }
                        if let Some(ref error) = res.error {
                            pre { class: "tool-error", "{error}" }
                        }
                    }
                }
            }
        }
    }
}
```

- [ ] **Step 7: Implement task list block**

`crates/ccmux-app/src/components/blocks/task_list.rs`:
```rust
use ccmux_core::display::{TaskItem, TaskStatus};
use dioxus::prelude::*;

#[component]
pub fn TaskListBlock(tasks: Vec<TaskItem>) -> Element {
    let completed = tasks.iter().filter(|t| t.status == TaskStatus::Completed).count();
    let total = tasks.len();

    rsx! {
        div { class: "task-list-block",
            div { class: "task-list-header",
                "Tasks ({completed}/{total} completed)"
            }
            ul { class: "task-list",
                for task in tasks {
                    li { class: "task-item",
                        span { class: "task-checkbox",
                            match task.status {
                                TaskStatus::Pending => "\u{2610}",      // ☐
                                TaskStatus::InProgress => "\u{25d1}",   // ◑
                                TaskStatus::Completed => "\u{2611}",    // ☑
                                TaskStatus::Cancelled => "\u{2612}",    // ☒
                            }
                        }
                        span { class: "task-subject", " {task.subject}" }
                    }
                }
            }
        }
    }
}
```

- [ ] **Step 8: Add CSS for session view and blocks**

Append to `crates/ccmux-app/assets/style.css`:

```css
/* Session view */
.session-view { display: flex; flex-direction: column; gap: 1rem; }
.session-header { margin-bottom: 0.5rem; }
.session-header h2 {
    font-size: 1.1rem;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
}
.display-items { display: flex; flex-direction: column; gap: 0.5rem; }

/* Message block */
.message-block {
    border: 1px solid var(--border);
    border-radius: 6px;
    overflow: hidden;
}
.message-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0.5rem 0.75rem;
    background: var(--bg-surface);
    cursor: pointer;
    user-select: none;
}
.message-label { font-weight: 600; font-size: 0.85rem; }
.collapse-indicator { font-size: 0.7rem; color: var(--text-muted); }
.message-content { padding: 0.75rem; }

.user-message { border-left: 3px solid #4a9eff; }
.assistant-message { border-left: 3px solid #50c878; }
.compaction { border-left: 3px solid #a78bfa; }

/* Prose */
.prose { line-height: 1.7; }
.prose p { margin-bottom: 0.5em; }
.prose code {
    font-family: var(--font-mono);
    background: var(--bg-surface);
    padding: 0.1em 0.3em;
    border-radius: 3px;
    font-size: 0.9em;
}
.prose pre {
    background: var(--bg-surface);
    padding: 0.75rem;
    border-radius: 4px;
    overflow-x: auto;
    margin: 0.5em 0;
}
.prose pre code { background: none; padding: 0; }

/* Thinking */
.thinking-block {
    border: 1px solid var(--border);
    border-radius: 6px;
    border-left: 3px solid #f59e0b;
}
.thinking-header {
    display: flex;
    gap: 0.75rem;
    align-items: center;
    padding: 0.5rem 0.75rem;
    cursor: pointer;
    user-select: none;
}
.thinking-label { font-weight: 600; font-size: 0.85rem; color: #f59e0b; }
.thinking-preview {
    font-family: var(--font-mono);
    font-size: 0.8rem;
    color: var(--text-muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
}
.thinking-content {
    font-family: var(--font-mono);
    font-size: 0.85rem;
    padding: 0.75rem;
    white-space: pre-wrap;
    word-break: break-word;
    border-top: 1px solid var(--border);
}

/* Tool use */
.tool-use-block {
    border: 1px solid var(--border);
    border-radius: 6px;
    border-left: 3px solid #06b6d4;
}
.tool-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0.5rem 0.75rem;
    background: var(--bg-surface);
    cursor: pointer;
    user-select: none;
}
.tool-name { font-weight: 600; font-size: 0.85rem; color: #06b6d4; }
.tool-input, .tool-output {
    font-family: var(--font-mono);
    font-size: 0.8rem;
    padding: 0.75rem;
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 400px;
    overflow-y: auto;
}
.tool-result { border-top: 1px solid var(--border); }
.tool-error {
    font-family: var(--font-mono);
    font-size: 0.8rem;
    padding: 0.75rem;
    color: #ff6b6b;
}

/* Task list */
.task-list-block {
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 0.5rem 0.75rem;
}
.task-list-header {
    font-weight: 600;
    font-size: 0.85rem;
    margin-bottom: 0.5rem;
}
.task-list { list-style: none; }
.task-item { padding: 0.15rem 0; font-size: 0.9rem; }
.task-checkbox { font-size: 1rem; }

/* Group */
.group {
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 0.25rem;
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
}
.group > * { border: none; }

/* Turn duration */
.turn-duration {
    font-size: 0.8rem;
    color: var(--text-muted);
    text-align: center;
    padding: 0.25rem;
}

/* Other */
.other {
    font-size: 0.8rem;
    color: var(--text-muted);
    padding: 0.25rem 0.75rem;
}
```

- [ ] **Step 9: Verify session view renders**

Run: `cd crates/ccmux-app && dx serve`
Expected: Navigate to a session. See display items rendered: user messages, assistant messages, thinking blocks, tool use blocks, task lists, groups.

- [ ] **Step 10: Run clippy**

Run: `cargo clippy --workspace -- -D warnings && cargo fmt --all`
Expected: No warnings.

- [ ] **Step 11: Commit**

```bash
git add crates/ccmux-app/
git commit -m "feat: session view with display item rendering components"
```

---

## Chunk 3: Streaming, Cleanup, and Polish

### Task 8: Implement SSE streaming for live session tailing

**Files:**
- Modify: `crates/ccmux-app/src/server/streaming.rs`
- Modify: `crates/ccmux-app/src/components/session_view.rs`
- Create: `crates/ccmux-core/src/display/streaming.rs`
- Modify: `crates/ccmux-core/src/display/mod.rs`
- Modify: `crates/ccmux-app/Cargo.toml` (add notify, async-stream, tokio deps)

- [ ] **Step 1: Define StreamEvent in ccmux-core**

`crates/ccmux-core/src/display/streaming.rs`:
```rust
use serde::{Deserialize, Serialize};

use super::{DisplayItem, DisplayMode, DisplayOpts, DisplayItemDiscriminant, TaskItem, ToolResultData};

/// Protocol message sent over SSE from server to client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum StreamEvent {
    Append { item: DisplayItem },
    MergeWithPrevious { item: DisplayItem },
    UpdateToolResult { tool_use_id: String, result: ToolResultData },
    UpdateTask { task: TaskItem },
}

/// Minimal server-side state for deciding how to emit stream events.
/// Maintains tool_results index so tool-use items arriving before their
/// results can be updated later via UpdateToolResult.
pub struct StreamPipelineState {
    pub last_mode: Option<DisplayMode>,
    pub opts: DisplayOpts,
    pub tool_results: std::collections::HashMap<String, super::ToolResultData>,
}

impl StreamPipelineState {
    pub fn new(opts: DisplayOpts) -> Self {
        Self {
            last_mode: None,
            opts,
            tool_results: std::collections::HashMap::new(),
        }
    }

    /// Given a new display item and its mode, decide the StreamEvent to emit.
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
                // Task items: if previous was also TaskList, merge; otherwise append
                let event = if self.last_mode == Some(DisplayMode::TaskList) {
                    if let DisplayItem::TaskList { tasks, .. } = &item {
                        // Emit individual task updates for merging
                        // For simplicity, emit as MergeWithPrevious
                        StreamEvent::MergeWithPrevious { item }
                    } else {
                        StreamEvent::Append { item }
                    }
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

    /// Index a tool result that arrived. Returns a StreamEvent to update
    /// an already-emitted ToolUse item, if applicable.
    pub fn index_tool_result(&mut self, tool_use_id: String, result: super::ToolResultData) -> StreamEvent {
        self.tool_results.insert(tool_use_id.clone(), result.clone());
        StreamEvent::UpdateToolResult { tool_use_id, result }
    }
}
```

Add `pub mod streaming;` to `crates/ccmux-core/src/display/mod.rs`.

- [ ] **Step 2: Implement SSE server function**

Add to `crates/ccmux-app/Cargo.toml` dependencies:
```toml
notify = "7"
futures = "0.3"
```

First, in `crates/ccmux-core/src/display/pipeline.rs`, make the per-event function public so streaming can use it:

```rust
/// Public per-event conversion for use in streaming pipeline.
/// Callers must maintain their own tool_results index across events.
pub fn single_event_to_display_items(
    event: &Event,
    raw: &Value,
    opts: &DisplayOpts,
    tool_results: &HashMap<String, ToolResultData>,
) -> Vec<(DisplayItem, DisplayMode)> {
    event_to_display_items(event, raw, opts, tool_results)
}
```

Then `crates/ccmux-app/src/server/streaming.rs`:
```rust
use ccmux_core::display::streaming::{StreamEvent, StreamPipelineState};
use ccmux_core::display::{DisplayOpts, ToolResultData};
use ccmux_core::display::pipeline::single_event_to_display_items;
use ccmux_core::events::{Event, parse::parse_event};
use ccmux_core::session::loader;
use dioxus::prelude::*;

use std::io::{Read as _, Seek, SeekFrom};

#[server(output = StreamingText)]
pub async fn stream_session_events(id: String) -> Result<TextStream, ServerFnError> {
    let home = std::env::var("HOME").expect("HOME not set");
    let base_path = std::path::PathBuf::from(format!("{home}/.claude/projects"));

    let sessions = loader::discover_sessions(&base_path)
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    let session = sessions
        .iter()
        .find(|s| s.id == id)
        .ok_or_else(|| ServerFnError::new(format!("Session {id} not found")))?;
    let path = session.path.clone();

    let initial_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(16);
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            if event.kind.is_modify() {
                let _ = tx.try_send(());
            }
        }
    })
    .map_err(|e| ServerFnError::new(format!("Watcher error: {e}")))?;

    use notify::{RecursiveMode, Watcher};
    watcher
        .watch(&path, RecursiveMode::NonRecursive)
        .map_err(|e| ServerFnError::new(format!("Watch error: {e}")))?;

    let stream = futures::stream::unfold(
        (watcher, rx, path, initial_size, StreamPipelineState::new(DisplayOpts::default())),
        |(watcher, mut rx, path, mut byte_pos, mut state)| async move {
            rx.recv().await?;
            // Drain extra notifications
            while rx.try_recv().is_ok() {}

            let new_size = std::fs::metadata(&path).ok()?.len();
            if new_size <= byte_pos {
                return Some((vec![], (watcher, rx, path, byte_pos, state)));
            }

            let mut file = std::fs::File::open(&path).ok()?;
            file.seek(SeekFrom::Start(byte_pos)).ok()?;
            let mut buf = String::new();
            file.read_to_string(&mut buf).ok()?;
            byte_pos = new_size;

            let mut stream_events = Vec::new();

            for line in buf.lines().filter(|l| !l.is_empty()) {
                if let Ok(raw) = serde_json::from_str::<serde_json::Value>(line) {
                    let parsed = parse_event(&raw);

                    // Check if this event contains tool results to index
                    if let Event::User(u) = &parsed {
                        if let Some(arr) = u.message.get("content").and_then(|c| c.as_array()) {
                            for item in arr {
                                if item.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                                    if let Some(id) = item.get("tool_use_id").and_then(|v| v.as_str()) {
                                        let result = ToolResultData {
                                            output: item.get("content").and_then(|c| c.as_str()).map(|s| s.to_string()),
                                            error: item.get("error").and_then(|e| e.as_str()).map(|s| s.to_string()),
                                            raw: item.clone(),
                                        };
                                        let evt = state.index_tool_result(id.to_string(), result);
                                        if let Ok(json) = serde_json::to_string(&evt) {
                                            stream_events.push(json);
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Convert to display items using state's tool_results index
                    let items = single_event_to_display_items(
                        &parsed, &raw, &state.opts, &state.tool_results,
                    );
                    for (item, mode) in items {
                        if let Some(stream_event) = state.emit(item, mode) {
                            if let Ok(json) = serde_json::to_string(&stream_event) {
                                stream_events.push(json);
                            }
                        }
                    }
                }
            }

            Some((stream_events, (watcher, rx, path, byte_pos, state)))
        },
    );

    use futures::StreamExt;
    let text_stream = stream.flat_map(|batch| futures::stream::iter(batch));

    Ok(TextStream::new(text_stream))
}
```

- [ ] **Step 3: Update session view to handle streaming**

Replace `crates/ccmux-app/src/components/session_view.rs` with a version that loads initial data then subscribes to the SSE stream:

```rust
use ccmux_core::display::DisplayItem;
use ccmux_core::display::streaming::StreamEvent;
use dioxus::prelude::*;

use crate::components::blocks::display_item::DisplayItemView;
use crate::server::sessions::get_session;
use crate::server::streaming::stream_session_events;

#[component]
pub fn SessionView(id: String) -> Element {
    let mut items = use_signal(Vec::<DisplayItem>::new);
    let mut meta = use_signal(|| None);
    let mut loaded = use_signal(|| false);

    // Load initial session data
    let id_clone = id.clone();
    use_future(move || {
        let id = id_clone.clone();
        async move {
            if let Ok(resp) = get_session(id).await {
                meta.set(Some(resp.meta));
                items.set(resp.items);
                loaded.set(true);
            }
        }
    });

    // Subscribe to streaming updates after initial load
    let id_clone2 = id.clone();
    use_future(move || {
        let id = id_clone2.clone();
        async move {
            // Wait for initial load
            // TODO: replace with proper reactive wait
            while !loaded() {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }

            if let Ok(stream) = stream_session_events(id).await {
                // Consume the text stream line by line
                // Each line is a JSON-serialized StreamEvent
                // The exact consumption API depends on how Dioxus TextStream
                // works on the client side — adapt as needed:
                //
                // for line in stream {
                //     if let Ok(event) = serde_json::from_str::<StreamEvent>(&line) {
                //         apply_stream_event(&mut items, event);
                //     }
                // }
            }
        }
    });

    let meta_val = meta.read();

    rsx! {
        div { class: "session-view",
            if let Some(ref m) = *meta_val {
                div { class: "session-header",
                    h2 { {m.first_message.as_deref().unwrap_or(&m.id)} }
                    span { class: "session-meta", "{m.message_count} events" }
                }
            }
            div { class: "display-items",
                for (i, item) in items.read().iter().enumerate() {
                    DisplayItemView { key: "{i}", item: item.clone(), index: i }
                }
            }
        }
    }
}

/// Apply a StreamEvent to the display items list.
fn apply_stream_event(items: &mut Signal<Vec<DisplayItem>>, event: StreamEvent) {
    match event {
        StreamEvent::Append { item } => {
            items.write().push(item);
        }
        StreamEvent::MergeWithPrevious { item } => {
            let mut list = items.write();
            if let Some(last) = list.last_mut() {
                match last {
                    DisplayItem::Group { items: group } => {
                        group.push(item);
                    }
                    _ => {
                        // Convert the last item into a group
                        let prev = std::mem::replace(last, DisplayItem::Other {
                            raw: serde_json::Value::Null,
                        });
                        *last = DisplayItem::Group {
                            items: vec![prev, item],
                        };
                    }
                }
            } else {
                list.push(item);
            }
        }
        StreamEvent::UpdateToolResult { tool_use_id, result } => {
            let mut list = items.write();
            // Search for the ToolUse item with matching id
            for item in list.iter_mut() {
                if update_tool_result_in_item(item, &tool_use_id, &result) {
                    break;
                }
            }
        }
        StreamEvent::UpdateTask { task } => {
            let mut list = items.write();
            // Find the last TaskList and update/add the task
            for item in list.iter_mut().rev() {
                if let DisplayItem::TaskList { tasks, .. } = item {
                    if let Some(existing) = tasks.iter_mut().find(|t| t.id == task.id) {
                        existing.status = task.status;
                        if !task.subject.is_empty() {
                            existing.subject = task.subject;
                        }
                    } else {
                        tasks.push(task);
                    }
                    return;
                }
            }
        }
    }
}

fn update_tool_result_in_item(
    item: &mut DisplayItem,
    tool_use_id: &str,
    result: &ccmux_core::display::ToolResultData,
) -> bool {
    match item {
        DisplayItem::ToolUse { tool_use_id: id, result: res, .. } if id == tool_use_id => {
            *res = Some(result.clone());
            true
        }
        DisplayItem::Group { items } => {
            for child in items.iter_mut() {
                if update_tool_result_in_item(child, tool_use_id, result) {
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}
```

Note: The exact client-side consumption of Dioxus `TextStream` may need adjustment based on the Dioxus 0.6 streaming API. The `apply_stream_event` logic is the important part and is independent of the transport.

- [ ] **Step 4: Verify streaming works**

Run: `cd crates/ccmux-app && dx serve`
Test: Open a session that is actively being used by Claude Code. New events should appear in real-time.

- [ ] **Step 5: Commit**

```bash
git add crates/
git commit -m "feat: implement SSE streaming for live session tailing"
```

---

### Task 9: Delete old frontend and GraphQL code

**Files:**
- Delete: `web/` (entire SolidJS frontend)
- Delete: `src/graphql/` (GraphQL layer)
- Delete: `src/main.rs` (old Axum server)
- Delete: `src/lib.rs`
- Delete: `src/session/` (moved to ccmux-core)
- Delete: `schema.graphql`
- Modify: `CLAUDE.md` (update dev instructions)

- [ ] **Step 1: Delete old code**

```bash
rm -rf web/ src/ schema.graphql
```

- [ ] **Step 2: Update CLAUDE.md**

Replace the Development, GraphQL Schema, and Browser Testing sections with:

```markdown
## Development

```
cd crates/ccmux-app && dx serve   # fullstack dev server
```

Run `cargo clippy --workspace` and `cargo fmt --all` to lint and format after every change.
```

Remove the "GraphQL Schema & TypeScript Codegen" section entirely.

- [ ] **Step 3: Update flake.nix**

Remove `bun` from `devShells.default.packages`. Add `dioxus-cli` (the `dx` command). Update the crane build to target the workspace.

- [ ] **Step 4: Verify clean build**

Run: `cargo check --workspace && cargo clippy --workspace -- -D warnings`
Expected: Workspace compiles cleanly with no references to deleted code.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "chore: remove SolidJS frontend, GraphQL layer, and old Axum server"
```

---

### Task 10: Final integration testing and polish

**Files:**
- Various minor fixes discovered during testing

- [ ] **Step 1: Run full test suite**

Run: `cargo test --workspace`
Expected: All tests pass.

- [ ] **Step 2: Visual verification with Rodney**

```bash
cd crates/ccmux-app && dx serve &
rodney start
rodney open http://localhost:8080
rodney screenshot session-list.png
# Click first session
rodney click '.session-card'
rodney waitstable
rodney screenshot session-view.png
rodney stop
```

Review screenshots for: session list grouping, session view rendering, message blocks, tool use blocks, thinking blocks, task lists.

- [ ] **Step 3: Fix any visual/functional issues found**

Address anything broken discovered in step 2.

- [ ] **Step 4: Run clippy and fmt one final time**

Run: `cargo clippy --workspace -- -D warnings && cargo fmt --all`
Expected: Clean.

- [ ] **Step 5: Commit any fixes**

```bash
git add -A
git commit -m "fix: address issues found during integration testing"
```
