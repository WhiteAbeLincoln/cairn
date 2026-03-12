use dioxus::prelude::*;

use ccmux_core::display::streaming::StreamEvent;
use ccmux_core::display::{DisplayItem, DisplayItemWithMode, DisplayModeF};

use crate::components::blocks::display_item::DisplayItemModedView;
use crate::routes::Route;
use crate::server_fns::{get_session, stream_session_events};

/// Global context for raw mode toggle (show raw JSON for all blocks).
#[derive(Clone, Copy)]
pub struct RawModeContext {
    pub global_raw: Signal<bool>,
}

/// Apply a stream event to a mutable list of display items.
fn apply_stream_event(items: &mut Vec<DisplayItemWithMode>, event: StreamEvent) {
    match event {
        StreamEvent::Append { item } => {
            // Merge consecutive Collapsed items into a Grouped
            if matches!(item, DisplayModeF::Collapsed(_))
                && let Some(last) = items.last_mut() {
                    match last {
                        DisplayModeF::Grouped(group_items) => {
                            if let DisplayModeF::Collapsed(inner) = item {
                                group_items.push(inner);
                            }
                            return;
                        }
                        DisplayModeF::Collapsed(_) => {
                            let prev = std::mem::replace(
                                last,
                                DisplayModeF::Grouped(Vec::with_capacity(2)),
                            );
                            if let (
                                DisplayModeF::Grouped(group_items),
                                DisplayModeF::Collapsed(prev_inner),
                                DisplayModeF::Collapsed(new_inner),
                            ) = (last, prev, item)
                            {
                                group_items.push(prev_inner);
                                group_items.push(new_inner);
                            }
                            return;
                        }
                        _ => {}
                    }
                }
            items.push(item);
        }
        StreamEvent::UpdateToolResult {
            tool_use_id,
            result,
        } => {
            update_tool_result(items, &tool_use_id, &result);
        }
    }
}

/// Find a ToolUse by id across all items and update its result.
fn update_tool_result(
    items: &mut [DisplayItemWithMode],
    tool_use_id: &str,
    result: &ccmux_core::display::ToolResultData,
) {
    for entry in items.iter_mut() {
        match entry {
            DisplayModeF::Full(item)
            | DisplayModeF::Collapsed(item)
            | DisplayModeF::Hidden(item) => {
                if let DisplayItem::ToolUse {
                    tool_use_id: id,
                    result: r,
                    ..
                } = item
                    && id == tool_use_id {
                        *r = Some(result.clone());
                        return;
                    }
            }
            DisplayModeF::Grouped(group_items) => {
                for item in group_items.iter_mut() {
                    if let DisplayItem::ToolUse {
                        tool_use_id: id,
                        result: r,
                        ..
                    } = item
                        && id == tool_use_id {
                            *r = Some(result.clone());
                            return;
                        }
                }
            }
        }
    }
}

#[component]
pub fn SessionView(id: String) -> Element {
    let session_id = id.clone();
    let session_resource = use_server_future(move || {
        let sid = session_id.clone();
        async move { get_session(sid).await }
    })?;

    // Global raw mode signal
    let mut global_raw = use_signal(|| false);
    use_context_provider(|| RawModeContext { global_raw });

    // Jump-to-bottom FAB state
    let mut show_fab = use_signal(|| false);
    // Debounce flag: skip spawning a new scroll eval if one is already in-flight.
    let mut eval_pending = use_signal(|| false);

    // Signal to hold live-updated items (initially None, set after load)
    let mut live_items: Signal<Option<Vec<DisplayItemWithMode>>> = use_signal(|| None);

    // Start streaming after initial load completes
    let stream_id = id.clone();
    use_effect(move || {
        let response = session_resource.read();
        if let Some(Ok(resp)) = &*response {
            // Initialize live items from the loaded data
            if live_items.peek().is_none() {
                live_items.set(Some(resp.items.clone()));

                // Spawn the streaming coroutine
                let sid = stream_id.clone();
                spawn(async move {
                    let result: Result<
                        dioxus::fullstack::ServerEvents<StreamEvent>,
                        dioxus::prelude::ServerFnError,
                    > = stream_session_events(sid).await;
                    match result {
                        Ok(mut events) => {
                            while let Some(Ok(event)) = events.recv().await {
                                live_items.with_mut(|items| {
                                    if let Some(items) = items {
                                        apply_stream_event(items, event);
                                    }
                                });
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Streaming error: {e}");
                        }
                    }
                });
            }
        }
    });

    let short_id = id[..id.len().min(8)].to_string();

    match &*session_resource.read() {
        Some(Ok(response)) => {
            let items = live_items
                .read()
                .as_ref()
                .cloned()
                .unwrap_or_else(|| response.items.clone());
            rsx! {
                div { class: "session-view",
                    div { class: "session-header",
                        Link { class: "session-back-link", to: Route::SessionList {},
                            "\u{2190} Back"
                        }
                        h1 { class: "session-title", "Session {short_id}" }
                        span { class: "session-header-spacer" }
                        button {
                            class: if global_raw() { "session-raw-toggle session-raw-toggle-active" } else { "session-raw-toggle" },
                            onclick: move |_| {
                                global_raw.toggle();
                            },
                            "Raw"
                        }
                    }
                    div {
                        class: "session-items",
                        onscroll: move |_evt| {
                            if !eval_pending() {
                                eval_pending.set(true);
                                spawn(async move {
                                    let result = dioxus::document::eval(
                                        r#"
                                        let el = document.querySelector('.session-items');
                                        el.scrollTop + el.clientHeight < el.scrollHeight - 200
                                    "#,
                                    )
                                    .await;
                                    if let Ok(val) = result {
                                        show_fab.set(val.as_bool().unwrap_or(false));
                                    }
                                    eval_pending.set(false);
                                });
                            }
                        },
                        for (i, item) in items.iter().enumerate() {
                            DisplayItemModedView { key: "{i}", item: item.clone() }
                        }
                    }
                    if show_fab() {
                        div {
                            class: "scroll-fab",
                            onclick: move |_| {
                                spawn(async move {
                                    let _ = dioxus::document::eval(
                                        r#"
                                        let el = document.querySelector('.session-items');
                                        el.scrollTo({ top: el.scrollHeight, behavior: 'smooth' })
                                    "#,
                                    )
                                    .await;
                                });
                            },
                            "\u{2193}"
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
