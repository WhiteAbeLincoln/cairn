# Markdown Session View

Plain-text markdown API for exploring Claude Code sessions. Designed for consumption by AI agents — concise, paginated, with links to drill into individual events.

## Endpoints

All markdown endpoints use the `.md` suffix. No content negotiation for now.

### `GET /sessions.md`

Lists available sessions grouped by project, mirroring `list_sessions()`.

**Response:** `Content-Type: text/markdown; charset=utf-8`

```markdown
# Sessions

## /Users/alice/myproject

- [Fix auth bug](/session/<id>.md) — 2026-03-14 10:30
- [Add logging](/session/<id>.md) — 2026-03-13 15:00

## /Users/alice/other-project

- [Initial setup](/session/<id>.md) — 2026-03-12 09:00
```

Each entry shows the session's first message (truncated) and updated_at timestamp.

### `GET /session/:id.md`

Paginated markdown view of a session. Shows full user/assistant messages inline; all other display items (tool calls, thinking, compactions) appear as a bullet list of links.

**Query params:**
- `page` — 1-indexed page number (default: 1)
- `per_page` — top-level items per page (default: 50)

**Response:** `Content-Type: text/markdown; charset=utf-8`

#### Output format

```markdown
# Session <id>

[sessions](/sessions.md)

## User
[details](/session/<id>/event/<cursor>.md)

What does this function do?

## Assistant
[details](/session/<id>/event/<cursor>.md)

This function parses the input and returns...

- [Bash: `ls src/`](/session/<id>/event/<cursor>.md)
- [Read: src/main.rs](/session/<id>/event/<cursor>.md)
- [Thinking](/session/<id>/event/<cursor>.md)

## User
[details](/session/<id>/event/<cursor>.md)

Can you refactor it?

---
Page 1 of 5 | [Next →](/session/<id>.md?page=2)
```

#### Display opts

The markdown API uses custom `DisplayOpts` different from the web UI:
- **Full**: UserMessage, AssistantMessage only
- **Collapsed/Grouped**: Thinking, ToolUse (all tools including Bash, AskUserQuestion), Compaction
- **Hidden**: TurnDuration, Other

This ensures only conversational content appears inline, with everything else as navigable links.

#### Display rules

- **Full items** (UserMessage, AssistantMessage): Render with `## User` or `## Assistant` header, details link directly under the header, then content.
- **Non-full items** (Collapsed, Grouped): Render as a bullet list of links within the preceding assistant section. Each item within a group gets its own bullet with its own cursor. A `Collapsed(item)` renders as a single bullet. A `Grouped(vec)` renders as multiple bullets.
- **Hidden items**: Omitted entirely.

#### Bullet label format

| Item type | Label example |
|-----------|--------------|
| ToolUse | `Bash: \`ls src/\`` / `Read: src/main.rs` / `Edit: src/lib.rs` / `Grep: "pattern"` |
| Thinking | `Thinking` |
| Compaction | `Compaction` |

Tool labels include a short summary derived from the tool input (command for Bash, file path for Read/Edit/Write, pattern for Grep/Glob, etc.).

#### Pagination

Pages are counted by top-level rendered items: each Full message counts as 1, each collapsed/grouped block counts as 1. The pipeline processes the full file but only renders the target page window.

Footer includes current page, total pages, and prev/next links when applicable. Single-page sessions omit the footer. Empty sessions render only the `# Session` header.

### `GET /session/:id/event/:cursor.md`

Detail view of a single event. Since one JSONL line can produce multiple display items (e.g., an assistant event with text + thinking + tool_use blocks), the detail page shows the entire event — all content blocks from that line.

**Query params:**
- `metadata` — if `true`, prepend metadata section (timestamp, model, tokens, uuid)

**Response:** `Content-Type: text/markdown; charset=utf-8`

**Navigation:** Includes a back link to the session page.

#### Output format — tool calls

```markdown
[back to session](/session/<id>.md)

## Bash

### Input
```json
{"command": "ls src/", "description": "List source files"}
```

### Output
```json
{"output": "main.rs\nlib.rs\n", "error": null}
```
```

Input and output are rendered as JSON code blocks from the raw JSONL event data (not parsed Rust structs).

#### Output format — text events

For UserMessage, AssistantMessage, Thinking: the full text content rendered as markdown.

#### Output format — metadata (when `?metadata=true`)

```markdown
## Metadata

| Field | Value |
|-------|-------|
| Timestamp | 2026-03-14T10:30:00Z |
| Model | claude-opus-4-6 |
| Tokens | 1234 in / 567 out |
| UUID | abc-123-def |

---
```

Metadata is extracted from the raw JSONL event fields (`timestamp`, `message.model`, `message.usage`, `uuid`).

## Opaque Cursor

The cursor encodes the byte offset of the event's line in the JSONL file. Format: base64url-encoded byte offset (no padding). This allows O(1) random access via `seek()`.

- Generated during session loading by tracking byte positions per line
- Stored on `DisplayItem` directly (new `cursor: Option<String>` field)
- Multiple display items from the same JSONL line share the same cursor value
- For grouped items, each source item retains its own cursor
- To resolve: decode offset, seek to that position, read one line, parse, render

## Error Handling

| Condition | Status | Body |
|-----------|--------|------|
| Session not found | 404 | `Session not found: <id>` |
| Invalid cursor (bad base64url) | 400 | `Invalid cursor` |
| Cursor offset beyond file length | 400 | `Invalid cursor: offset out of range` |
| Page out of range | 404 | `Page <n> not found. Session has <m> pages.` |
| Invalid page/per_page (non-numeric, ≤0) | 400 | `Invalid parameter: <details>` |

Error responses use `Content-Type: text/plain; charset=utf-8`.

## Streaming Sessions

The markdown endpoints return a point-in-time snapshot. No SSE/streaming markdown endpoint is planned. For live sessions, the last page may grow between requests as new events are appended. Earlier pages are stable since JSONL files are append-only.

## Implementation

### ccmux-core

#### `session/loader.rs`
- Add `load_session_raw_with_offsets(path) -> Vec<(u64, Value)>` — reads the file tracking byte position before each line, returns each JSON value paired with its byte offset.

#### `display/mod.rs`
- Add `cursor: Option<String>` field to each `DisplayItem` variant (or as a common field if the enum structure allows).

#### `display/pipeline.rs`
- `events_to_display_items` gains an optional `offsets: Option<&[u64]>` parameter (parallel to `raw_events`). When provided, each intermediate item is tagged with its source offset, which is base64url-encoded into `DisplayItem.cursor`.
- The existing call sites pass `None` for offsets (no behavior change for the web UI).
- `single_event_to_display_items` gains an optional `offset: Option<u64>` parameter for streaming use.

#### `display/markdown.rs` (new)
- `render_session_markdown(session_id: &str, items: &[DisplayItemWithMode], page: usize, per_page: usize) -> String` — paginated session view.
- `render_event_detail(raw_event: &Value, show_metadata: bool, session_id: &str) -> String` — single event detail from raw JSON.
- `render_session_list(groups: &[ProjectGroup]) -> String` — session list view.
- Bullet label generation per tool type.
- Pagination footer rendering.

### ccmux-app

#### `main.rs`
- Server target (`#[cfg(feature = "server")]`): use `dioxus::prelude::dioxus_server::serve(|| async { ... })`. Inside the closure, build a `Router::new()`, merge custom markdown routes via `build_api_router()`, then chain `.serve_dioxus_application(ServeConfig::new(), App)` to add SSR and server functions. Return `Ok(router)`.
- WASM target (`#[cfg(not(feature = "server"))]`): continue using `dioxus::launch(App)`.

#### `api.rs` (new server-only module)
- `session_list_markdown_handler` — calls `list_sessions`, renders markdown.
- `session_markdown_handler` — loads session with offsets, runs display pipeline with markdown-specific `DisplayOpts`, renders markdown.
- `event_detail_handler` — decodes cursor, seeks to byte offset, reads/parses one line, renders detail markdown.
- `build_api_router() -> axum::Router` — registers all markdown routes, called from `main.rs`.

#### `Cargo.toml`
- Add `axum` as a direct server-only dependency.
