use std::collections::BTreeMap;

use rusqlite::Connection;

use crate::{FileMatch, MessageMatch, SearchQuery, SearchResult};

/// Search the FTS5 index for messages matching the query.
pub fn search(
    conn: &Connection,
    query: &SearchQuery,
) -> Result<Vec<SearchResult>, Box<dyn std::error::Error>> {
    // Build dynamic SQL with optional filters
    let mut conditions = vec!["messages_fts MATCH ?1".to_string()];
    let mut param_idx = 2usize;

    let project_idx = if query.project.is_some() {
        let idx = param_idx;
        param_idx += 1;
        conditions.push(format!("s.project_path = ?{idx}"));
        Some(idx)
    } else {
        None
    };

    let after_idx = if query.after.is_some() {
        let idx = param_idx;
        param_idx += 1;
        conditions.push(format!("s.created_at >= ?{idx}"));
        Some(idx)
    } else {
        None
    };

    let before_idx = if query.before.is_some() {
        let idx = param_idx;
        param_idx += 1;
        conditions.push(format!("s.created_at <= ?{idx}"));
        Some(idx)
    } else {
        None
    };

    let limit_idx = param_idx;

    let where_clause = conditions.join("\n  AND ");

    let sql = format!(
        "SELECT m.event_uuid, m.role, m.content, m.timestamp, m.session_id,
                snippet(messages_fts, 0, '**', '**', '...', 32) as snippet,
                s.slug, s.project, s.project_path, s.created_at
         FROM messages_fts fts
         JOIN messages m ON m.id = fts.rowid
         JOIN session_index s ON s.session_id = m.session_id
         WHERE {where_clause}
         ORDER BY fts.rank
         LIMIT ?{limit_idx}"
    );

    let mut stmt = conn.prepare(&sql)?;

    // Bind parameters positionally
    stmt.raw_bind_parameter(1, &query.text)?;
    if let (Some(idx), Some(val)) = (project_idx, &query.project) {
        stmt.raw_bind_parameter(idx, val.as_str())?;
    }
    if let (Some(idx), Some(val)) = (after_idx, &query.after) {
        stmt.raw_bind_parameter(idx, val.as_str())?;
    }
    if let (Some(idx), Some(val)) = (before_idx, &query.before) {
        stmt.raw_bind_parameter(idx, val.as_str())?;
    }
    stmt.raw_bind_parameter(limit_idx, query.limit as i64)?;

    let mut rows = stmt.raw_query();

    // Group results by session_id using BTreeMap for deterministic ordering
    let mut sessions: BTreeMap<String, SearchResult> = BTreeMap::new();

    while let Some(row) = rows.next()? {
        let event_uuid: String = row.get(0)?;
        let role: String = row.get(1)?;
        let content: String = row.get(2)?;
        let timestamp: String = row.get(3)?;
        let session_id: String = row.get(4)?;
        let snippet: String = row.get(5)?;
        let slug: Option<String> = row.get(6)?;
        let project: String = row.get(7)?;
        let project_path: Option<String> = row.get(8)?;
        let created_at: Option<String> = row.get(9)?;

        let result = sessions
            .entry(session_id.clone())
            .or_insert_with(|| SearchResult {
                session_id: session_id.clone(),
                slug,
                project,
                project_path,
                created_at,
                matches: Vec::new(),
            });

        result.matches.push(MessageMatch {
            event_uuid,
            role,
            content,
            timestamp,
            snippet,
        });
    }

    Ok(sessions.into_values().collect())
}

/// Search session_files for file paths matching the given pattern (substring match).
pub fn search_files(
    conn: &Connection,
    pattern: &str,
) -> Result<Vec<FileMatch>, Box<dyn std::error::Error>> {
    let like_pattern = format!("%{pattern}%");

    let mut stmt = conn.prepare(
        "SELECT session_id, file_path, message_id
         FROM session_files
         WHERE file_path LIKE ?1
         ORDER BY file_path",
    )?;

    let results = stmt
        .query_map([&like_pattern], |row| {
            Ok(FileMatch {
                session_id: row.get(0)?,
                file_path: row.get(1)?,
                message_id: row.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(results)
}
