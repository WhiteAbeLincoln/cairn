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
                    let project =
                        unescape_project_name(&project_name, meta.project_path.as_deref());
                    sessions.push(SessionInfo {
                        id: session_id,
                        project,
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
                                    let sub_project = unescape_project_name(
                                        &project_name,
                                        sub_meta.project_path.as_deref(),
                                    );
                                    sessions.push(SessionInfo {
                                        id: stem.clone(),
                                        project: sub_project,
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
        // Extract first real user message text (skip CLI/tool messages)
        if first_message.is_none()
            && line.contains("\"type\":\"user\"")
            && line.contains("\"userType\":\"external\"")
            && !line.contains("\"toolUseResult\"")
            && !line.contains("\"sourceToolAssistantUUID\"")
            && let Some(text) = extract_user_content_string(&line)
            && !text.starts_with("<local-command")
            && !text.starts_with("<local_command")
        {
            let cleaned = strip_xml_tags(&text);
            let truncated = if cleaned.len() > 200 {
                format!("{}...", &cleaned[..cleaned.floor_char_boundary(200)])
            } else {
                cleaned
            };
            first_message = Some(truncated);
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

/// Strip XML-style tags from a string, keeping only the text content.
fn strip_xml_tags(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    result.trim().to_string()
}

/// Convert dash-escaped Claude project directory name to a path.
///
/// Claude stores projects as directories with path separators replaced by dashes,
/// e.g. `-Users-alice-myproject`. The real project path is always preferred when
/// available via `project_path` (extracted from event `cwd` fields).
pub fn unescape_project_name(dir_name: &str, project_path: Option<&str>) -> String {
    if let Some(path) = project_path {
        return path.to_string();
    }
    if dir_name.starts_with('-') {
        dir_name.replacen('-', "/", 1).replace('-', "/")
    } else {
        dir_name.to_string()
    }
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

/// Load all events from a session JSONL file as raw JSON values, paired with byte offsets.
/// Each tuple contains (byte_offset, json_value) where byte_offset is the position
/// of the line's first byte in the file.
pub fn load_session_raw_with_offsets(
    path: &Path,
) -> std::io::Result<Vec<(u64, serde_json::Value)>> {
    use std::io::BufRead;
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);

    let mut events = Vec::new();
    let mut offset: u64 = 0;
    let mut line = String::new();
    let mut line_num = 0;

    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line)?;
        if bytes_read == 0 {
            break;
        }
        line_num += 1;
        let line_offset = offset;
        offset += bytes_read as u64;

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(value) => events.push((line_offset, value)),
            Err(e) => {
                tracing::warn!(line = line_num, error = %e, "Failed to parse JSONL line");
            }
        }
    }
    Ok(events)
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
        assert_eq!(sessions[0].project, "/tmp");
        assert_eq!(sessions[0].first_message, Some("hello".to_string()));
    }

    #[test]
    fn test_first_message_filtering() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = dir.path().join("-Users-alice-myproject");
        std::fs::create_dir_all(&project_dir).unwrap();

        let session_path = project_dir.join("sess1.jsonl");
        let mut f = std::fs::File::create(&session_path).unwrap();
        // Line 1: local-command-caveat user event (should be skipped)
        writeln!(f, r#"{{"type":"user","timestamp":"2026-01-01T00:00:00Z","message":{{"role":"user","content":"<local-command-caveat>some caveat</local-command-caveat>"}},"userType":"external","sessionId":"sess1","uuid":"u1","version":"1","isSidechain":false}}"#).unwrap();
        // Line 2: tool_result user event (should be skipped)
        writeln!(f, r#"{{"type":"user","timestamp":"2026-01-01T00:00:01Z","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t1","content":"output"}}]}},"toolUseResult":true,"userType":"external","sessionId":"sess1","uuid":"u2","version":"1","isSidechain":false}}"#).unwrap();
        // Line 3: real external user message (should be selected)
        writeln!(f, r#"{{"type":"user","timestamp":"2026-01-01T00:00:02Z","message":{{"role":"user","content":"hello world"}},"userType":"external","sessionId":"sess1","uuid":"u3","version":"1","isSidechain":false}}"#).unwrap();

        let sessions = discover_sessions(dir.path()).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].first_message, Some("hello world".to_string()));
    }

    #[test]
    fn test_first_message_skips_source_tool_assistant_uuid() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = dir.path().join("myproject");
        std::fs::create_dir_all(&project_dir).unwrap();

        let session_path = project_dir.join("sess2.jsonl");
        let mut f = std::fs::File::create(&session_path).unwrap();
        // Sidechain user event (has sourceToolAssistantUUID, should be skipped)
        writeln!(f, r#"{{"type":"user","timestamp":"2026-01-01T00:00:00Z","message":{{"role":"user","content":"sidechain input"}},"userType":"external","sourceToolAssistantUUID":"some-uuid","sessionId":"sess2","uuid":"u1","version":"1","isSidechain":false}}"#).unwrap();
        // Real user message
        writeln!(f, r#"{{"type":"user","timestamp":"2026-01-01T00:00:01Z","message":{{"role":"user","content":"real message"}},"userType":"external","sessionId":"sess2","uuid":"u2","version":"1","isSidechain":false}}"#).unwrap();

        let sessions = discover_sessions(dir.path()).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].first_message, Some("real message".to_string()));
    }

    #[test]
    fn test_unescape_project_name_with_project_path() {
        // project_path always wins
        let result =
            unescape_project_name("-Users-alice-myproject", Some("/Users/alice/myproject"));
        assert_eq!(result, "/Users/alice/myproject");
    }

    #[test]
    fn test_unescape_project_name_dash_heuristic() {
        // Leading dash → convert all dashes to slashes
        let result = unescape_project_name("-Users-alice-myproject", None);
        assert_eq!(result, "/Users/alice/myproject");
    }

    #[test]
    fn test_unescape_project_name_no_leading_dash() {
        // No leading dash → return as-is
        let result = unescape_project_name("myproject", None);
        assert_eq!(result, "myproject");
    }

    #[test]
    fn test_strip_xml_tags() {
        assert_eq!(
            strip_xml_tags("<local-command-caveat>text</local-command-caveat>"),
            "text"
        );
        assert_eq!(strip_xml_tags("hello world"), "hello world");
        assert_eq!(strip_xml_tags("  plain  "), "plain");
        assert_eq!(strip_xml_tags("<tag>a</tag> and <b>b</b>"), "a and b");
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

    #[test]
    fn test_load_session_raw_with_offsets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"type":"user","msg":"hello"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","msg":"hi"}}"#).unwrap();

        let events = load_session_raw_with_offsets(&path).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].0, 0); // first line starts at byte 0
        assert!(events[1].0 > 0); // second line starts after first
        assert_eq!(events[0].1["type"], "user");
        assert_eq!(events[1].1["type"], "assistant");
    }

    #[test]
    fn test_load_session_raw_with_offsets_skips_empty_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"type":"user","msg":"hello"}}"#).unwrap();
        writeln!(f).unwrap(); // empty line
        writeln!(f, r#"{{"type":"assistant","msg":"hi"}}"#).unwrap();

        let events = load_session_raw_with_offsets(&path).unwrap();
        assert_eq!(events.len(), 2);
    }
}
