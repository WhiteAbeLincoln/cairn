//! Markdown API endpoints for session exploration by AI agents.

use axum::{
    Router,
    extract::{Path, Query, Request},
    http::{StatusCode, header},
    middleware::{self, Next},
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
use ccmux_core::events::parse::parse_events_refs;
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

fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter(|s| !s.is_empty())
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

const MAX_PER_PAGE: usize = 500;

async fn session_markdown_handler(session_id: &str, query: &str) -> Response {
    let params = parse_query(query);

    let page: usize = match params.get("page") {
        Some(v) => match v.parse() {
            Ok(n) if n >= 1 => n,
            _ => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "Invalid parameter: page must be a positive integer",
                );
            }
        },
        None => 1,
    };
    let per_page: usize = match params.get("per_page") {
        Some(v) => match v.parse::<usize>() {
            Ok(n) if n >= 1 => n.min(MAX_PER_PAGE),
            _ => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "Invalid parameter: per_page must be a positive integer",
                );
            }
        },
        None => 50,
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

    let raw_values: Vec<&serde_json::Value> = raw_with_offsets.iter().map(|(_, v)| v).collect();
    let events = parse_events_refs(&raw_values);
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

/// Middleware that intercepts `/session/{id}.md` requests and routes them
/// to the session markdown handler.
async fn session_md_middleware(req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    if let Some(rest) = path.strip_prefix("/session/")
        && let Some(session_id) = rest.strip_suffix(".md")
        && !session_id.contains('/')
        && !session_id.is_empty()
    {
        let query = req.uri().query().unwrap_or("");
        return session_markdown_handler(session_id, query).await;
    }
    next.run(req).await
}

pub fn router() -> Router {
    Router::new()
        .route("/sessions.md", get(session_list_handler))
        .route(
            "/session/{id}/event/{cursor_with_ext}",
            get(event_detail_handler),
        )
        .layer(middleware::from_fn(session_md_middleware))
}
