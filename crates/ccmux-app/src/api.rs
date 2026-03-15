//! Markdown API endpoints for session exploration by AI agents.

#[cfg(not(target_arch = "wasm32"))]
mod handlers {
    use dioxus::server::axum::{
        Router,
        extract::{Path, Query},
        http::{StatusCode, header},
        response::{IntoResponse, Response},
        routing::get,
    };
    use std::collections::HashMap;

    use ccmux_core::display::markdown::{
        SessionListEntry, SessionListGroup, render_event_detail, render_session_list,
        render_session_markdown,
    };
    use ccmux_core::display::pipeline::events_to_display_items_with_offsets;
    use ccmux_core::display::{DisplayOpts, decode_cursor};
    use ccmux_core::events::parse::parse_events;
    use ccmux_core::session::loader;

    fn base_path() -> std::path::PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        std::path::PathBuf::from(home)
            .join(".claude")
            .join("projects")
    }

    fn markdown_response(body: String) -> Response {
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
            body,
        )
            .into_response()
    }

    fn error_response(status: StatusCode, msg: &str) -> Response {
        (
            status,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            msg.to_string(),
        )
            .into_response()
    }

    async fn session_list_handler() -> Response {
        let base = base_path();
        let sessions = match loader::discover_sessions(&base) {
            Ok(s) => s,
            Err(e) => {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("Failed to discover sessions: {e}"),
                );
            }
        };

        let mut groups: Vec<SessionListGroup> = Vec::new();
        for session in sessions
            .iter()
            .filter(|s| !s.is_sidechain && s.first_message.is_some())
        {
            let updated_str = session
                .updated_at
                .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string());
            let entry = SessionListEntry {
                id: session.id.clone(),
                first_message: session.first_message.clone(),
                updated_at: updated_str,
            };
            if let Some(group) = groups.iter_mut().find(|g| g.project == session.project) {
                group.sessions.push(entry);
            } else {
                groups.push(SessionListGroup {
                    project: session.project.clone(),
                    sessions: vec![entry],
                });
            }
        }

        markdown_response(render_session_list(&groups))
    }

    async fn session_markdown_handler(
        Path(id_with_ext): Path<String>,
        Query(params): Query<HashMap<String, String>>,
    ) -> Response {
        // Only handle .md requests; let the Dioxus fallback serve the web UI
        let Some(session_id) = id_with_ext.strip_suffix(".md") else {
            return error_response(StatusCode::NOT_FOUND, "Not found");
        };

        let page: usize = params.get("page").and_then(|v| v.parse().ok()).unwrap_or(1);
        let per_page: usize = params
            .get("per_page")
            .and_then(|v| v.parse().ok())
            .unwrap_or(50);

        if page == 0 {
            return error_response(
                StatusCode::BAD_REQUEST,
                "Invalid parameter: page must be >= 1",
            );
        }
        if per_page == 0 {
            return error_response(
                StatusCode::BAD_REQUEST,
                "Invalid parameter: per_page must be >= 1",
            );
        }

        let base = base_path();
        let sessions = match loader::discover_sessions(&base) {
            Ok(s) => s,
            Err(e) => {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("Failed to discover sessions: {e}"),
                );
            }
        };

        let info = match sessions.iter().find(|s| s.id == session_id) {
            Some(info) => info,
            None => {
                return error_response(
                    StatusCode::NOT_FOUND,
                    &format!("Session not found: {session_id}"),
                );
            }
        };

        let raw_with_offsets = match loader::load_session_raw_with_offsets(&info.path) {
            Ok(r) => r,
            Err(e) => {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("Failed to load session: {e}"),
                );
            }
        };

        let raw_values: Vec<serde_json::Value> =
            raw_with_offsets.iter().map(|(_, v)| v.clone()).collect();
        let events = parse_events(&raw_values);
        let opts = DisplayOpts::markdown();
        let items = events_to_display_items_with_offsets(&events, &raw_with_offsets, &opts);

        let total_items = items.len();
        let total_pages = if total_items == 0 {
            1
        } else {
            total_items.div_ceil(per_page)
        };
        if page > total_pages {
            return error_response(
                StatusCode::NOT_FOUND,
                &format!("Page {page} not found. Session has {total_pages} pages."),
            );
        }

        markdown_response(render_session_markdown(session_id, &items, page, per_page))
    }

    async fn event_detail_handler(
        Path((id, cursor_with_ext)): Path<(String, String)>,
        Query(params): Query<HashMap<String, String>>,
    ) -> Response {
        // Only handle .md requests
        let Some(cursor) = cursor_with_ext.strip_suffix(".md") else {
            return error_response(StatusCode::NOT_FOUND, "Not found");
        };

        let show_metadata = params.get("metadata").map(|v| v == "true").unwrap_or(false);

        let offset = match decode_cursor(cursor) {
            Some(o) => o,
            None => return error_response(StatusCode::BAD_REQUEST, "Invalid cursor"),
        };

        let base = base_path();
        let sessions = match loader::discover_sessions(&base) {
            Ok(s) => s,
            Err(e) => {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("Failed to discover sessions: {e}"),
                );
            }
        };

        let info = match sessions.iter().find(|s| s.id == id) {
            Some(info) => info,
            None => {
                return error_response(StatusCode::NOT_FOUND, &format!("Session not found: {id}"));
            }
        };

        // Seek to offset and read one line
        use std::io::{BufRead, Seek, SeekFrom};
        let file = match std::fs::File::open(&info.path) {
            Ok(f) => f,
            Err(e) => {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("Failed to open session file: {e}"),
                );
            }
        };

        let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);
        if offset >= file_len {
            return error_response(
                StatusCode::BAD_REQUEST,
                "Invalid cursor: offset out of range",
            );
        }

        let mut reader = std::io::BufReader::new(file);
        if reader.seek(SeekFrom::Start(offset)).is_err() {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to seek in session file",
            );
        }

        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
            return error_response(StatusCode::BAD_REQUEST, "Invalid cursor: no data at offset");
        }

        let raw: serde_json::Value = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(_) => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "Invalid cursor: corrupt data at offset",
                );
            }
        };

        markdown_response(render_event_detail(&raw, show_metadata, &id))
    }

    /// Build the Axum router for markdown API endpoints.
    pub fn build_api_router() -> Router {
        Router::new()
            .route("/sessions.md", get(session_list_handler))
            .route("/session/{id_with_ext}", get(session_markdown_handler))
            .route(
                "/session/{id}/event/{cursor_with_ext}",
                get(event_detail_handler),
            )
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use handlers::build_api_router;
