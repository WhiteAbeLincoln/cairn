# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is ccmux?

Session log viewer for Claude Code. Reads `.jsonl` session logs from `~/.claude/projects/` and provides a markdown API for AI agents to explore sessions. Built as a Rust workspace with an Axum server.

## Development

```
cargo run -p ccmux-app    # start the markdown API server on port 3000
```

Run after every change:
```
cargo clippy --workspace && cargo fmt --all
```

Run tests:
```
cargo test --workspace               # all tests
cargo test -p ccmux-core              # core crate only
cargo test -p ccmux-core -- <name>    # single test
```

## Architecture

### Workspace Crates

- **ccmux-core**: Pure data library. Parses JSONL session files into typed events, then transforms them through a display pipeline into renderable items. No UI code. Syntax highlighting via syntect.
- **ccmux-app**: Axum HTTP server. Exposes markdown API endpoints for session exploration by AI agents. Bridges ccmux-core to HTTP.

### Data Flow: JSONL → Markdown API

```
Session .jsonl file
  → loader.rs: load_session_raw() → Vec<Value>
  → events/parse.rs: parse_events() → Vec<Event>
  → display/pipeline.rs: events_to_display_items() → Vec<DisplayItemWithMode>
  → display/markdown.rs: render as markdown
  → api.rs: served over HTTP
```

### Key Concepts

**Event types** (`ccmux-core/src/events/`): `AssistantEventData`, `UserEventData`, `SystemEventData`, `ProgressEventData`, `FileHistoryEventData`, `QueueOperationEventData`. Unknown event types gracefully degrade to `Event::Unknown(Value)`.

**Display pipeline** (`ccmux-core/src/display/pipeline.rs`): Converts typed events into `DisplayItem` variants (UserMessage, AssistantMessage, Thinking, ToolUse, TurnDuration, Compaction, Other). Each item gets a `DisplayMode` (Full, Collapsed, Grouped, Hidden) that controls rendering behavior.

**Tool result pairing**: User events contain `tool_result` content blocks keyed by `tool_use_id`. The pipeline pre-scans these into a HashMap, then pairs them inline with the corresponding assistant ToolUse items.

**Grouping logic**: Items with Grouped mode accumulate until a Full/Collapsed item breaks the group. Single grouped item → Collapsed; multiple → Grouped with summary header (e.g., "Thinking · Read×2 · Bash").

### Markdown API (`api.rs`)

- `GET /sessions.md`: Lists all sessions grouped by project
- `GET /session/{id}.md`: Paginated session content (`?page=1&per_page=50`)
- `GET /session/{id}/event/{cursor}.md`: Single event detail (`?metadata=true` for raw JSON)
