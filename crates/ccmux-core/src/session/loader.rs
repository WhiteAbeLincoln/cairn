use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

/// Metadata about a discovered session, without loading all events.
#[derive(Debug)]
pub struct SessionInfo {
    pub id: String,
    pub project: String,
    pub path: PathBuf,
    pub slug: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub message_count: usize,
    pub first_message: Option<String>,
    /// The real project directory path, extracted from the `cwd` field in events.
    pub project_path: Option<String>,
    /// True if this is a subagent/sidechain session.
    pub is_sidechain: bool,
    /// For sidechain sessions, the parent session ID.
    pub parent_session_id: Option<String>,
    /// For sidechain sessions, the agent ID.
    pub agent_id: Option<String>,
}

/// Discover all session JSONL files under the Claude projects directory.
pub fn discover_sessions(base_path: &Path) -> std::io::Result<Vec<SessionInfo>> {
    let mut sessions = Vec::new();

    if !base_path.is_dir() {
        return Ok(sessions);
    }

    // Iterate project directories: ~/.claude/projects/<project-path>/
    for project_entry in std::fs::read_dir(base_path)? {
        let project_entry = project_entry?;
        let project_path = project_entry.path();
        if !project_path.is_dir() {
            continue;
        }

        let project_name = project_entry.file_name().to_string_lossy().into_owned();

        // Find .jsonl files directly in the project directory
        for file_entry in std::fs::read_dir(&project_path)? {
            let file_entry = file_entry?;
            let file_path = file_entry.path();

            if file_path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }

            let session_id = file_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();

            // Quick scan: read a few lines to extract metadata
            match scan_session_metadata(&file_path) {
                Ok(meta) => {
                    let sid = session_id.clone();
                    sessions.push(SessionInfo {
                        id: session_id,
                        project: project_name.clone(),
                        path: file_path,
                        slug: meta.slug,
                        created_at: meta.first_timestamp,
                        updated_at: meta.last_timestamp,
                        message_count: meta.line_count,
                        first_message: meta.first_message,
                        project_path: meta.project_path,
                        is_sidechain: false,
                        parent_session_id: None,
                        agent_id: None,
                    });

                    // Scan for subagent sessions in <session-id>/subagents/
                    let subagents_dir = project_path.join(&sid).join("subagents");
                    if subagents_dir.is_dir()
                        && let Ok(entries) = std::fs::read_dir(&subagents_dir)
                    {
                        for entry in entries.flatten() {
                            let path = entry.path();
                            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                                continue;
                            }
                            let stem = path
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .unwrap_or("")
                                .to_string();
                            // Extract agentId from filename: "agent-<agentId>"
                            let agent_id = stem.strip_prefix("agent-").map(|s| s.to_string());
                            match scan_session_metadata(&path) {
                                Ok(sub_meta) => {
                                    sessions.push(SessionInfo {
                                        id: stem.clone(),
                                        project: project_name.clone(),
                                        path,
                                        slug: sub_meta.slug,
                                        created_at: sub_meta.first_timestamp,
                                        updated_at: sub_meta.last_timestamp,
                                        message_count: sub_meta.line_count,
                                        first_message: sub_meta.first_message,
                                        project_path: sub_meta.project_path,
                                        is_sidechain: true,
                                        parent_session_id: Some(sid.clone()),
                                        agent_id,
                                    });
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        subagent = %stem,
                                        error = %e,
                                        "Failed to scan subagent",
                                    );
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(path = %file_path.display(), error = %e, "Failed to scan session");
                }
            }
        }
    }

    // Sort by updated_at descending (most recent first)
    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    Ok(sessions)
}

struct SessionMeta {
    slug: Option<String>,
    first_timestamp: Option<DateTime<Utc>>,
    last_timestamp: Option<DateTime<Utc>>,
    line_count: usize,
    first_message: Option<String>,
    project_path: Option<String>,
}

/// Quick scan of a session file to extract slug, timestamps, and line count
/// without fully parsing every event.
fn scan_session_metadata(path: &Path) -> std::io::Result<SessionMeta> {
    use std::io::BufRead;
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);

    let mut slug = None;
    let mut first_timestamp = None;
    let mut last_timestamp = None;
    let mut line_count = 0;
    let mut first_message = None;
    let mut project_path = None;

    for line in reader.lines() {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        line_count += 1;

        // Lightweight JSON field extraction without full deserialization
        if slug.is_none()
            && let Some(s) = extract_json_string(&line, "slug")
        {
            slug = Some(s);
        }
        if let Some(ts) = extract_json_string(&line, "timestamp")
            && let Ok(dt) = ts.parse::<DateTime<Utc>>()
        {
            if first_timestamp.is_none() {
                first_timestamp = Some(dt);
            }
            last_timestamp = Some(dt);
        }
        if project_path.is_none()
            && let Some(cwd) = extract_json_string(&line, "cwd")
        {
            project_path = Some(cwd);
        }
        // Extract first user message text (the first user prompt)
        if first_message.is_none()
            && line.contains("\"type\":\"user\"")
            && !line.contains("\"toolUseResult\"")
            && let Some(text) = extract_user_content_string(&line)
        {
            first_message = Some(text);
        }
    }

    Ok(SessionMeta {
        slug,
        first_timestamp,
        last_timestamp,
        line_count,
        first_message,
        project_path,
    })
}

/// Extract the content string from a user message line.
/// Looks for `"content":"<text>"` pattern (string content, not array).
fn extract_user_content_string(line: &str) -> Option<String> {
    let pattern = "\"content\":\"";
    let start = line.find(pattern)? + pattern.len();
    let rest = &line[start..];
    // Find the closing quote, handling escaped quotes
    let mut end = 0;
    let bytes = rest.as_bytes();
    while end < bytes.len() {
        if bytes[end] == b'\\' {
            end += 2; // skip escaped char
        } else if bytes[end] == b'"' {
            break;
        } else {
            end += 1;
        }
    }
    if end == 0 || end >= bytes.len() {
        return None;
    }
    // Unescape basic sequences
    let raw = &rest[..end];
    let unescaped = raw
        .replace("\\n", " ")
        .replace("\\t", " ")
        .replace("\\\"", "\"")
        .replace("\\\\", "\\");
    Some(unescaped)
}

/// Extract a string value for a given key from a JSON line without full parsing.
fn extract_json_string(line: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\":\"", key);
    let start = line.find(&pattern)? + pattern.len();
    let rest = &line[start..];
    // Find closing quote, handling escaped quotes
    let mut end = 0;
    let bytes = rest.as_bytes();
    while end < bytes.len() {
        if bytes[end] == b'\\' {
            end += 2; // skip escaped char
        } else if bytes[end] == b'"' {
            break;
        } else {
            end += 1;
        }
    }
    if end >= bytes.len() {
        return None;
    }
    let raw = &rest[..end];
    let unescaped = raw.replace("\\\"", "\"").replace("\\\\", "\\");
    Some(unescaped)
}

/// Extract agent mappings (parentToolUseID -> agentId) from progress events.
pub fn extract_agent_map(path: &Path) -> std::io::Result<Vec<(String, String)>> {
    use std::io::BufRead;
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);

    let mut seen = std::collections::HashSet::new();
    let mut mappings = Vec::new();

    for line in reader.lines() {
        let line = line?;
        if !line.contains("\"agent_progress\"") {
            continue;
        }
        // Extract parentToolUseID and agentId
        if let (Some(parent_id), Some(agent_id)) = (
            extract_json_string(&line, "parentToolUseID"),
            extract_json_string(&line, "agentId"),
        ) && seen.insert(parent_id.clone())
        {
            mappings.push((parent_id, agent_id));
        }
    }

    Ok(mappings)
}

/// Load all events from a session JSONL file as raw JSON values.
pub fn load_session_raw(path: &Path) -> std::io::Result<Vec<serde_json::Value>> {
    use std::io::BufRead;
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);

    let mut events = Vec::new();
    for (line_num, line) in reader.lines().enumerate() {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<serde_json::Value>(&line) {
            Ok(value) => events.push(value),
            Err(e) => {
                tracing::warn!(line = line_num + 1, error = %e, "Failed to parse JSONL line");
            }
        }
    }
    Ok(events)
}

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
