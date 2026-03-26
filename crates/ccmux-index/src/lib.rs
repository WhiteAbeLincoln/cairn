pub mod db;
pub mod indexer;
pub mod query;

use std::path::Path;
use std::time::Duration;

use ccmux_core::session::SessionInfo;
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

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn index_session(&self, info: &SessionInfo) -> Result<(), Box<dyn std::error::Error>> {
        indexer::index_session(&self.conn, info)?;
        Ok(())
    }

    pub fn index_all(&self, base_path: &Path) -> Result<IndexStats, Box<dyn std::error::Error>> {
        indexer::index_all(&self.conn, base_path)
    }

    pub fn search(
        &self,
        query: &SearchQuery,
    ) -> Result<Vec<SearchResult>, Box<dyn std::error::Error>> {
        query::search(&self.conn, query)
    }

    pub fn search_files(
        &self,
        pattern: &str,
    ) -> Result<Vec<FileMatch>, Box<dyn std::error::Error>> {
        query::search_files(&self.conn, pattern)
    }
}
