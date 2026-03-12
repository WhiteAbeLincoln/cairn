# Post-Migration UI/UX Fixup Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bring the Dioxus 0.7 app to functional and visual parity with the pre-migration SolidJS app.

**Architecture:** Three-phase approach — backend metadata + shared frontend infrastructure (Phase 1), then tool formatters and feature work (Phase 2), then visual polish (Phase 3). Backend changes are in `ccmux-core`, frontend in `ccmux-app`.

**Tech Stack:** Rust, Dioxus 0.7 fullstack, pulldown-cmark, chrono, syntect (Phase 3)

**Spec:** `docs/superpowers/specs/2026-03-12-post-migration-fixup-design.md`

**Old source reference:** All tasks should consult the old SolidJS source at commit `5e14bad` as the source of truth. Use `git show 5e14bad:<path>` to read files. See the spec's "Old Source Reference" table for the full file list.

---

## File Structure

### Phase 1 files

| File | Action | Responsibility |
|------|--------|---------------|
| `crates/ccmux-core/src/display/mod.rs` | Modify | Add `ItemMeta` struct, add `meta` field to `DisplayItem` variants |
| `crates/ccmux-core/src/display/pipeline.rs` | Modify | Extract metadata from events, compute group aggregates |
| `crates/ccmux-core/src/session/loader.rs` | Modify | First message cleanup, project path unescaping |
| `crates/ccmux-app/src/server_fns.rs` | Modify | Server-side grouping + sorting, use `project_path` for display |
| `crates/ccmux-app/src/components/session_list.rs` | Modify | Consume server-sorted groups, collapsible headers, count badges |
| `crates/ccmux-app/src/components/session_view.rs` | Modify | Session title, back button, Raw button, scroll FAB, raw mode context |
| `crates/ccmux-app/src/components/blocks/message.rs` | Modify | Full block + minimal row modes, metadata display, raw toggle, kebab menu |
| `crates/ccmux-app/src/components/blocks/display_item.rs` | Modify | Pass metadata through, use GroupBlock for Group variant |
| `crates/ccmux-app/src/components/blocks/group.rs` | Create | Collapsed summary + expanded view with child rows |
| `crates/ccmux-app/src/components/blocks/mod.rs` | Modify | Add `group` module |
| `crates/ccmux-app/assets/style.css` | Modify | Group block styles, kebab menu, FAB, responsive breakpoints |

### Phase 2 files

| File | Action | Responsibility |
|------|--------|---------------|
| `crates/ccmux-app/src/components/blocks/tool_use.rs` | Modify | Dispatcher pattern, delegates to tool-specific components |
| `crates/ccmux-app/src/components/blocks/tools/mod.rs` | Create | Module declarations for tool-specific components |
| `crates/ccmux-app/src/components/blocks/tools/bash.rs` | Create | Bash command display with description, ANSI stripping |
| `crates/ccmux-app/src/components/blocks/tools/read.rs` | Create | Read file display, line number stripping, image rendering |
| `crates/ccmux-app/src/components/blocks/tools/edit.rs` | Create | Edit diff view (old → new), replace_all badge |
| `crates/ccmux-app/src/components/blocks/tools/grep.rs` | Create | Grep output parser + file-grouped display |
| `crates/ccmux-app/src/components/blocks/tools/write.rs` | Create | Write file path + content display |
| `crates/ccmux-app/src/components/blocks/tools/glob.rs` | Create | Glob pattern + file list |
| `crates/ccmux-app/src/components/blocks/tools/tool_search.rs` | Create | ToolSearch badges |
| `crates/ccmux-app/src/components/blocks/tools/web_search.rs` | Create | WebSearch links + summary |
| `crates/ccmux-app/src/components/blocks/tools/agent.rs` | Create | Agent subagent link + prose output |
| `crates/ccmux-app/src/components/blocks/tools/ask_user.rs` | Create | AskUserQuestion with answer |
| `crates/ccmux-core/src/display/format.rs` | Create | Shared formatting utilities: strip_ansi, strip_read_line_numbers, parse_grep_output |
| `crates/ccmux-app/assets/style.css` | Modify | Tool-specific styles |

### Phase 3 files

| File | Action | Responsibility |
|------|--------|---------------|
| `crates/ccmux-app/src/components/theme_toggle.rs` | Create | Theme selector dropdown, localStorage persistence |
| `crates/ccmux-app/src/components/mod.rs` | Modify | Add `theme_toggle` module |
| `crates/ccmux-app/src/components/app.rs` | Modify | Include ThemeToggle in nav |
| `crates/ccmux-app/assets/style.css` | Modify | CSS custom properties, light/dark themes, typography, block styling overhaul |
| `crates/ccmux-core/Cargo.toml` | Modify | Add syntect dependency |
| `crates/ccmux-core/src/display/highlight.rs` | Create | Server-side syntax highlighting with syntect |
| `crates/ccmux-core/src/display/mod.rs` | Modify | Add `highlight` module |

---

## Chunk 1: Backend Foundation (Tasks 1-3)

### Task 1: Add ItemMeta to DisplayItem

**Files:**
- Modify: `crates/ccmux-core/src/display/mod.rs`
- Modify: `crates/ccmux-core/src/display/pipeline.rs`
- Modify: `crates/ccmux-core/src/display/streaming.rs` (tests only — StreamEvent carries DisplayItem, no struct change)

**Old source reference:** Read `git show 5e14bad:web/src/components/blocks/DisplayItemView.tsx` and `git show 5e14bad:web/src/components/SessionView.tsx` to see how the old app extracted model, tokens, and UUID from events.

- [ ] **Step 1: Add ItemMeta struct and update DisplayItem variants in mod.rs**

Add the `ItemMeta` struct and add `meta: ItemMeta` to `UserMessage`, `AssistantMessage`, `Thinking`, `ToolUse`, `TaskList`, `TurnDuration`, `Compaction`, and `Group`. Keep `Other` without meta.

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ItemMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
}
```

Each variant gains `meta: ItemMeta`. Example for `UserMessage`:
```rust
UserMessage {
    content: String,
    meta: ItemMeta,
    raw: Value,
},
```

- [ ] **Step 2: Update pipeline.rs to extract and attach metadata**

In `assistant_event_items()`, extract metadata from the event data:
```rust
let model = data.message.get("model").and_then(|v| v.as_str()).map(|s| s.to_string());
let tokens = data.message.pointer("/usage/output_tokens").and_then(|v| v.as_u64());
let uuid = Some(data.core.uuid.clone());
let meta = ItemMeta { uuid, model, tokens };
```

Attach `meta.clone()` to each `DisplayItem` emitted from that function (AssistantMessage, Thinking, ToolUse, TaskList).

In `user_event_items()`, extract UUID only:
```rust
let meta = ItemMeta { uuid: Some(data.core.uuid.clone()), ..Default::default() };
```

In `system_event_items()`, extract UUID:
```rust
let meta = ItemMeta { uuid: Some(data.core.uuid.clone()), ..Default::default() };
```

- [ ] **Step 3: Update flush_grouped() to compute aggregate meta for Group**

```rust
fn flush_grouped(acc: &mut Vec<DisplayItem>, output: &mut Vec<DisplayItem>) {
    if acc.is_empty() {
        return;
    }
    if acc.len() == 1 {
        output.push(acc.remove(0));
    } else {
        let meta = aggregate_meta(&acc);
        output.push(DisplayItem::Group {
            items: std::mem::take(acc),
            meta,
        });
    }
}

fn aggregate_meta(items: &[DisplayItem]) -> ItemMeta {
    let mut total_tokens: u64 = 0;
    let mut model = None;
    for item in items {
        if let Some(m) = item_meta(item) {
            if model.is_none() {
                model = m.model.clone();
            }
            if let Some(t) = m.tokens {
                total_tokens += t;
            }
        }
    }
    ItemMeta {
        uuid: None,
        model,
        tokens: if total_tokens > 0 { Some(total_tokens) } else { None },
    }
}

fn item_meta(item: &DisplayItem) -> Option<&ItemMeta> {
    match item {
        DisplayItem::UserMessage { meta, .. }
        | DisplayItem::AssistantMessage { meta, .. }
        | DisplayItem::Thinking { meta, .. }
        | DisplayItem::ToolUse { meta, .. }
        | DisplayItem::TaskList { meta, .. }
        | DisplayItem::TurnDuration { meta, .. }
        | DisplayItem::Compaction { meta, .. }
        | DisplayItem::Group { meta, .. } => Some(meta),
        DisplayItem::Other { .. } => None,
    }
}
```

Similarly update `flush_tasks()` to include meta.

- [ ] **Step 4: Fix all existing tests in pipeline.rs**

Update every test that constructs or matches `DisplayItem` variants to include `meta: ItemMeta::default()` or the appropriate expected metadata. The tests should still pass with the same assertions but now include the meta field.

- [ ] **Step 5: Run tests to verify everything passes**

Run: `cargo test -p ccmux-core`
Expected: All tests pass.

- [ ] **Step 6: Fix all files that destructure or construct DisplayItem**

Update `display_item.rs`, `session_view.rs`, and any other files that pattern-match on `DisplayItem` variants to include the new `meta` field in their match patterns. The frontend doesn't use `meta` yet — just add `meta, ..` or `meta: _` to the destructure patterns so they compile.

**Critical:** `session_view.rs::apply_stream_event` also **constructs** `DisplayItem::Group` at line 25. This must be updated to include the `meta` field:
```rust
DisplayItem::Group {
    items: Vec::with_capacity(2),
    meta: ItemMeta::default(),
},
```

When extending an existing Group (pushing a new item), recompute the aggregate meta, or use `ItemMeta::default()` as a placeholder (the group meta will be approximate during streaming but correct on next full load).

- [ ] **Step 7: Run clippy and format**

Run: `cargo clippy --workspace && cargo fmt --all`
Expected: No errors or warnings.

- [ ] **Step 8: Commit**

```bash
git add -A && git commit -m "Add ItemMeta to DisplayItem with model, tokens, and UUID extraction"
```

---

### Task 2: Backend Session List Fixes

**Files:**
- Modify: `crates/ccmux-core/src/session/loader.rs`
- Modify: `crates/ccmux-app/src/server_fns.rs`

**Old source reference:** Read `git show 5e14bad:web/src/components/SessionList.tsx` for grouping/sorting logic. Read `git show 5e14bad:web/src/lib/session.ts` for message filtering.

- [ ] **Step 1: Write test for first_message filtering in loader.rs**

Add a test to `loader.rs::tests` that creates a session JSONL with:
1. A `local-command-caveat` user event (should be skipped)
2. A tool_result user event (should be skipped)
3. A real external user message "hello world" (should be selected)

Assert that `first_message` is `Some("hello world")`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ccmux-core test_first_message_skips_local_commands`
Expected: FAIL — current logic picks the first user message without filtering.

- [ ] **Step 3: Implement first_message filtering in scan_session_metadata()**

Update the first_message extraction in `scan_session_metadata()`:

```rust
// Extract first *real* user message (not CLI-generated, not tool results)
if first_message.is_none()
    && line.contains("\"type\":\"user\"")
    && line.contains("\"userType\":\"external\"")
    && !line.contains("\"toolUseResult\"")
    && !line.contains("\"sourceToolAssistantUUID\"")
    && let Some(text) = extract_user_content_string(&line)
    && !text.starts_with("<local-command")
    && !text.starts_with("<local_command")
    // Note: "role":"user" is implicitly satisfied by the "type":"user" event type check
{
    // Strip XML tags and truncate
    let cleaned = strip_xml_tags(&text);
    let truncated = if cleaned.len() > 200 {
        format!("{}...", &cleaned[..cleaned.floor_char_boundary(200)])
    } else {
        cleaned
    };
    first_message = Some(truncated);
}
```

Add the `strip_xml_tags` helper:
```rust
fn strip_xml_tags(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        if ch == '<' {
            in_tag = true;
        } else if ch == '>' {
            in_tag = false;
        } else if !in_tag {
            result.push(ch);
        }
    }
    result.trim().to_string()
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ccmux-core test_first_message_skips_local_commands`
Expected: PASS

- [ ] **Step 5: Write test for project path unescaping**

Add a test for a `unescape_project_name` function in `loader.rs`:
```rust
#[test]
fn test_unescape_project_name() {
    // With project_path available (preferred path)
    assert_eq!(
        unescape_project_name("-Users-abe-Projects-ccmux", Some("/Users/abe/Projects/ccmux")),
        "/Users/abe/Projects/ccmux"
    );
    // With project_path for dash-containing directories
    assert_eq!(
        unescape_project_name("-Users-abe-my-cool-project", Some("/Users/abe/my-cool-project")),
        "/Users/abe/my-cool-project"
    );
    // Without project_path — best-effort, inherently lossy for dash-containing dir names
    assert_eq!(
        unescape_project_name("-Users-abe-Projects-ccmux", None),
        "/Users/abe/Projects/ccmux"
    );
}
```

- [ ] **Step 6: Implement unescape_project_name() in loader.rs**

```rust
/// Convert a dash-escaped project directory name to a real path.
/// Prefers `project_path` (from event cwd) if available — this is the only
/// reliable mechanism. The dash-based heuristic is best-effort and inherently
/// lossy for directory names containing dashes (e.g., "my-project").
pub fn unescape_project_name(dir_name: &str, project_path: Option<&str>) -> String {
    if let Some(path) = project_path {
        return path.to_string();
    }
    // Best-effort: leading dash, then segments separated by dashes -> slashes
    if dir_name.starts_with('-') {
        dir_name.replacen('-', "/", 1).replace('-', "/")
    } else {
        dir_name.to_string()
    }
}
```

**Important:** The dash heuristic is lossy — `/Users/abe/my-project` and `/Users/abe/my/project` both produce `-Users-abe-my-project`. Always prefer `project_path` from the event `cwd` field. The heuristic only runs when `project_path` is `None` (e.g., empty session files).

- [ ] **Step 7: Run test to verify it passes**

Run: `cargo test -p ccmux-core test_unescape_project_name`
Expected: PASS

- [ ] **Step 8: Apply project path unescaping in SessionInfo construction**

In `discover_sessions()`, when building `SessionInfo`, set the `project` field using the unescaped name:
```rust
let display_project = unescape_project_name(&project_name, meta.project_path.as_deref());
// ... use display_project instead of project_name in SessionInfo
```

- [ ] **Step 9: Update server_fns.rs to return sorted grouped sessions (must be done atomically with Step 10)**

Change `list_sessions()` to return sessions pre-grouped and sorted. Replace `Vec<SessionMeta>` return type with a new wire type:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectGroup {
    pub project: String,
    pub sessions: Vec<SessionMeta>,
}
```

In `list_sessions()`, group sessions by project, sort groups by most recent `updated_at`, and return `Vec<ProjectGroup>`.

- [ ] **Step 10: Update session_list.rs to consume ProjectGroup**

Replace the `BTreeMap` grouping with direct iteration over `Vec<ProjectGroup>` from the server.

- [ ] **Step 11: Run clippy and format, then commit**

Run: `cargo clippy --workspace && cargo fmt --all`

```bash
git add -A && git commit -m "Fix session list: filter first message, unescape project paths, sort by recency"
```

---

### Task 3: Frontend Block Header Rework (MessageBlock)

**Files:**
- Modify: `crates/ccmux-app/src/components/blocks/message.rs`
- Modify: `crates/ccmux-app/src/components/blocks/display_item.rs`
- Modify: `crates/ccmux-app/src/components/session_view.rs`
- Modify: `crates/ccmux-app/assets/style.css`

**Old source reference:** Read `git show 5e14bad:web/src/components/blocks/MessageBlock.tsx`, `git show 5e14bad:web/src/components/blocks/MessageBlock.module.css`, and `git show 5e14bad:web/src/components/blocks/CollapsibleBlock.module.css` to understand the two rendering modes and styling.

- [ ] **Step 1: Add raw mode context signal to session_view.rs**

Add a global raw mode signal using Dioxus context:

```rust
use dioxus::prelude::*;

#[derive(Clone, Copy)]
pub struct RawModeContext {
    pub global_raw: Signal<bool>,
}
```

Provide this context in `SessionView`:
```rust
let global_raw = use_signal(|| false);
use_context_provider(|| RawModeContext { global_raw });
```

Add a "Raw" button in the session header that toggles `global_raw`.

- [ ] **Step 2: Rewrite MessageBlock with two rendering modes**

The `MessageBlock` component should accept these props:

```rust
#[component]
pub fn MessageBlock(
    label: String,
    border_class: String,
    #[props(default)] extra_label: Option<String>,
    #[props(default)] meta: Option<ItemMeta>,
    #[props(default)] raw: Option<Value>,
    #[props(default = true)] collapsible: bool,
    #[props(default = true)] default_open: bool,
    #[props(default = false)] minimal: bool,
    children: Element,
) -> Element
```

Note: If `#[props(default = true)]` doesn't compile in Dioxus 0.7, use `#[props(default)]` with a manual Default impl or use `Option<bool>` with `.unwrap_or(true)` in the body. Check Dioxus 0.7 docs for the correct attribute syntax.

**Minimal mode** (`minimal: true`): Single-line row, clicking expands to full mode.

**Full mode** (`minimal: false`): Left border, header with label + extra_label + metadata (model, tokens formatted as "N tok", short UUID) + action buttons (raw `{}`, collapse `^`/`v` if `collapsible`), expandable body.

**Raw toggle**: Read `RawModeContext` from context. Block shows raw JSON if `global_raw` OR local `raw_open` signal is true. `{}` button toggles the local override.

**Kebab menu**: Below 768px, wrap raw toggle and collapse toggle in a `⋮` dropdown. Use a `show_menu` signal and CSS for positioning.

Refer to the old source for exact layout structure and CSS class naming.

- [ ] **Step 3: Update display_item.rs to pass metadata and raw values through**

Update each `DisplayItem` match arm to pass `meta`, `raw`, and appropriate `collapsible`/`extra_label` props to `MessageBlock`. User and Assistant messages get `collapsible: false`.

- [ ] **Step 4: Add CSS for the new block header**

Add styles to `style.css` for:
- `.message-header` with flexbox layout (left side: label + extra, right side: metadata + actions)
- `.message-meta` for model, token count, UUID
- `.kebab-menu` for the responsive dropdown
- `.raw-json-view` for the raw JSON pre block
- `@media (max-width: 768px)` to hide direct action buttons and show kebab

Refer to the old CSS files for spacing, colors, and layout patterns.

- [ ] **Step 5: Add back button and session title to session_view.rs**

Change the session header from project name + item count to:
- `← Back` button (Link to `Route::SessionList {}`)
- `Session <first-6-chars-of-id>` title
- `Raw` toggle button

Read `git show 5e14bad:web/src/components/SessionView.tsx` for the exact header layout.

- [ ] **Step 6: Run clippy and format, verify the app compiles**

Run: `cargo clippy --workspace && cargo fmt --all`
Run: `cd crates/ccmux-app && dx build` (or `cargo build --workspace`)
Expected: Compiles without errors.

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "Rework MessageBlock with full/minimal modes, raw toggle, and kebab menu"
```

---

## Chunk 2: Group Block and Session List UI (Tasks 4-5)

### Task 4: Group Block Component

**Files:**
- Create: `crates/ccmux-app/src/components/blocks/group.rs`
- Modify: `crates/ccmux-app/src/components/blocks/mod.rs`
- Modify: `crates/ccmux-app/src/components/blocks/display_item.rs`
- Modify: `crates/ccmux-app/assets/style.css`

**Old source reference:** Read `git show 5e14bad:web/src/components/SessionView.tsx` for the internal-group rendering logic and step summary generation. Read `git show 5e14bad:web/src/components/blocks/ToolUseBlockView.tsx` for the `toolExtraLabel` function. Read `git show 5e14bad:web/src/components/blocks/CollapsibleBlock.module.css` for styling.

- [ ] **Step 1: Create group.rs with summary generation logic**

Implement `GroupBlock` component:

```rust
#[component]
pub fn GroupBlock(items: Vec<DisplayItem>, meta: ItemMeta) -> Element
```

**Summary generation:**
1. Walk items, track first-seen order of `(kind_label, name)` pairs using `IndexMap` or a Vec + HashSet
2. Count occurrences of each
3. Extract extra label from the *last* occurrence of each type (using `tool_extra_label()` helper)
4. Render in first-seen order with `·` separators

**Collapsed state (default):** `▸` + summary + aggregate meta on right.
**Expanded state:** `▾` + list of children, each rendered as `MessageBlock` in minimal mode.

Implement `tool_extra_label(name: &str, input: &Value) -> Option<String>` helper that extracts the contextual label per tool type. Read the old `toolExtraLabel` function for the complete mapping:
- Read/Write: `file_path`
- Bash: `description`
- Grep/Glob: `pattern`
- Agent: `subagent_type · description`
- WebSearch/ToolSearch: `query`
- Edit: `file_path`

- [ ] **Step 2: Register group module and wire into display_item.rs**

Add `pub mod group;` to `blocks/mod.rs`.

In `display_item.rs`, change the `Group` match arm from the flat div to:
```rust
DisplayItem::Group { items, meta, .. } => rsx! {
    GroupBlock { items, meta }
},
```

- [ ] **Step 3: Add CSS for group block**

Add styles for:
- `.group-block` — container
- `.group-summary` — collapsed row with flex layout
- `.group-summary-item` — individual summary entries
- `.step-dot` — `·` separator
- `.group-count` — `×N` count badge
- `.group-expanded` — expanded state container

Refer to `5e14bad:web/src/components/blocks/CollapsibleBlock.module.css`.

- [ ] **Step 4: Run clippy, format, and compile check**

Run: `cargo clippy --workspace && cargo fmt --all`
Expected: Compiles.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "Add GroupBlock with collapsed summary and expanded row view"
```

---

### Task 5: Session List UI Fixes

**Files:**
- Modify: `crates/ccmux-app/src/components/session_list.rs`
- Modify: `crates/ccmux-app/assets/style.css`

**Old source reference:** Read `git show 5e14bad:web/src/components/SessionList.tsx` and `git show 5e14bad:web/src/components/SessionList.module.css` for the collapsible project groups, session count badge, date formatting, and flat row styling.

- [ ] **Step 1: Add collapsible project groups with session count**

Add a `collapsed` signal per project group. Use a triangle toggle (`▸`/`▾`). Show session count badge next to project name: `ProjectName (23)`.

```rust
// Inside the project group rendering:
let mut collapsed = use_signal(|| false);
let count = sessions.len();
rsx! {
    div { class: "project-group",
        div {
            class: "project-header",
            onclick: move |_| collapsed.toggle(),
            span { class: "project-toggle",
                if collapsed() { "▸" } else { "▾" }
            }
            h2 { class: "project-name", "{project}" }
            span { class: "session-count-badge", "{count}" }
        }
        if !collapsed() {
            // ... session cards
        }
    }
}
```

- [ ] **Step 2: Update date formatting**

Change the date format from `%Y-%m-%d %H:%M` to US-style with chrono:
```rust
let updated = session
    .updated_at
    .map(|dt| {
        let local = dt.with_timezone(&chrono::Local);
        local.format("%-m/%-d/%Y, %-I:%M:%S %p").to_string()
    })
    .unwrap_or_else(|| "unknown".to_string());
```

Note: This requires `chrono::Local` which may need the `clock` feature. Check if it's already available; if not, add it to `ccmux-app/Cargo.toml`: `chrono = { version = "0.4", features = ["serde", "clock"] }`.

- [ ] **Step 3: Update CSS for flat row style**

Replace the current rounded card styles with flat list rows:
- Remove `border-radius` and heavy `border` from `.session-card`
- Add `border-bottom: 1px solid var(--border)` for subtle dividers
- Adjust padding for a more compact list feel

Refer to the old `SessionList.module.css`.

- [ ] **Step 4: Run clippy, format, and compile check**

Run: `cargo clippy --workspace && cargo fmt --all`

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "Fix session list: collapsible groups, count badges, date format, flat rows"
```

---

## Chunk 3: Tool Formatters — Tier 1 (Tasks 6-8)

### Task 6: Tool Formatter Infrastructure + Bash

**Files:**
- Create: `crates/ccmux-core/src/display/format.rs`
- Modify: `crates/ccmux-core/src/display/mod.rs`
- Create: `crates/ccmux-app/src/components/blocks/tools/mod.rs`
- Create: `crates/ccmux-app/src/components/blocks/tools/bash.rs`
- Modify: `crates/ccmux-app/src/components/blocks/tool_use.rs`
- Modify: `crates/ccmux-app/src/components/blocks/mod.rs`
- Modify: `crates/ccmux-app/assets/style.css`

**Old source reference:** Read `git show 5e14bad:web/src/components/blocks/ToolUseBlockView.tsx` (BashView function). Read `git show 5e14bad:web/src/lib/format.ts` for `stripAnsi` and `parseToolResultParts`.

- [ ] **Step 1: Write test for strip_ansi in format.rs**

Create `crates/ccmux-core/src/display/format.rs`:

```rust
/// Strip ANSI escape codes from a string.
pub fn strip_ansi(s: &str) -> String { todo!() }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_ansi() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m"), "red");
        assert_eq!(strip_ansi("no ansi here"), "no ansi here");
        assert_eq!(strip_ansi("\x1b[1;32mbold green\x1b[0m"), "bold green");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ccmux-core test_strip_ansi`
Expected: FAIL

- [ ] **Step 3: Implement strip_ansi**

```rust
pub fn strip_ansi(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // Skip escape sequence
            if chars.peek() == Some(&'[') {
                chars.next(); // consume '['
                // consume until we hit a letter (the terminator)
                while let Some(&c) = chars.peek() {
                    chars.next();
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            result.push(ch);
        }
    }
    result
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ccmux-core test_strip_ansi`
Expected: PASS

- [ ] **Step 5: Add format module to display/mod.rs**

Add `pub mod format;` to `crates/ccmux-core/src/display/mod.rs`.

- [ ] **Step 6: Create tools module and bash.rs**

Create `crates/ccmux-app/src/components/blocks/tools/mod.rs`:
```rust
pub mod bash;
```

Create `crates/ccmux-app/src/components/blocks/tools/bash.rs`:

Read `git show 5e14bad:web/src/components/blocks/ToolUseBlockView.tsx` — find the `BashView` function and port its logic to Rust/Dioxus. The component should:

- Extract `command` and `description` from input JSON
- Display command with `$ ` prefix in a `<pre>` block
- Display result: strip ANSI codes, separate stdout/stderr, show error badge if `is_error`

```rust
#[component]
pub fn BashView(input: Value, result: Option<ToolResultData>) -> Element
```

- [ ] **Step 7: Create dispatcher in tool_use.rs**

Rewrite `ToolUseBlock` to dispatch based on tool name:

```rust
#[component]
pub fn ToolUseBlock(name: String, input: Value, result: Option<ToolResultData>, meta: ItemMeta, raw: Value) -> Element {
    match name.as_str() {
        "Bash" => rsx! { tools::bash::BashView { input, result } },
        // ... more tools added in later tasks
        _ => rsx! { GenericToolView { name, input, result } },
    }
}
```

Keep the current generic view as `GenericToolView` fallback.

The `MessageBlock` wrapping should happen inside `ToolUseBlock` itself (not in `display_item.rs`). `ToolUseBlock` is responsible for creating the `MessageBlock` with the right label, extra_label (from `tool_extra_label`), meta, raw, and collapsibility props based on the tool name. The tool-specific view (BashView, ReadView, etc.) renders only the content that goes inside the block body. This mirrors the old SolidJS pattern where `DisplayItemView` wrapped tool blocks in `MessageBlock`. Copy the relevant class definitions from the old CSS modules and adapt selectors from `.styles.className` to plain `.className`.

- [ ] **Step 8: Update blocks/mod.rs and add CSS**

Add `pub mod tools;` to `blocks/mod.rs`.

Add CSS for:
- `.bash-command` — styled `<pre>` with prompt
- `.bash-output` — result container
- `.bash-error` — error badge styling

Refer to `5e14bad:web/src/components/blocks/ToolUseBlockView.module.css`.

- [ ] **Step 9: Run clippy, format, and compile check**

Run: `cargo clippy --workspace && cargo fmt --all`

- [ ] **Step 10: Commit**

```bash
git add -A && git commit -m "Add tool formatter infrastructure and Bash-specific view"
```

---

### Task 7: Read Tool Formatter

**Files:**
- Modify: `crates/ccmux-core/src/display/format.rs`
- Create: `crates/ccmux-app/src/components/blocks/tools/read.rs`
- Modify: `crates/ccmux-app/src/components/blocks/tools/mod.rs`
- Modify: `crates/ccmux-app/src/components/blocks/tool_use.rs`
- Modify: `crates/ccmux-app/assets/style.css`

**Old source reference:** Read `git show 5e14bad:web/src/components/blocks/ToolUseBlockView.tsx` (ReadView function). Read `git show 5e14bad:web/src/lib/format.ts` for `stripReadLineNumbers`.

- [ ] **Step 1: Write test for strip_read_line_numbers**

In `format.rs`:
```rust
#[test]
fn test_strip_read_line_numbers() {
    let input = "     1\tline one\n     2\tline two\n    10\tline ten";
    let expected = "line one\nline two\nline ten";
    assert_eq!(strip_read_line_numbers(input), expected);
}
```

- [ ] **Step 2: Run test, verify fail**

Run: `cargo test -p ccmux-core test_strip_read_line_numbers`

- [ ] **Step 3: Implement strip_read_line_numbers**

```rust
/// Strip the `    N\t` line number prefixes added by the Read tool.
pub fn strip_read_line_numbers(s: &str) -> String {
    s.lines()
        .map(|line| {
            // Pattern: optional spaces, digits, tab, then content
            if let Some(idx) = line.find('\t') {
                let prefix = &line[..idx];
                if prefix.trim().chars().all(|c| c.is_ascii_digit()) {
                    return &line[idx + 1..];
                }
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}
```

Read the old `stripReadLineNumbers` in `format.ts` to verify the regex pattern matches.

- [ ] **Step 4: Run test, verify pass**

Run: `cargo test -p ccmux-core test_strip_read_line_numbers`

- [ ] **Step 5: Create read.rs component**

Read `git show 5e14bad:web/src/components/blocks/ToolUseBlockView.tsx` — find the `ReadView` function and port it. The component should:

- Extract `file_path` from input
- Strip line numbers from result output
- Detect image reads: check if the tool result's raw field contains content blocks with `type: "image"` — if so, render the base64 data as an `<img>` tag
- Otherwise display stripped text content in a `<pre>` block

```rust
#[component]
pub fn ReadView(input: Value, result: Option<ToolResultData>) -> Element
```

- [ ] **Step 6: Wire into tool_use.rs dispatcher**

Add `"Read" => rsx! { tools::read::ReadView { input, result } }` to the match.

- [ ] **Step 7: Add CSS for read view**

Refer to old source for styling.

- [ ] **Step 8: Run clippy, format, commit**

Run: `cargo clippy --workspace && cargo fmt --all`

```bash
git add -A && git commit -m "Add Read tool formatter with line number stripping and image support"
```

---

### Task 8: Edit Tool Formatter

**Files:**
- Create: `crates/ccmux-app/src/components/blocks/tools/edit.rs`
- Modify: `crates/ccmux-app/src/components/blocks/tools/mod.rs`
- Modify: `crates/ccmux-app/src/components/blocks/tool_use.rs`
- Modify: `crates/ccmux-app/assets/style.css`

**Old source reference:** Read `git show 5e14bad:web/src/components/blocks/ToolUseBlockView.tsx` (EditView function). Read `git show 5e14bad:web/src/components/blocks/EditBlockView.module.css`.

- [ ] **Step 1: Create edit.rs component**

Read the old `EditView` function and port it. The component should:

- Extract `file_path`, `old_string`, `new_string`, `replace_all` from input
- Show file path as header context
- Display old string with deletion styling (red background)
- Display new string with addition styling (green background)
- Show "all" badge if `replace_all` is true

```rust
#[component]
pub fn EditView(input: Value, result: Option<ToolResultData>) -> Element
```

- [ ] **Step 2: Wire into dispatcher and add CSS**

Add to tool_use.rs dispatcher. Add CSS for:
- `.edit-diff` container
- `.edit-old` — red/deletion styling
- `.edit-new` — green/addition styling
- `.edit-badge` — "all" badge

Refer to `5e14bad:web/src/components/blocks/EditBlockView.module.css`.

- [ ] **Step 3: Run clippy, format, commit**

Run: `cargo clippy --workspace && cargo fmt --all`

```bash
git add -A && git commit -m "Add Edit tool formatter with diff view"
```

---

## Chunk 4: Tool Formatters — Tiers 2 & 3 (Tasks 9-11)

### Task 9: Grep, Write, Glob Formatters

**Files:**
- Modify: `crates/ccmux-core/src/display/format.rs`
- Create: `crates/ccmux-app/src/components/blocks/tools/grep.rs`
- Create: `crates/ccmux-app/src/components/blocks/tools/write.rs`
- Create: `crates/ccmux-app/src/components/blocks/tools/glob.rs`
- Modify: `crates/ccmux-app/src/components/blocks/tools/mod.rs`
- Modify: `crates/ccmux-app/src/components/blocks/tool_use.rs`
- Modify: `crates/ccmux-app/assets/style.css`

**Old source reference:** Read `git show 5e14bad:web/src/components/blocks/ToolUseBlockView.tsx` (GrepView, WriteView, GlobView). Read `git show 5e14bad:web/src/lib/grep-parse.ts` for the grep output parser.

- [ ] **Step 1: Write test for grep output parser in format.rs**

```rust
#[test]
fn test_parse_grep_output() {
    let output = "src/main.rs:10:fn main() {\nsrc/main.rs:11:    println!(\"hello\");\nsrc/lib.rs:5:pub fn foo() {";
    let groups = parse_grep_output(output);
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].file, "src/main.rs");
    assert_eq!(groups[0].lines.len(), 2);
    assert_eq!(groups[1].file, "src/lib.rs");
}
```

- [ ] **Step 2: Run test, verify fail**

- [ ] **Step 3: Implement parse_grep_output**

Port the logic from `5e14bad:web/src/lib/grep-parse.ts`. Parse rg output format: `file:line:content` or `file-line-content` for context lines.

```rust
pub struct GrepGroup {
    pub file: String,
    pub lines: Vec<GrepLine>,
}
pub struct GrepLine {
    pub line_num: Option<u32>,
    pub content: String,
    pub is_match: bool,
}
pub fn parse_grep_output(output: &str) -> Vec<GrepGroup> { /* ... */ }
```

- [ ] **Step 4: Run test, verify pass**

- [ ] **Step 5: Create grep.rs, write.rs, glob.rs components**

For each, read the old source's corresponding View function and port to Dioxus:

**grep.rs:** Display groups of matches by file with line numbers and highlighted match lines.
**write.rs:** Display file path and content.
**glob.rs:** Display pattern and matched file list.

- [ ] **Step 6: Wire all three into dispatcher and add CSS**

- [ ] **Step 7: Run clippy, format, commit**

```bash
git add -A && git commit -m "Add Grep, Write, and Glob tool formatters"
```

---

### Task 10: ToolSearch, WebSearch, Agent, AskUserQuestion Formatters

**Files:**
- Create: `crates/ccmux-app/src/components/blocks/tools/tool_search.rs`
- Create: `crates/ccmux-app/src/components/blocks/tools/web_search.rs`
- Create: `crates/ccmux-app/src/components/blocks/tools/agent.rs`
- Create: `crates/ccmux-app/src/components/blocks/tools/ask_user.rs`
- Modify: `crates/ccmux-app/src/components/blocks/tools/mod.rs`
- Modify: `crates/ccmux-app/src/components/blocks/tool_use.rs`
- Modify: `crates/ccmux-app/assets/style.css`

**Old source reference:** Read `git show 5e14bad:web/src/components/blocks/ToolUseBlockView.tsx` for ToolSearchView, WebSearchView, AgentView, AskUserQuestionView. Read `git show 5e14bad:web/src/components/blocks/AgentBlockView.module.css`.

- [ ] **Step 1: Create all four components**

For each, read the old source's corresponding View function and port:

**tool_search.rs:** Query display, result as tool name badges with count of deferred tools.
**web_search.rs:** Query display, links list, collapsible summary.
**agent.rs:** Subagent type + description, link to subagent session (`/session/<agent-id>`), output rendered as Prose.
**ask_user.rs:** Structured question display, user's answer from tool result.

- [ ] **Step 2: Wire all four into dispatcher**

- [ ] **Step 3: Add CSS for all four tools**

- [ ] **Step 4: Run clippy, format, commit**

```bash
git add -A && git commit -m "Add ToolSearch, WebSearch, Agent, and AskUserQuestion formatters"
```

---

### Task 11: Collapsibility Defaults + Jump-to-Bottom FAB

**Files:**
- Modify: `crates/ccmux-app/src/components/blocks/display_item.rs`
- Modify: `crates/ccmux-app/src/components/blocks/tool_use.rs`
- Modify: `crates/ccmux-app/src/components/session_view.rs`
- Modify: `crates/ccmux-app/assets/style.css`

**Old source reference:** Read `git show 5e14bad:web/src/components/SessionView.tsx` for collapse defaults per block type.

- [ ] **Step 1: Set correct collapsibility defaults**

In `display_item.rs`, ensure:
- `UserMessage` and `AssistantMessage` pass `collapsible: false` to MessageBlock
- Bash and AskUserQuestion pass `collapsible: true, default_open: true`
- Other tool blocks (when displayed as full blocks) pass `collapsible: true, default_open: false`
- ThinkingBlock keeps its current default of closed with preview

Verify the GroupBlock defaults to collapsed (from Task 4).

- [ ] **Step 2: Add jump-to-bottom FAB in session_view.rs**

Add a floating button that appears when scrolled away from bottom. Use Dioxus's `onscroll` handler on the session items container:

```rust
let mut show_fab = use_signal(|| false);

// In the RSX:
div {
    class: "session-items",
    onscroll: move |evt| {
        // Use eval to check scroll position
        spawn(async move {
            let result = eval(r#"
                let el = document.querySelector('.session-items');
                el.scrollTop + el.clientHeight < el.scrollHeight - 200
            "#).await;
            if let Ok(val) = result {
                show_fab.set(val.as_bool().unwrap_or(false));
            }
        });
    },
    // ... items ...
}

if show_fab() {
    div {
        class: "scroll-fab",
        onclick: move |_| {
            spawn(async move {
                let _ = eval(r#"
                    document.querySelector('.session-items')
                        .scrollTo({ top: 999999, behavior: 'smooth' })
                "#).await;
            });
        },
        "↓"
    }
}
```

Note: If Dioxus's `onscroll` doesn't fire on the container, fall back to attaching a JS scroll listener via `use_effect` + `eval`. The FAB can be deferred to a follow-up if the scroll listener proves problematic — it should not block the collapsibility defaults work in Step 1.

- [ ] **Step 3: Add CSS for the FAB**

```css
.scroll-fab {
    position: fixed;
    bottom: 2rem;
    right: 2rem;
    width: 3rem;
    height: 3rem;
    border-radius: 50%;
    display: flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    box-shadow: 0 2px 8px rgba(0,0,0,0.3);
    z-index: 100;
}
```

- [ ] **Step 4: Run clippy, format, commit**

```bash
git add -A && git commit -m "Set collapsibility defaults and add jump-to-bottom FAB"
```

---

## Chunk 5: Visual Polish (Tasks 12-14)

### Task 12: Theme System

**Files:**
- Create: `crates/ccmux-app/src/components/theme_toggle.rs`
- Modify: `crates/ccmux-app/src/components/mod.rs`
- Modify: `crates/ccmux-app/src/components/app.rs`
- Modify: `crates/ccmux-app/assets/style.css`

**Old source reference:** Read `git show 5e14bad:web/src/components/ThemeToggle.tsx`, `git show 5e14bad:web/src/components/ThemeToggle.module.css`, and `git show 5e14bad:web/src/app.css` for the complete theme definitions and CSS custom properties. Copy color values directly from the old CSS.

- [ ] **Step 1: Create ThemeToggle component**

A dropdown with three options: System, Light, Dark. Uses `eval` to:
- Read `localStorage.getItem("theme")` on mount
- Set `document.documentElement.setAttribute("data-theme", value)` on change
- Write `localStorage.setItem("theme", value)` on change
- For "system", remove the `data-theme` attribute and let `@media (prefers-color-scheme)` take over

```rust
#[component]
pub fn ThemeToggle() -> Element
```

- [ ] **Step 2: Add ThemeToggle to app.rs nav bar**

In `AppLayout`, add the ThemeToggle to the nav alongside the home link.

- [ ] **Step 3: Replace CSS with theme-aware custom properties**

Overhaul `style.css` to use CSS custom properties. Copy the old app's color values from `5e14bad:web/src/app.css`:

```css
:root {
    --bg: #f4f2ed;
    --surface: #eae7e0;
    --text: #2c2a25;
    --text-muted: #6b6560;
    --accent: ...;
    --border: ...;
    /* ... all other colors from old app.css */
}

[data-theme="dark"] {
    --bg: #1c1b18;
    --surface: #252420;
    --text: #d4cfc4;
    --text-muted: #8a8478;
    /* ... dark theme colors from old app.css */
}

@media (prefers-color-scheme: dark) {
    :root:not([data-theme]) {
        --bg: #1c1b18;
        /* ... same as [data-theme="dark"] */
    }
}
```

Replace all hardcoded colors throughout the CSS with `var(--property-name)`.

- [ ] **Step 4: Run clippy, format, commit**

```bash
git add -A && git commit -m "Add theme system with system/light/dark toggle and CSS custom properties"
```

---

### Task 13: Typography, Color Scheme, Block Styling

**Files:**
- Modify: `crates/ccmux-app/assets/style.css`
- Modify: various component files for label text changes

**Old source reference:** Read `git show 5e14bad:web/src/app.css` for font declarations. Read `git show 5e14bad:web/src/components/SessionView.module.css` and `git show 5e14bad:web/src/components/blocks/CollapsibleBlock.module.css` for block styling. Copy styles directly where applicable.

- [ ] **Step 1: Update typography**

- Add serif font for headings (match old app — check `app.css` for the exact font-family)
- Ensure body uses sans-serif
- Tool names and file paths use monospace
- Change all uppercase labels ("USER", "ASSISTANT") to title case ("User", "Assistant") in the component code

- [ ] **Step 2: Update block borders and backgrounds**

- Green left border for User and Assistant blocks
- Blue left border for groups
- Subtle background fill instead of heavy rounded borders
- Remove `border-radius` from blocks (or make it very subtle)

Copy the exact border colors and background values from the old CSS.

- [ ] **Step 3: Update session list card styles**

Make session cards flat rows with subtle bottom border dividers. Remove card-like styling.

- [ ] **Step 4: Run clippy, format, commit**

```bash
git add -A && git commit -m "Match old visual design: typography, color scheme, block styling"
```

---

### Task 14: Syntax Highlighting with syntect

**Files:**
- Modify: `crates/ccmux-core/Cargo.toml`
- Create: `crates/ccmux-core/src/display/highlight.rs`
- Modify: `crates/ccmux-core/src/display/mod.rs`
- Modify: `crates/ccmux-app/src/components/blocks/prose.rs`
- Modify: `crates/ccmux-app/src/server_fns.rs` (add highlight server function)
- Modify: `crates/ccmux-app/assets/style.css`

**Old source reference:** Read `git show 5e14bad:web/src/lib/highlight.ts` for language detection logic and theme selection.

- [ ] **Step 1: Add syntect dependency**

In `crates/ccmux-core/Cargo.toml`:
```toml
syntect = { version = "5", default-features = false, features = ["default-syntaxes", "default-themes", "html"] }
```

Note: `cfg-gate` this to non-wasm only since syntect should only run server-side:
```toml
[target.'cfg(not(target_arch = "wasm32"))'.dependencies]
syntect = { version = "5", default-features = false, features = ["default-syntaxes", "default-themes", "html"] }
```

- [ ] **Step 2: Create highlight.rs with highlight_code function**

```rust
/// Highlight code using syntect, returning HTML with inline styles.
#[cfg(not(target_arch = "wasm32"))]
pub fn highlight_code(code: &str, language: &str, theme_name: &str) -> String { /* ... */ }

/// Map file extension to syntect language name.
pub fn ext_to_lang(ext: &str) -> &str { /* ... */ }
```

Read the old `highlight.ts` for the `fileExtToLang` mapping and port it.

Use syntect's `highlighted_html_for_string` with `ThemeSet::load_defaults()` and `SyntaxSet::load_defaults_newlines()`.

- [ ] **Step 3: Add a server function for highlighting**

In `server_fns.rs`:
```rust
#[server]
pub async fn highlight_code(code: String, language: String) -> Result<String, ServerFnError> {
    Ok(ccmux_core::display::highlight::highlight_code(&code, &language, "base16-ocean.dark"))
}
```

- [ ] **Step 4: Integrate highlighting into Prose component**

Modify `markdown_to_html` to detect fenced code blocks with language info strings and pass them through the server-side highlighter. This may require a custom pulldown-cmark event handler that intercepts `CodeBlock` events and replaces them with pre-highlighted HTML.

Alternatively, keep the current pulldown-cmark rendering and add a post-processing step that replaces `<code class="language-X">` blocks with highlighted versions via a server call. The simpler approach is preferred for now.

- [ ] **Step 5: Add highlighting to tool-specific views**

Use `highlight_code` in Bash (for command), Read (for file content), Write (for file content), and Edit (for old/new strings) where file extensions provide language hints.

- [ ] **Step 6: Run clippy, format, compile check**

Run: `cargo clippy --workspace && cargo fmt --all`

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "Add server-side syntax highlighting with syntect"
```

---

## Summary

| Task | Description | Phase | Dependencies |
|------|-------------|-------|-------------|
| 1 | Add ItemMeta to DisplayItem | 1 | None |
| 2 | Backend session list fixes | 1 | None |
| 3 | Frontend block header rework | 1 | Task 1 |
| 4 | Group block component | 1 | Task 3 |
| 5 | Session list UI fixes | 2 | Task 2 |
| 6 | Bash formatter + infrastructure | 2 | Task 3 |
| 7 | Read formatter | 2 | Task 6 |
| 8 | Edit formatter | 2 | Task 6 |
| 9 | Grep, Write, Glob formatters | 2 | Task 6 |
| 10 | ToolSearch, WebSearch, Agent, AskUser | 2 | Task 6 |
| 11 | Collapsibility defaults + FAB | 2 | Tasks 3, 4 |
| 12 | Theme system | 3 | None |
| 13 | Typography, colors, block styling | 3 | Task 12 |
| 14 | Syntax highlighting | 3 | Task 12 |

**Parallelizable:** Tasks 1 & 2 (no dependencies). Tasks 5, 6, 11 (after their deps). Tasks 7, 8, 9, 10 (after Task 6). Tasks 13 & 14 (after Task 12).
