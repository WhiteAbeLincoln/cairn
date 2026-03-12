use dioxus::prelude::*;

use crate::components::blocks::display_item::DisplayItemView;
use crate::server_fns::get_session;

#[component]
pub fn SessionView(id: String) -> Element {
    let session_id = id.clone();
    let session_resource = use_server_future(move || {
        let sid = session_id.clone();
        async move { get_session(sid).await }
    })?;

    match &*session_resource.read() {
        Some(Ok(response)) => {
            let project = &response.meta.project;
            let count = response.items.len();
            rsx! {
                div { class: "session-view",
                    div { class: "session-header",
                        h1 { class: "session-title", "{project}" }
                        span { class: "session-item-count", "{count} items" }
                    }
                    div { class: "session-items",
                        for (i, item) in response.items.iter().enumerate() {
                            DisplayItemView { key: "{i}", item: item.clone() }
                        }
                    }
                }
            }
        }
        Some(Err(e)) => rsx! {
            div { class: "error", "Error loading session: {e}" }
        },
        None => rsx! {
            div { class: "loading", "Loading session..." }
        },
    }
}
