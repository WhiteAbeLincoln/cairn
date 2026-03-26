# Session Indexing & Search

## Overview

Add full-text keyword search across Claude Code session logs so an agent (via a skill) or user (via CLI) can find relevant past sessions by content. Built as a new `ccmux-index` crate with a local SQLite + FTS5 index. Designed to support future semantic/embedding search without schema migration.

## Goals

- Search past sessions by message content ("find the session where we discussed authentication middleware")
- Search by file path ("find sessions that modified auth.rs")
- Fast incremental indexing — only process new content since last index
- Works offline, no external services required
- Agent-consumable: a `ccmux search` CLI command that outputs markdown or JSON
- Forward-compatible: schema supports embeddings from day one

## Non-Goals (Phase 1)

- Embedding/semantic search (deferred to phase 2, but schema-ready)
- Web UI search (future follow-up, reuses same query logic)
- Full session backup/archival (separate initiative)
- Indexing tool call inputs/outputs or thinking blocks

## Architecture

### New Crate: `ccmux-index`

A new workspace crate that depends on `ccmux-core` for session discovery and event classification. Owns all indexing and search logic.

```
ccmux-core   (parsing, display pipeline)
    ^
    |
ccmux-index  (SQLite, FTS5, indexing, queries)
    ^
    |
ccmux-app    (web server, CLI subcommands)
```

Dependencies:
- `ccmux-core` — session discovery, event parsing, display pipeline classification
- `rusqlite` with `bundled` feature — SQLite with FTS5 support
- `refinery` with `rusqlite` backend — schema migrations

### Index Location

`~/.claude/ccmux/index.db`

## Schema

Managed via `refinery` migrations. Initial migration (`V1__initial.sql`):

```sql
CREATE TABLE session_index (
    session_id   TEXT PRIMARY KEY,
    project      TEXT NOT NULL,
    project_path TEXT,
    slug         TEXT,
    first_message TEXT,
    created_at   TEXT,              -- ISO 8601
    updated_at   TEXT,              -- ISO 8601
    file_path    TEXT NOT NULL,     -- path to .jsonl file
    last_offset  INTEGER NOT NULL DEFAULT 0,
    indexed_at   TEXT NOT NULL      -- ISO 8601
);

CREATE TABLE messages (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id   TEXT NOT NULL REFERENCES session_index(session_id),
    event_uuid   TEXT NOT NULL,
    role         TEXT NOT NULL,     -- 'user' or 'assistant'
    content      TEXT NOT NULL,
    timestamp    TEXT NOT NULL,     -- ISO 8601
    chunk_index  INTEGER NOT NULL DEFAULT 0,
    embedding    BLOB,
    UNIQUE(event_uuid, chunk_index)
);

CREATE VIRTUAL TABLE messages_fts USING fts5(
    content,
    content_rowid='id',
    content='messages',
    tokenize='porter unicode61'
);

CREATE TABLE session_files (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id   TEXT NOT NULL REFERENCES session_index(session_id),
    file_path    TEXT NOT NULL,
    message_id   TEXT,
    UNIQUE(session_id, file_path, message_id)
);

CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content);
END;

CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, content)
    VALUES('delete', old.id, old.content);
END;
```

### Schema Migrations

Managed by `refinery` with the `rusqlite` backend. Migrations are embedded SQL files (`include_str!`). On `SearchIndex::open()`, pending migrations run automatically in a transaction. Migrations are forward-only (additive).

## What Gets Indexed

### Messages

Events are processed through `ccmux-core`'s existing display pipeline (`events_to_display_items`), which classifies content into `DisplayItem` variants. The indexer extracts:

- **`DisplayItem::UserMessage`** — Human messages to the agent. Indexed with `role = "user"`.
- **`DisplayItem::AssistantMessage`** — Agent text responses to the user. Indexed with `role = "assistant"`.

Skipped (by virtue of display pipeline classification):
- `ToolUse` — agent invoking tools
- `Thinking` — internal reasoning
- `TurnDuration`, `Compaction`, `Other` — system events
- Sidechain/subagent messages (`is_sidechain == true`)
- Tool result content (hidden by pipeline)
- Compaction summaries

### File Paths

From `Event::FileHistory` events, extract file paths from the `snapshot` value. Each unique `(session_id, file_path, message_id)` tuple is stored in `session_files`. Content indexing deferred to a future phase.

## Indexing Pipeline

### Incremental Logic

1. Discover all `.jsonl` session files via `ccmux-core`'s session discovery
2. For each file, look up `session_index.last_offset`
3. If no entry: index from byte 0 (new session)
4. If entry exists: seek to `last_offset`, index only new lines
5. Parse new lines through the display pipeline to classify events
6. Insert qualifying messages and file paths
7. Update `last_offset` and `indexed_at`
8. Each session is wrapped in a SQLite transaction

Reuses `load_session_raw_with_offsets()` which returns byte offsets per JSONL line — these map directly to `last_offset` for tracking progress.

### Lifecycle

**Background indexer (web server mode):** When `ccmux serve` starts, spawn `index_all()` on a background tokio task after the server begins listening. The existing `notify` file watcher (used for SSE streaming) also triggers `index_session()` on `.jsonl` changes, debounced to at most once per 30 seconds per session.

**Standalone CLI:** `ccmux index` runs `index_all()` and exits. Useful for initial index build or cron-based reindexing.

**Missing files:** If a `.jsonl` file has been deleted, the indexer skips it. Existing index rows are preserved — the index remains useful as a lightweight record after session logs are cleaned up.

## Query Interface

### CLI

```
ccmux search <query> [OPTIONS]
```

Options:
- `--limit N` — max results (default 20)
- `--project <path>` — filter to a specific project
- `--after <date>` — sessions created after this date
- `--before <date>` — sessions created before this date
- `--files` — search file paths instead of message content
- `--json` — structured JSON output instead of markdown

### Default Output (Markdown)

Search results are rendered as markdown with a search header followed by matching messages using the same `render_full_item` format from `markdown.rs`. A new `render_search_results` function in that module handles the search-specific wrapper (header, session grouping) while delegating individual message rendering to the existing functions.

```markdown
# Search: "authentication middleware"
12 results across 4 sessions

## fix-auth-bug (2026-03-20)
Project: /Users/abe/Projects/myapp
Session: abc123

## User
[details](/session/abc123/event/0.md)

Can you refactor the authentication middleware to use JWT instead of session cookies?

## Assistant
[details](/session/abc123/event/3a.md)

I'll update the auth middleware in `src/middleware/auth.rs` to use JWT verification...

---
```

Results are grouped by session, ranked by FTS5 relevance.

### Query Logic

```sql
SELECT m.*, s.slug, s.project, s.project_path, s.created_at
FROM messages_fts fts
JOIN messages m ON m.id = fts.rowid
JOIN session_index s ON s.session_id = m.session_id
WHERE messages_fts MATCH ?
  AND (s.project_path = ? OR ? IS NULL)
  AND (s.created_at >= ? OR ? IS NULL)
  AND (s.created_at <= ? OR ? IS NULL)
ORDER BY fts.rank
LIMIT ?
```

File search queries `session_files` with glob/LIKE matching on `file_path`.

## Crate Public API

```rust
pub struct SearchIndex { /* owns rusqlite::Connection */ }

pub struct SearchResult {
    pub session_id: String,
    pub slug: Option<String>,
    pub project: String,
    pub project_path: Option<String>,
    pub created_at: Option<String>,
    pub matches: Vec<MessageMatch>,
}

pub struct MessageMatch {
    pub event_uuid: String,
    pub role: String,
    pub content: String,
    pub timestamp: String,
    pub snippet: String,     // FTS5 snippet() with highlights
}

pub struct FileMatch {
    pub session_id: String,
    pub file_path: String,
    pub message_id: Option<String>,
}

pub struct SearchQuery {
    pub text: String,
    pub project: Option<String>,
    pub after: Option<String>,       // ISO 8601
    pub before: Option<String>,      // ISO 8601
    pub limit: usize,
}

pub struct IndexStats {
    pub sessions_indexed: usize,
    pub messages_indexed: usize,
    pub files_indexed: usize,
    pub duration: Duration,
}

impl SearchIndex {
    pub fn open(path: &Path) -> Result<Self>;
    pub fn index_all(&self, base_path: &Path) -> Result<IndexStats>;
    pub fn index_session(&self, info: &SessionInfo) -> Result<()>;
    pub fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>>;
    pub fn search_files(&self, pattern: &str) -> Result<Vec<FileMatch>>;
}
```

## CLI Subcommands

The existing `ccmux` binary gains subcommands:

- **`ccmux serve`** — starts the web server (current default behavior) with background indexer
- **`ccmux index`** — runs incremental indexing and exits
- **`ccmux search <query> [options]`** — queries the index, outputs markdown or JSON

## Future: Embedding Support (Phase 2)

The schema is ready — `messages.embedding` and `messages.chunk_index` columns exist. When adding embeddings:

1. Add a `config` table to track embedding provider/model (detect when re-embedding is needed)
2. Add a `ccmux embed` subcommand to backfill embeddings for un-embedded rows
3. Configurable embedding provider via trait:

```rust
pub trait EmbeddingProvider: Send + Sync {
    async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
    fn model_id(&self) -> &str;
}
```

4. `ccmux search --semantic` uses cosine similarity on embeddings
5. Hybrid search (FTS5 keyword + embedding reranking) combines both scores

No embedding code is written in phase 1.
