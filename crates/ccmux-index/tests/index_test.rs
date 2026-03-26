use ccmux_index::SearchIndex;
use tempfile::TempDir;

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
