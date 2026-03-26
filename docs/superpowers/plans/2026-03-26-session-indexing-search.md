# Session Indexing & Search Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add full-text keyword search across Claude Code session logs via a new `ccmux-index` crate with SQLite + FTS5, exposed through `ccmux search` and `ccmux index` CLI subcommands.

**Architecture:** New `ccmux-index` crate owns SQLite (via `rusqlite`) and schema migrations (via `refinery`). It depends on `ccmux-core` for session discovery and event classification through the display pipeline. `ccmux-app` integrates the indexer into the web server lifecycle and adds CLI subcommands via `clap`.

**Tech Stack:** Rust, rusqlite (bundled), refinery, clap, ccmux-core display pipeline, FTS5

**Spec:** `docs/superpowers/specs/2026-03-26-session-indexing-search-design.md`

---

## File Structure

### New crate: `crates/ccmux-index/`

| File | Responsibility |
|------|---------------|
| `Cargo.toml` | Crate manifest: rusqlite, refinery, ccmux-core deps |
| `src/lib.rs` | Public API: `SearchIndex`, query/result types, re-exports |
| `src/db.rs` | Database connection, migration runner, low-level SQL helpers |
| `src/indexer.rs` | Incremental indexing logic: session scanning, event extraction, file path extraction |
| `src/query.rs` | FTS5 search queries, file path search, result grouping |
| `src/migrations/V1__initial.sql` | Initial schema migration |
| `tests/index_test.rs` | Integration tests against a temp SQLite DB |

### Modified files

| File | Change |
|------|--------|
| `Cargo.toml` (workspace) | Add `ccmux-index` to workspace members |
| `crates/ccmux-app/Cargo.toml` | Add `ccmux-index`, `clap` dependencies |
| `crates/ccmux-app/src/main.rs` | Add clap subcommand dispatch (`serve`, `index`, `search`) around existing Axum server startup |
| `crates/ccmux-core/src/display/markdown.rs` | Add `render_search_results()` function |

---

## Task 1: Create `ccmux-index` crate scaffold with schema migration

**Files:**
- Create: `crates/ccmux-index/Cargo.toml`
- Create: `crates/ccmux-index/src/lib.rs`
- Create: `crates/ccmux-index/src/db.rs`
- Create: `crates/ccmux-index/src/migrations/V1__initial.sql`
- Modify: `Cargo.toml` (workspace root)

- [ ] **Step 1: Create the migration SQL file**

Create `crates/ccmux-index/src/migrations/V1__initial.sql`:

```sql
CREATE TABLE session_index (
    session_id   TEXT PRIMARY KEY,
    project      TEXT NOT NULL,
    project_path TEXT,
    slug         TEXT,
    first_message TEXT,
    created_at   TEXT,
    updated_at   TEXT,
    file_path    TEXT NOT NULL,
    last_offset  INTEGER NOT NULL DEFAULT 0,
    indexed_at   TEXT NOT NULL
);

CREATE TABLE messages (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id   TEXT NOT NULL REFERENCES session_index(session_id),
    event_uuid   TEXT NOT NULL,
    role         TEXT NOT NULL,
    content      TEXT NOT NULL,
    timestamp    TEXT NOT NULL,
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

- [ ] **Step 2: Create Cargo.toml for ccmux-index**

Create `crates/ccmux-index/Cargo.toml`:

```toml
[package]
name = "ccmux-index"
version.workspace = true
edition = "2024"

[dependencies]
ccmux-core = { path = "../ccmux-core" }
rusqlite = { version = "0.35", features = ["bundled"] }
refinery = { version = "0.8", features = ["rusqlite"] }
chrono = { version = "0.4", features = ["serde"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tracing = "0.1"

[dev-dependencies]
tempfile = "3"
```

Note: Check the latest `rusqlite` and `refinery` versions on crates.io at implementation time. The versions above are reasonable defaults but may need bumping.

- [ ] **Step 3: Create db.rs with migration runner**

Create `crates/ccmux-index/src/db.rs`:

```rust
use std::path::Path;

use refinery::embed_migrations;
use rusqlite::Connection;

embed_migrations!("src/migrations");

/// Open (or create) the SQLite database and run pending migrations.
pub fn open_db(path: &Path) -> Result<Connection, Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;

    migrations::runner().run(&mut conn)?;

    Ok(conn)
}
```

- [ ] **Step 4: Create lib.rs with public types and SearchIndex struct**

Create `crates/ccmux-index/src/lib.rs`:

```rust
pub mod db;
pub mod indexer;
pub mod query;

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;
use serde::Serialize;

/// Handle to the search index database.
pub struct SearchIndex {
    conn: Connection,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub session_id: String,
    pub slug: Option<String>,
    pub project: String,
    pub project_path: Option<String>,
    pub created_at: Option<String>,
    pub matches: Vec<MessageMatch>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MessageMatch {
    pub event_uuid: String,
    pub role: String,
    pub content: String,
    pub timestamp: String,
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileMatch {
    pub session_id: String,
    pub file_path: String,
    pub message_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub text: String,
    pub project: Option<String>,
    pub after: Option<String>,
    pub before: Option<String>,
    pub limit: usize,
}

#[derive(Debug, Clone)]
pub struct IndexStats {
    pub sessions_indexed: usize,
    pub messages_indexed: usize,
    pub files_indexed: usize,
    pub duration: Duration,
}

impl SearchIndex {
    /// Open or create the index database at the given path.
    pub fn open(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let conn = db::open_db(path)?;
        Ok(Self { conn })
    }
}
```

- [ ] **Step 5: Add ccmux-index to workspace**

In the root `Cargo.toml`, add `"crates/ccmux-index"` to the workspace members list:

```toml
[workspace]
members = ["crates/ccmux-core", "crates/ccmux-app", "crates/ccmux-index"]
```

- [ ] **Step 6: Create empty module files**

Create `crates/ccmux-index/src/indexer.rs`:

```rust
// Indexing pipeline — implemented in Task 3
```

Create `crates/ccmux-index/src/query.rs`:

```rust
// Search queries — implemented in Task 4
```

- [ ] **Step 7: Verify it compiles**

Run: `cargo check -p ccmux-index`
Expected: Compiles with no errors (warnings about unused modules are OK).

- [ ] **Step 8: Write test for database open and migration**

Create `crates/ccmux-index/tests/index_test.rs`:

```rust
use ccmux_index::SearchIndex;
use tempfile::TempDir;

#[test]
fn test_open_creates_db_and_runs_migrations() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");

    let index = SearchIndex::open(&db_path).unwrap();

    // Verify tables exist by querying them
    let conn = &index.conn;
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM session_index", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM session_files", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn test_open_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");

    let _index1 = SearchIndex::open(&db_path).unwrap();
    // Opening again should not fail (migrations already applied)
    let _index2 = SearchIndex::open(&db_path).unwrap();
}
```

Note: The test accesses `index.conn` directly. If `conn` is private, either make it `pub(crate)` or add a `SearchIndex::conn(&self) -> &Connection` accessor. Choose whichever pattern fits — the test needs to verify tables exist.

- [ ] **Step 9: Run tests**

Run: `cargo test -p ccmux-index`
Expected: Both tests pass.

- [ ] **Step 10: Commit**

```bash
git add crates/ccmux-index/ Cargo.toml
git commit -m "feat: add ccmux-index crate with schema migration"
```

---

## Task 2: Incremental indexing of messages

**Files:**
- Modify: `crates/ccmux-index/src/indexer.rs`
- Modify: `crates/ccmux-index/src/lib.rs`
- Modify: `crates/ccmux-index/tests/index_test.rs`

This task implements `index_session()` and `index_all()`. The indexer loads raw events with offsets from `ccmux-core`, runs them through the display pipeline, and inserts `UserMessage` and `AssistantMessage` items into the database.

- [ ] **Step 1: Write the failing test for `index_session`**

Add to `crates/ccmux-index/tests/index_test.rs`:

```rust
use ccmux_index::{IndexStats, SearchIndex};
use ccmux_core::session::SessionInfo;
use chrono::Utc;
use std::io::Write;
use tempfile::TempDir;

fn make_test_session(dir: &TempDir) -> (std::path::PathBuf, SessionInfo) {
    let session_id = "test-session-1";
    let jsonl_path = dir.path().join(format!("{session_id}.jsonl"));

    let events = vec![
        // User message
        serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": "How do I fix the authentication bug?"},
            "uuid": "u1",
            "timestamp": "2026-03-20T10:00:00Z",
            "userType": "external",
            "cwd": "/Users/test/myproject",
            "sessionId": session_id,
            "isSidechain": false,
            "version": "1"
        }),
        // Assistant text response
        serde_json::json!({
            "type": "assistant",
            "message": {
                "model": "claude-opus-4-6",
                "content": [
                    {"type": "text", "text": "I'll help you fix the authentication middleware."}
                ],
                "usage": {"input_tokens": 100, "output_tokens": 50}
            },
            "uuid": "a1",
            "timestamp": "2026-03-20T10:00:05Z",
            "userType": "external",
            "cwd": "/Users/test/myproject",
            "sessionId": session_id,
            "isSidechain": false,
            "version": "1"
        }),
        // Assistant tool use (should NOT be indexed as a message)
        serde_json::json!({
            "type": "assistant",
            "message": {
                "model": "claude-opus-4-6",
                "content": [
                    {"type": "tool_use", "id": "t1", "name": "Read", "input": {"file_path": "/src/auth.rs"}}
                ],
                "usage": {"input_tokens": 50, "output_tokens": 20}
            },
            "uuid": "a2",
            "timestamp": "2026-03-20T10:00:10Z",
            "userType": "external",
            "cwd": "/Users/test/myproject",
            "sessionId": session_id,
            "isSidechain": false,
            "version": "1"
        }),
        // Tool result (user event with array content — should NOT be indexed)
        serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "file contents here"}]
            },
            "uuid": "u2",
            "timestamp": "2026-03-20T10:00:15Z",
            "userType": "external",
            "cwd": "/Users/test/myproject",
            "sessionId": session_id,
            "isSidechain": false,
            "version": "1"
        }),
        // Another user message
        serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": "Now add JWT token validation"},
            "uuid": "u3",
            "timestamp": "2026-03-20T10:01:00Z",
            "userType": "external",
            "cwd": "/Users/test/myproject",
            "sessionId": session_id,
            "isSidechain": false,
            "version": "1"
        }),
    ];

    let mut file = std::fs::File::create(&jsonl_path).unwrap();
    for event in &events {
        writeln!(file, "{}", serde_json::to_string(event).unwrap()).unwrap();
    }

    let info = SessionInfo {
        id: session_id.to_string(),
        project: "-Users-test-myproject".to_string(),
        path: jsonl_path.clone(),
        slug: Some("fix-auth-bug".to_string()),
        created_at: Some(Utc::now()),
        updated_at: Some(Utc::now()),
        message_count: events.len(),
        first_message: Some("How do I fix the authentication bug?".to_string()),
        project_path: Some("/Users/test/myproject".to_string()),
        is_sidechain: false,
        parent_session_id: None,
        agent_id: None,
    };

    (jsonl_path, info)
}

#[test]
fn test_index_session_extracts_messages() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let index = SearchIndex::open(&db_path).unwrap();

    let session_dir = TempDir::new().unwrap();
    let (_jsonl_path, info) = make_test_session(&session_dir);

    index.index_session(&info).unwrap();

    // Should have indexed 2 user messages + 1 assistant text = 3 messages
    // Tool use and tool result should be excluded
    let count: i64 = index.conn()
        .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 3);

    // Verify roles
    let user_count: i64 = index.conn()
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE role = 'user'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(user_count, 2);

    let assistant_count: i64 = index.conn()
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE role = 'assistant'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(assistant_count, 1);
}

#[test]
fn test_index_session_is_incremental() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let index = SearchIndex::open(&db_path).unwrap();

    let session_dir = TempDir::new().unwrap();
    let (jsonl_path, info) = make_test_session(&session_dir);

    // First index
    index.index_session(&info).unwrap();
    let count1: i64 = index.conn()
        .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
        .unwrap();

    // Append a new user message
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&jsonl_path)
        .unwrap();
    writeln!(file, "{}", serde_json::to_string(&serde_json::json!({
        "type": "user",
        "message": {"role": "user", "content": "One more thing about RBAC"},
        "uuid": "u4",
        "timestamp": "2026-03-20T10:05:00Z",
        "userType": "external",
        "cwd": "/Users/test/myproject",
        "sessionId": "test-session-1",
        "isSidechain": false,
        "version": "1"
    })).unwrap()).unwrap();

    // Re-index — should only pick up the new message
    index.index_session(&info).unwrap();
    let count2: i64 = index.conn()
        .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
        .unwrap();

    assert_eq!(count2, count1 + 1);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ccmux-index`
Expected: FAIL — `index_session` method doesn't exist yet.

- [ ] **Step 3: Implement the indexer**

Write `crates/ccmux-index/src/indexer.rs`:

```rust
use std::io::{BufRead, Seek, SeekFrom};
use std::path::Path;
use std::time::Instant;

use rusqlite::{Connection, params};
use serde_json::Value;

use ccmux_core::display::{DisplayItem, DisplayModeF, DisplayOpts};
use ccmux_core::display::pipeline::events_to_display_items;
use ccmux_core::events::{Event, parse_events};
use ccmux_core::session::SessionInfo;
use ccmux_core::session::loader::discover_sessions;

use crate::IndexStats;

/// Index a single session. Reads from `last_offset` if previously indexed.
pub fn index_session(
    conn: &Connection,
    info: &SessionInfo,
) -> Result<(), Box<dyn std::error::Error>> {
    let last_offset = get_last_offset(conn, &info.id)?;

    let raw_events = load_from_offset(&info.path, last_offset)?;
    if raw_events.is_empty() {
        return Ok(());
    }

    let final_offset = raw_events.last().map(|(off, val)| {
        // offset after this line = offset + serialized length + newline
        let serialized = serde_json::to_string(val).unwrap_or_default();
        off + serialized.len() as u64 + 1
    }).unwrap_or(last_offset);

    let events = parse_events(&raw_events.iter().map(|(_, v)| v.clone()).collect::<Vec<_>>());
    let raw_values: Vec<Value> = raw_events.iter().map(|(_, v)| v.clone()).collect();

    // Use markdown display opts — this gives us UserMessage and AssistantMessage as Full,
    // tool uses as Grouped, and tool results as Hidden. We only care about Full items.
    let opts = DisplayOpts::markdown();
    let display_items = events_to_display_items(&events, &raw_values, &opts);

    let tx = conn.unchecked_transaction()?;

    // Upsert session_index
    tx.execute(
        "INSERT INTO session_index (session_id, project, project_path, slug, first_message, created_at, updated_at, file_path, last_offset, indexed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(session_id) DO UPDATE SET
           slug = COALESCE(excluded.slug, session_index.slug),
           first_message = COALESCE(excluded.first_message, session_index.first_message),
           updated_at = COALESCE(excluded.updated_at, session_index.updated_at),
           last_offset = excluded.last_offset,
           indexed_at = excluded.indexed_at",
        params![
            info.id,
            info.project,
            info.project_path,
            info.slug,
            info.first_message,
            info.created_at.map(|d| d.to_rfc3339()),
            info.updated_at.map(|d| d.to_rfc3339()),
            info.path.to_string_lossy().to_string(),
            final_offset as i64,
            chrono::Utc::now().to_rfc3339(),
        ],
    )?;

    // Insert messages from display items
    for item in &display_items {
        match item {
            DisplayModeF::Full(DisplayItem::UserMessage { content, meta, .. }) => {
                insert_message(&tx, &info.id, meta.uuid.as_deref().unwrap_or(""), "user", content, "")?;
            }
            DisplayModeF::Full(DisplayItem::AssistantMessage { text, meta, .. }) => {
                insert_message(&tx, &info.id, meta.uuid.as_deref().unwrap_or(""), "assistant", text, "")?;
            }
            _ => {}
        }
    }

    // Extract file paths from raw events (file-history-snapshot)
    for raw in &raw_values {
        if raw.get("type").and_then(|v| v.as_str()) == Some("file-history-snapshot") {
            extract_file_paths(&tx, &info.id, raw)?;
        }
    }

    tx.commit()?;
    Ok(())
}

/// Index all sessions discovered under the base path.
pub fn index_all(
    conn: &Connection,
    base_path: &Path,
) -> Result<IndexStats, Box<dyn std::error::Error>> {
    let start = Instant::now();
    let sessions = discover_sessions(base_path)?;

    let mut stats = IndexStats {
        sessions_indexed: 0,
        messages_indexed: 0,
        files_indexed: 0,
        duration: std::time::Duration::ZERO,
    };

    for session in &sessions {
        if session.is_sidechain {
            continue;
        }
        if !session.path.exists() {
            continue;
        }

        let msgs_before = count_messages(conn, &session.id)?;
        let files_before = count_files(conn, &session.id)?;

        match index_session(conn, session) {
            Ok(()) => {
                let msgs_after = count_messages(conn, &session.id)?;
                let files_after = count_files(conn, &session.id)?;
                if msgs_after > msgs_before || files_after > files_before {
                    stats.sessions_indexed += 1;
                }
                stats.messages_indexed += (msgs_after - msgs_before) as usize;
                stats.files_indexed += (files_after - files_before) as usize;
            }
            Err(e) => {
                tracing::warn!(session_id = %session.id, error = %e, "Failed to index session");
            }
        }
    }

    stats.duration = start.elapsed();
    Ok(stats)
}

fn get_last_offset(conn: &Connection, session_id: &str) -> Result<u64, Box<dyn std::error::Error>> {
    let result = conn.query_row(
        "SELECT last_offset FROM session_index WHERE session_id = ?1",
        params![session_id],
        |row| row.get::<_, i64>(0),
    );
    match result {
        Ok(offset) => Ok(offset as u64),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
        Err(e) => Err(e.into()),
    }
}

/// Load JSONL events from a file starting at the given byte offset.
fn load_from_offset(
    path: &Path,
    offset: u64,
) -> Result<Vec<(u64, Value)>, Box<dyn std::error::Error>> {
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    reader.seek(SeekFrom::Start(offset))?;

    let mut events = Vec::new();
    let mut current_offset = offset;
    let mut line = String::new();

    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line)?;
        if bytes_read == 0 {
            break;
        }
        let line_offset = current_offset;
        current_offset += bytes_read as u64;

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match serde_json::from_str::<Value>(trimmed) {
            Ok(value) => events.push((line_offset, value)),
            Err(e) => {
                tracing::warn!(offset = line_offset, error = %e, "Failed to parse JSONL line");
            }
        }
    }

    Ok(events)
}

fn insert_message(
    conn: &Connection,
    session_id: &str,
    event_uuid: &str,
    role: &str,
    content: &str,
    timestamp: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    conn.execute(
        "INSERT OR IGNORE INTO messages (session_id, event_uuid, role, content, timestamp, chunk_index)
         VALUES (?1, ?2, ?3, ?4, ?5, 0)",
        params![session_id, event_uuid, role, content, timestamp],
    )?;
    Ok(())
}

fn extract_file_paths(
    conn: &Connection,
    session_id: &str,
    raw: &Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let message_id = raw.get("messageId").and_then(|v| v.as_str());
    let snapshot = raw.get("snapshot").and_then(|v| v.as_object());

    if let Some(snapshot) = snapshot {
        let backups = snapshot
            .get("trackedFileBackups")
            .and_then(|v| v.as_object());

        if let Some(backups) = backups {
            for file_path in backups.keys() {
                conn.execute(
                    "INSERT OR IGNORE INTO session_files (session_id, file_path, message_id)
                     VALUES (?1, ?2, ?3)",
                    params![session_id, file_path, message_id],
                )?;
            }
        }
    }

    Ok(())
}

fn count_messages(conn: &Connection, session_id: &str) -> Result<i64, Box<dyn std::error::Error>> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
        params![session_id],
        |row| row.get(0),
    )?)
}

fn count_files(conn: &Connection, session_id: &str) -> Result<i64, Box<dyn std::error::Error>> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM session_files WHERE session_id = ?1",
        params![session_id],
        |row| row.get(0),
    )?)
}
```

Note: The `insert_message` function needs a timestamp, but `ItemMeta` doesn't carry one. Build a `HashMap<String, String>` mapping `uuid -> timestamp` from raw events before iterating display items. When inserting a message, look up the timestamp from this map using `meta.uuid`. Example:

```rust
let timestamps: HashMap<String, String> = raw_values.iter()
    .filter_map(|v| {
        let uuid = v.get("uuid")?.as_str()?.to_string();
        let ts = v.get("timestamp")?.as_str()?.to_string();
        Some((uuid, ts))
    })
    .collect();
```

Then when calling `insert_message`, pass `timestamps.get(uuid).map(|s| s.as_str()).unwrap_or("")`.

- [ ] **Step 4: Add `index_session` and `index_all` methods to `SearchIndex`**

In `crates/ccmux-index/src/lib.rs`, add to the `impl SearchIndex` block:

```rust
use ccmux_core::session::SessionInfo;
use std::path::Path;

impl SearchIndex {
    pub fn open(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let conn = db::open_db(path)?;
        Ok(Self { conn })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn index_session(&self, info: &SessionInfo) -> Result<(), Box<dyn std::error::Error>> {
        indexer::index_session(&self.conn, info)
    }

    pub fn index_all(&self, base_path: &Path) -> Result<IndexStats, Box<dyn std::error::Error>> {
        indexer::index_all(&self.conn, base_path)
    }
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p ccmux-index`
Expected: All tests pass, including the new `test_index_session_extracts_messages` and `test_index_session_is_incremental`.

- [ ] **Step 6: Commit**

```bash
git add crates/ccmux-index/
git commit -m "feat: implement incremental session indexing"
```

---

## Task 3: File path extraction from file-history-snapshot events

**Files:**
- Modify: `crates/ccmux-index/tests/index_test.rs`

The file path extraction is already implemented in `indexer.rs` (Task 2). This task adds tests for it.

- [ ] **Step 1: Write test for file path extraction**

Add to `crates/ccmux-index/tests/index_test.rs`:

```rust
#[test]
fn test_index_session_extracts_file_paths() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let index = SearchIndex::open(&db_path).unwrap();

    let session_dir = TempDir::new().unwrap();
    let session_id = "test-session-files";
    let jsonl_path = session_dir.path().join(format!("{session_id}.jsonl"));

    let events = vec![
        serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": "Fix the config"},
            "uuid": "u1",
            "timestamp": "2026-03-20T10:00:00Z",
            "userType": "external",
            "cwd": "/Users/test/myproject",
            "sessionId": session_id,
            "isSidechain": false,
            "version": "1"
        }),
        serde_json::json!({
            "type": "file-history-snapshot",
            "messageId": "a1",
            "snapshot": {
                "trackedFileBackups": {
                    "src/config.rs": {"backupFileName": "backup1", "version": 1, "backupTime": "2026-03-20T10:00:10Z"},
                    "src/lib.rs": {"backupFileName": "backup2", "version": 1, "backupTime": "2026-03-20T10:00:10Z"}
                },
                "messageId": "a1",
                "timestamp": "2026-03-20T10:00:10Z"
            },
            "isSnapshotUpdate": false
        }),
    ];

    let mut file = std::fs::File::create(&jsonl_path).unwrap();
    for event in &events {
        writeln!(file, "{}", serde_json::to_string(event).unwrap()).unwrap();
    }

    let info = SessionInfo {
        id: session_id.to_string(),
        project: "-Users-test-myproject".to_string(),
        path: jsonl_path,
        slug: None,
        created_at: Some(Utc::now()),
        updated_at: Some(Utc::now()),
        message_count: events.len(),
        first_message: Some("Fix the config".to_string()),
        project_path: Some("/Users/test/myproject".to_string()),
        is_sidechain: false,
        parent_session_id: None,
        agent_id: None,
    };

    index.index_session(&info).unwrap();

    let count: i64 = index.conn()
        .query_row("SELECT COUNT(*) FROM session_files", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 2);

    // Verify specific file paths
    let paths: Vec<String> = {
        let mut stmt = index.conn()
            .prepare("SELECT file_path FROM session_files ORDER BY file_path")
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    assert_eq!(paths, vec!["src/config.rs", "src/lib.rs"]);
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p ccmux-index`
Expected: All tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/ccmux-index/
git commit -m "test: add file path extraction tests for indexer"
```

---

## Task 4: FTS5 search queries

**Files:**
- Modify: `crates/ccmux-index/src/query.rs`
- Modify: `crates/ccmux-index/src/lib.rs`
- Modify: `crates/ccmux-index/tests/index_test.rs`

- [ ] **Step 1: Write failing test for text search**

Add to `crates/ccmux-index/tests/index_test.rs`:

```rust
use ccmux_index::SearchQuery;

#[test]
fn test_search_finds_matching_messages() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let index = SearchIndex::open(&db_path).unwrap();

    let session_dir = TempDir::new().unwrap();
    let (_jsonl_path, info) = make_test_session(&session_dir);
    index.index_session(&info).unwrap();

    let results = index.search(&SearchQuery {
        text: "authentication".to_string(),
        project: None,
        after: None,
        before: None,
        limit: 20,
    }).unwrap();

    assert_eq!(results.len(), 1); // One session
    assert_eq!(results[0].session_id, "test-session-1");
    assert!(results[0].matches.len() >= 1); // At least the user message about auth
}

#[test]
fn test_search_no_results() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let index = SearchIndex::open(&db_path).unwrap();

    let session_dir = TempDir::new().unwrap();
    let (_jsonl_path, info) = make_test_session(&session_dir);
    index.index_session(&info).unwrap();

    let results = index.search(&SearchQuery {
        text: "kubernetes".to_string(),
        project: None,
        after: None,
        before: None,
        limit: 20,
    }).unwrap();

    assert!(results.is_empty());
}

#[test]
fn test_search_with_project_filter() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let index = SearchIndex::open(&db_path).unwrap();

    let session_dir = TempDir::new().unwrap();
    let (_jsonl_path, info) = make_test_session(&session_dir);
    index.index_session(&info).unwrap();

    // Search with matching project
    let results = index.search(&SearchQuery {
        text: "authentication".to_string(),
        project: Some("/Users/test/myproject".to_string()),
        after: None,
        before: None,
        limit: 20,
    }).unwrap();
    assert_eq!(results.len(), 1);

    // Search with non-matching project
    let results = index.search(&SearchQuery {
        text: "authentication".to_string(),
        project: Some("/Users/test/other".to_string()),
        after: None,
        before: None,
        limit: 20,
    }).unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_search_files() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let index = SearchIndex::open(&db_path).unwrap();

    // Create a session with file-history-snapshot (reuse from Task 3 test helper or inline)
    let session_dir = TempDir::new().unwrap();
    let session_id = "test-session-files-search";
    let jsonl_path = session_dir.path().join(format!("{session_id}.jsonl"));
    let events = vec![
        serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": "Fix config"},
            "uuid": "u1", "timestamp": "2026-03-20T10:00:00Z",
            "userType": "external", "cwd": "/Users/test/proj",
            "sessionId": session_id, "isSidechain": false, "version": "1"
        }),
        serde_json::json!({
            "type": "file-history-snapshot",
            "messageId": "a1",
            "snapshot": {
                "trackedFileBackups": {
                    "src/config.rs": {"backupFileName": "b1", "version": 1, "backupTime": "t"},
                    "src/auth.rs": {"backupFileName": "b2", "version": 1, "backupTime": "t"}
                },
                "messageId": "a1", "timestamp": "2026-03-20T10:00:10Z"
            },
            "isSnapshotUpdate": false
        }),
    ];
    let mut file = std::fs::File::create(&jsonl_path).unwrap();
    for event in &events {
        writeln!(file, "{}", serde_json::to_string(event).unwrap()).unwrap();
    }
    let info = SessionInfo {
        id: session_id.to_string(),
        project: "-Users-test-proj".to_string(),
        path: jsonl_path,
        slug: None,
        created_at: Some(Utc::now()),
        updated_at: Some(Utc::now()),
        message_count: events.len(),
        first_message: Some("Fix config".to_string()),
        project_path: Some("/Users/test/proj".to_string()),
        is_sidechain: false,
        parent_session_id: None,
        agent_id: None,
    };
    index.index_session(&info).unwrap();

    let results = index.search_files("config").unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].file_path, "src/config.rs");

    let results = index.search_files("auth").unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].file_path, "src/auth.rs");

    let results = index.search_files("nonexistent").unwrap();
    assert!(results.is_empty());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ccmux-index`
Expected: FAIL — `search` and `search_files` methods don't exist yet.

- [ ] **Step 3: Implement query.rs**

Write `crates/ccmux-index/src/query.rs`:

```rust
use std::collections::BTreeMap;

use rusqlite::{Connection, params};

use crate::{FileMatch, MessageMatch, SearchQuery, SearchResult};

/// Search messages using FTS5.
pub fn search(
    conn: &Connection,
    query: &SearchQuery,
) -> Result<Vec<SearchResult>, Box<dyn std::error::Error>> {
    let mut sql = String::from(
        "SELECT m.event_uuid, m.role, m.content, m.timestamp, m.session_id,
                snippet(messages_fts, 0, '**', '**', '...', 32) as snippet,
                s.slug, s.project, s.project_path, s.created_at
         FROM messages_fts fts
         JOIN messages m ON m.id = fts.rowid
         JOIN session_index s ON s.session_id = m.session_id
         WHERE messages_fts MATCH ?1"
    );

    let mut param_idx = 2;
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(query.text.clone())];

    if let Some(ref project) = query.project {
        sql.push_str(&format!(" AND s.project_path = ?{param_idx}"));
        param_values.push(Box::new(project.clone()));
        param_idx += 1;
    }
    if let Some(ref after) = query.after {
        sql.push_str(&format!(" AND s.created_at >= ?{param_idx}"));
        param_values.push(Box::new(after.clone()));
        param_idx += 1;
    }
    if let Some(ref before) = query.before {
        sql.push_str(&format!(" AND s.created_at <= ?{param_idx}"));
        param_values.push(Box::new(before.clone()));
        param_idx += 1;
    }

    sql.push_str(&format!(" ORDER BY fts.rank LIMIT ?{param_idx}"));
    param_values.push(Box::new(query.limit as i64));

    let params_refs: Vec<&dyn rusqlite::types::ToSql> = param_values.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_refs.as_slice(), |row| {
        Ok((
            row.get::<_, String>(0)?,  // event_uuid
            row.get::<_, String>(1)?,  // role
            row.get::<_, String>(2)?,  // content
            row.get::<_, String>(3)?,  // timestamp
            row.get::<_, String>(4)?,  // session_id
            row.get::<_, String>(5)?,  // snippet
            row.get::<_, Option<String>>(6)?,  // slug
            row.get::<_, String>(7)?,  // project
            row.get::<_, Option<String>>(8)?,  // project_path
            row.get::<_, Option<String>>(9)?,  // created_at
        ))
    })?;

    // Group matches by session
    let mut session_map: BTreeMap<String, SearchResult> = BTreeMap::new();

    for row in rows {
        let (event_uuid, role, content, timestamp, session_id, snippet, slug, project, project_path, created_at) = row?;

        let entry = session_map.entry(session_id.clone()).or_insert_with(|| SearchResult {
            session_id,
            slug,
            project,
            project_path,
            created_at,
            matches: Vec::new(),
        });

        entry.matches.push(MessageMatch {
            event_uuid,
            role,
            content,
            timestamp,
            snippet,
        });
    }

    Ok(session_map.into_values().collect())
}

/// Search for sessions that touched files matching a pattern.
pub fn search_files(
    conn: &Connection,
    pattern: &str,
) -> Result<Vec<FileMatch>, Box<dyn std::error::Error>> {
    let like_pattern = format!("%{pattern}%");
    let mut stmt = conn.prepare(
        "SELECT session_id, file_path, message_id
         FROM session_files
         WHERE file_path LIKE ?1
         ORDER BY file_path"
    )?;

    let results = stmt.query_map(params![like_pattern], |row| {
        Ok(FileMatch {
            session_id: row.get(0)?,
            file_path: row.get(1)?,
            message_id: row.get(2)?,
        })
    })?;

    results.map(|r| Ok(r?)).collect()
}
```

- [ ] **Step 4: Wire up search methods on SearchIndex**

Add to the `impl SearchIndex` block in `crates/ccmux-index/src/lib.rs`:

```rust
    pub fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>, Box<dyn std::error::Error>> {
        query::search(&self.conn, query)
    }

    pub fn search_files(&self, pattern: &str) -> Result<Vec<FileMatch>, Box<dyn std::error::Error>> {
        query::search_files(&self.conn, pattern)
    }
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p ccmux-index`
Expected: All tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/ccmux-index/
git commit -m "feat: implement FTS5 search queries and file path search"
```

---

## Task 5: Search results markdown rendering

**Files:**
- Modify: `crates/ccmux-core/src/display/markdown.rs`
- Modify: `crates/ccmux-core/src/display/mod.rs` (if needed for re-exports)

- [ ] **Step 1: Write failing test for render_search_results**

Add to the test module in `crates/ccmux-core/src/display/markdown.rs`:

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ccmux-core -- test_render_search`
Expected: FAIL — `render_search_results` doesn't exist.

- [ ] **Step 3: Implement render_search_results**

Add to `crates/ccmux-core/src/display/markdown.rs`:

```rust
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
    let mut out = format!("# Search: \"{query}\"\n{total_matches} results across {total_sessions} session{}\n",
        if total_sessions == 1 { "" } else { "s" });

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
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p ccmux-core -- test_render_search`
Expected: Both tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ccmux-core/
git commit -m "feat: add search results markdown renderer"
```

---

## Task 6: CLI subcommands (serve, index, search)

**Files:**
- Modify: `crates/ccmux-app/Cargo.toml`
- Modify: `crates/ccmux-app/src/main.rs`
- Create: `crates/ccmux-app/src/cli.rs`

**Context:** The app is now a plain Axum HTTP server (no Dioxus, no WASM). `main.rs` uses `#[tokio::main]`, imports only `mod api;`, and calls `axum::serve`. No `cfg` guards needed.

- [ ] **Step 1: Add dependencies to ccmux-app**

In `crates/ccmux-app/Cargo.toml`, add to `[dependencies]`:

```toml
ccmux-index = { path = "../ccmux-index" }
clap = { version = "4", features = ["derive"] }
dirs = "6"
```

Note: Check how the existing `api.rs` resolves the `~/.claude/projects/` base path. If it already uses `dirs` or a helper, reuse that instead of adding `dirs` separately.

- [ ] **Step 2: Create cli.rs with subcommand definitions**

Create `crates/ccmux-app/src/cli.rs`:

```rust
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ccmux", about = "Session log viewer for Claude Code")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Start the web server with background indexing
    Serve,
    /// Build or update the search index and exit
    Index,
    /// Search indexed sessions
    Search {
        /// Search query
        query: String,
        /// Maximum number of results
        #[arg(long, default_value = "20")]
        limit: usize,
        /// Filter to a specific project path
        #[arg(long)]
        project: Option<String>,
        /// Only sessions created after this date (ISO 8601)
        #[arg(long)]
        after: Option<String>,
        /// Only sessions created before this date (ISO 8601)
        #[arg(long)]
        before: Option<String>,
        /// Search file paths instead of message content
        #[arg(long)]
        files: bool,
        /// Output JSON instead of markdown
        #[arg(long)]
        json: bool,
    },
}
```

- [ ] **Step 3: Update main.rs to dispatch subcommands**

Replace the contents of `crates/ccmux-app/src/main.rs` with:

```rust
mod api;
mod cli;

use clap::Parser;
use cli::{Cli, Commands};

fn index_db_path() -> std::path::PathBuf {
    dirs::home_dir()
        .expect("Could not determine home directory")
        .join(".claude/ccmux/index.db")
}

fn claude_projects_path() -> std::path::PathBuf {
    dirs::home_dir()
        .expect("Could not determine home directory")
        .join(".claude/projects")
}

fn run_index() -> Result<ccmux_index::IndexStats, Box<dyn std::error::Error>> {
    let index = ccmux_index::SearchIndex::open(&index_db_path())?;
    index.index_all(&claude_projects_path())
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        None | Some(Commands::Serve) => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "info".into()),
                )
                .init();

            // Start background indexer
            tokio::task::spawn_blocking(|| {
                if let Err(e) = run_index() {
                    tracing::warn!(error = %e, "Background indexing failed");
                }
            });

            let app = api::router();
            let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
                .await
                .expect("failed to bind to port 3000");

            tracing::info!("listening on {}", listener.local_addr().unwrap());
            axum::serve(listener, app).await.expect("server error");
        }
        Some(Commands::Index) => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::from_default_env()
                        .add_directive("ccmux=info".parse().unwrap()),
                )
                .init();

            match run_index() {
                Ok(stats) => {
                    println!(
                        "Indexed {} sessions ({} messages, {} files) in {:.1}s",
                        stats.sessions_indexed,
                        stats.messages_indexed,
                        stats.files_indexed,
                        stats.duration.as_secs_f64()
                    );
                }
                Err(e) => {
                    eprintln!("Indexing failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        Some(Commands::Search {
            query,
            limit,
            project,
            after,
            before,
            files,
            json,
        }) => {
            run_search(query, limit, project, after, before, files, json);
        }
    }
}

fn run_search(
    query: String,
    limit: usize,
    project: Option<String>,
    after: Option<String>,
    before: Option<String>,
    files: bool,
    json: bool,
) {
    let index = match ccmux_index::SearchIndex::open(&index_db_path()) {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!("Failed to open index: {e}");
            eprintln!("Run 'ccmux index' first to build the search index.");
            std::process::exit(1);
        }
    };

    if files {
        match index.search_files(&query) {
            Ok(results) => {
                if json {
                    println!("{}", serde_json::to_string_pretty(&results).unwrap());
                } else {
                    if results.is_empty() {
                        println!("No files matching \"{query}\"");
                        return;
                    }
                    println!("# Files matching \"{query}\"\n");
                    for result in &results {
                        println!("- {} (session: {})", result.file_path, result.session_id);
                    }
                }
            }
            Err(e) => {
                eprintln!("Search failed: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    let search_query = ccmux_index::SearchQuery {
        text: query.clone(),
        project,
        after,
        before,
        limit,
    };

    match index.search(&search_query) {
        Ok(results) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&results).unwrap());
            } else {
                use ccmux_core::display::markdown::{render_search_results, SearchResultGroup};
                use ccmux_core::display::{DisplayItem, DisplayModeF, ItemMeta};

                let total_matches: usize = results.iter().map(|r| r.matches.len()).sum();
                let total_sessions = results.len();

                let groups: Vec<SearchResultGroup> = results.iter().map(|r| {
                    let items: Vec<_> = r.matches.iter().map(|m| {
                        let item = match m.role.as_str() {
                            "user" => DisplayItem::UserMessage {
                                content: m.content.clone(),
                                meta: ItemMeta { uuid: Some(m.event_uuid.clone()), model: None, tokens: None },
                                raw: serde_json::Value::Null,
                                cursor: None,
                            },
                            _ => DisplayItem::AssistantMessage {
                                text: m.content.clone(),
                                meta: ItemMeta { uuid: Some(m.event_uuid.clone()), model: None, tokens: None },
                                raw: serde_json::Value::Null,
                                cursor: None,
                            },
                        };
                        DisplayModeF::Full(item)
                    }).collect();

                    SearchResultGroup {
                        session_id: r.session_id.clone(),
                        slug: r.slug.clone(),
                        project_path: r.project_path.clone(),
                        created_at: r.created_at.clone(),
                        items,
                    }
                }).collect();

                let output = render_search_results(&query, &groups, total_matches, total_sessions);
                print!("{output}");
            }
        }
        Err(e) => {
            eprintln!("Search failed: {e}");
            std::process::exit(1);
        }
    }
}
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check -p ccmux-app`
Expected: Compiles with no errors.

- [ ] **Step 5: Test the CLI help**

Run: `cargo run -p ccmux-app -- --help`
Expected: Shows subcommand help with `serve`, `index`, `search`.

Run: `cargo run -p ccmux-app -- search --help`
Expected: Shows search options (query, --limit, --project, --after, --before, --files, --json).

- [ ] **Step 6: Test the index command against real sessions**

Run: `cargo run -p ccmux-app -- index`
Expected: Prints stats like "Indexed N sessions (M messages, F files) in X.Ys"

- [ ] **Step 7: Test the search command**

Run: `cargo run -p ccmux-app -- search "test"` (use a term likely in your sessions)
Expected: Prints markdown-formatted search results.

Run: `cargo run -p ccmux-app -- search "test" --json`
Expected: Prints JSON array of search results.

- [ ] **Step 8: Commit**

```bash
git add crates/ccmux-app/
git commit -m "feat: add serve, index, search CLI subcommands"
```

---

## Deferred: File watcher integration during serve

The spec describes debounced re-indexing triggered by the existing `notify` file watcher during serve mode. Task 6 implements a one-time `index_all()` on server startup, which handles the "index since last run" case. Integrating with the live file watcher (debounced per-session re-indexing every 30s) is a follow-up task that requires coordinating with the SSE streaming code in `ccmux-app`. It can be added after the core indexing and search are working.

---

## Task 7: Lint, clippy, and final verification

**Files:** All modified crates

- [ ] **Step 1: Run clippy on the whole workspace**

Run: `cargo clippy --workspace`
Expected: No errors. Fix any warnings.

- [ ] **Step 2: Run formatter**

Run: `cargo fmt --all`

- [ ] **Step 3: Run all tests**

Run: `cargo test --workspace`
Expected: All tests pass across all crates.

- [ ] **Step 4: Run the full integration flow**

```bash
# Build the index
cargo run -p ccmux-app -- index

# Search for something
cargo run -p ccmux-app -- search "authentication"

# Search files
cargo run -p ccmux-app -- search "config" --files

# JSON output
cargo run -p ccmux-app -- search "test" --json --limit 5
```

Expected: All commands produce reasonable output.

- [ ] **Step 5: Commit any final fixes**

```bash
git add -A
git commit -m "chore: lint fixes and final cleanup"
```
