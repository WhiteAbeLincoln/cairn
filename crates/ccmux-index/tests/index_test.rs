use ccmux_core::session::SessionInfo;
use ccmux_index::{SearchIndex, SearchQuery};
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
fn test_open_creates_db_and_runs_migrations() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");

    let index = SearchIndex::open(&db_path).unwrap();

    // Verify tables exist by querying them
    let conn = index.conn();
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
    let _index2 = SearchIndex::open(&db_path).unwrap();
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
    let count: i64 = index
        .conn()
        .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 3);

    // Verify roles
    let user_count: i64 = index
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE role = 'user'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(user_count, 2);

    let assistant_count: i64 = index
        .conn()
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
    let count1: i64 = index
        .conn()
        .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
        .unwrap();

    // Append a new user message
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&jsonl_path)
        .unwrap();
    writeln!(
        file,
        "{}",
        serde_json::to_string(&serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": "One more thing about RBAC"},
            "uuid": "u4",
            "timestamp": "2026-03-20T10:05:00Z",
            "userType": "external",
            "cwd": "/Users/test/myproject",
            "sessionId": "test-session-1",
            "isSidechain": false,
            "version": "1"
        }))
        .unwrap()
    )
    .unwrap();

    // Re-index — should only pick up the new message
    index.index_session(&info).unwrap();
    let count2: i64 = index
        .conn()
        .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
        .unwrap();

    assert_eq!(count2, count1 + 1);
}

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

    let count: i64 = index
        .conn()
        .query_row("SELECT COUNT(*) FROM session_files", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 2);

    // Verify specific file paths
    let paths: Vec<String> = {
        let mut stmt = index
            .conn()
            .prepare("SELECT file_path FROM session_files ORDER BY file_path")
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    assert_eq!(paths, vec!["src/config.rs", "src/lib.rs"]);
}

#[test]
fn test_search_finds_matching_messages() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let index = SearchIndex::open(&db_path).unwrap();

    let session_dir = TempDir::new().unwrap();
    let (_jsonl_path, info) = make_test_session(&session_dir);
    index.index_session(&info).unwrap();

    let results = index
        .search(&SearchQuery {
            text: "authentication".to_string(),
            project: None,
            after: None,
            before: None,
            limit: 20,
        })
        .unwrap();

    assert_eq!(results.len(), 1); // One session
    assert_eq!(results[0].session_id, "test-session-1");
    assert!(results[0].matches.len() >= 1);
}

#[test]
fn test_search_no_results() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let index = SearchIndex::open(&db_path).unwrap();

    let session_dir = TempDir::new().unwrap();
    let (_jsonl_path, info) = make_test_session(&session_dir);
    index.index_session(&info).unwrap();

    let results = index
        .search(&SearchQuery {
            text: "kubernetes".to_string(),
            project: None,
            after: None,
            before: None,
            limit: 20,
        })
        .unwrap();

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
    let results = index
        .search(&SearchQuery {
            text: "authentication".to_string(),
            project: Some("/Users/test/myproject".to_string()),
            after: None,
            before: None,
            limit: 20,
        })
        .unwrap();
    assert_eq!(results.len(), 1);

    // Search with non-matching project
    let results = index
        .search(&SearchQuery {
            text: "authentication".to_string(),
            project: Some("/Users/test/other".to_string()),
            after: None,
            before: None,
            limit: 20,
        })
        .unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_search_files() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let index = SearchIndex::open(&db_path).unwrap();

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
