use dioxus::prelude::*;

use crate::routes::Route;
use crate::server_fns::{SessionMeta, list_sessions};

#[component]
pub fn SessionList() -> Element {
    let sessions_resource = use_server_future(move || list_sessions(None))?;

    match &*sessions_resource.read() {
        Some(Ok(groups)) => {
            rsx! {
                div { class: "session-list",
                    h1 { class: "page-title", "Sessions" }
                    for group in groups {
                        ProjectGroup { key: "{group.project}", project: group.project.clone(), sessions: group.sessions.clone() }
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

#[component]
fn ProjectGroup(project: String, sessions: Vec<SessionMeta>) -> Element {
    let mut collapsed = use_signal(|| false);
    let count = sessions.len();

    rsx! {
        div { class: "project-group",
            button {
                class: "project-header",
                onclick: move |_| collapsed.toggle(),
                span { class: "project-toggle",
                    if collapsed() { "\u{25B8}" } else { "\u{25BE}" }
                }
                span { class: "project-name", "{project}" }
                span { class: "session-count-badge", "{count}" }
            }
            if !collapsed() {
                div { class: "session-rows",
                    for session in &sessions {
                        SessionRow { key: "{session.id}", session: session.clone() }
                    }
                }
            }
        }
    }
}

#[component]
fn SessionRow(session: SessionMeta) -> Element {
    let preview = session
        .first_message
        .as_deref()
        .unwrap_or("\u{2014}")
        .chars()
        .take(120)
        .collect::<String>();

    let updated = session
        .updated_at
        .map(|dt| {
            let local = dt.with_timezone(&chrono::Local);
            local.format("%-m/%-d/%Y, %-I:%M:%S %p").to_string()
        })
        .unwrap_or_else(|| "unknown".to_string());

    let id = session.id.clone();

    rsx! {
        Link {
            to: Route::SessionView { id },
            class: "session-row",
            div { class: "session-row-summary", "{preview}" }
            div { class: "session-row-meta",
                span { class: "session-row-date", "{updated}" }
                span { class: "session-row-count", "{session.message_count} msgs" }
            }
        }
    }
}
