use std::collections::BTreeMap;

use dioxus::prelude::*;

use crate::routes::Route;
use crate::server_fns::{SessionMeta, list_sessions};

#[component]
pub fn SessionList() -> Element {
    let sessions_resource = use_server_future(move || list_sessions(None))?;

    match &*sessions_resource.read() {
        Some(Ok(sessions)) => {
            let grouped = group_by_project(sessions);
            rsx! {
                div { class: "session-list",
                    h1 { class: "page-title", "Sessions" }
                    for (project, sessions) in grouped {
                        div { key: "{project}", class: "project-group",
                            h2 { class: "project-name", "{project}" }
                            div { class: "session-cards",
                                for session in sessions {
                                    SessionCard { key: "{session.id}", session: session.clone() }
                                }
                            }
                        }
                    }
                }
            }
        }
        Some(Err(e)) => rsx! {
            div { class: "error", "Error loading sessions: {e}" }
        },
        None => rsx! {
            div { class: "loading", "Loading sessions..." }
        },
    }
}

fn group_by_project(sessions: &[SessionMeta]) -> BTreeMap<String, Vec<&SessionMeta>> {
    let mut groups: BTreeMap<String, Vec<&SessionMeta>> = BTreeMap::new();
    for session in sessions {
        groups
            .entry(session.project.clone())
            .or_default()
            .push(session);
    }
    groups
}

#[component]
fn SessionCard(session: SessionMeta) -> Element {
    let preview = session
        .first_message
        .as_deref()
        .unwrap_or("(no message)")
        .chars()
        .take(120)
        .collect::<String>();

    let updated = session
        .updated_at
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let id = session.id.clone();

    rsx! {
        Link {
            to: Route::SessionView { id },
            class: "session-card",
            div { class: "session-card-preview", "{preview}" }
            div { class: "session-card-meta",
                span { class: "session-card-count", "{session.message_count} events" }
                span { class: "session-card-updated", "{updated}" }
            }
        }
    }
}
