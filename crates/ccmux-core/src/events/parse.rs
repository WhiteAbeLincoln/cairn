use serde::Deserialize;
use serde_json::Value;

use super::*;

/// Parse a single raw JSON value into a typed Event.
pub fn parse_event(raw: &Value) -> Event {
    let event_type = raw.get("type").and_then(|v| v.as_str()).unwrap_or("");

    macro_rules! try_parse {
        ($variant:ident, $data_type:ty) => {
            match <$data_type>::deserialize(raw) {
                Ok(data) => Event::$variant(data),
                Err(e) => {
                    tracing::warn!(
                        event_type,
                        %e,
                        "failed to parse event, falling back to Unknown"
                    );
                    Event::Unknown(raw.clone())
                }
            }
        };
    }

    match event_type {
        "assistant" => try_parse!(Assistant, AssistantEventData),
        "user" => try_parse!(User, UserEventData),
        "system" => try_parse!(System, SystemEventData),
        "progress" => try_parse!(Progress, ProgressEventData),
        "file-history-snapshot" => try_parse!(FileHistory, FileHistoryEventData),
        "queue_operation" => try_parse!(QueueOperation, QueueOperationEventData),
        _ => Event::Unknown(raw.clone()),
    }
}

/// Parse all raw JSON values into typed Events.
pub fn parse_events(raw_events: &[Value]) -> Vec<Event> {
    raw_events.iter().map(parse_event).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_assistant_event() {
        let raw = json!({
            "type": "assistant",
            "cwd": "/tmp",
            "isSidechain": false,
            "sessionId": "s1",
            "timestamp": "2026-01-01T00:00:00Z",
            "userType": "external",
            "uuid": "u1",
            "version": "1",
            "message": {"role": "assistant", "content": [{"type": "text", "text": "hello"}]}
        });
        let event = parse_event(&raw);
        assert!(matches!(event, Event::Assistant(_)));
    }

    #[test]
    fn test_parse_user_event() {
        let raw = json!({
            "type": "user",
            "cwd": "/tmp",
            "isSidechain": false,
            "sessionId": "s1",
            "timestamp": "2026-01-01T00:00:00Z",
            "userType": "external",
            "uuid": "u2",
            "version": "1",
            "message": {"role": "user", "content": "hello"}
        });
        let event = parse_event(&raw);
        assert!(matches!(event, Event::User(_)));
    }

    #[test]
    fn test_parse_unknown_type() {
        let raw = json!({"type": "something_new", "data": 42});
        let event = parse_event(&raw);
        assert!(matches!(event, Event::Unknown(_)));
    }

    #[test]
    fn test_parse_malformed_falls_back_to_unknown() {
        // Missing required fields for assistant
        let raw = json!({"type": "assistant"});
        let event = parse_event(&raw);
        assert!(matches!(event, Event::Unknown(_)));
    }
}
