use dioxus::prelude::*;

use ccmux_core::display::DisplayItem;
use ccmux_core::display::streaming::StreamEvent;

use crate::components::blocks::display_item::DisplayItemView;
use crate::server_fns::{get_session, stream_session_events};

/// Apply a stream event to a mutable list of display items.
fn apply_stream_event(items: &mut Vec<DisplayItem>, event: StreamEvent) {
    match event {
        StreamEvent::Append { item } => {
            items.push(item);
        }
        StreamEvent::MergeWithPrevious { item } => {
            if let Some(last) = items.last_mut() {
                match last {
                    DisplayItem::Group { items: group_items } => {
                        group_items.push(item);
                    }
                    _ => {
                        // Wrap the previous item and the new one into a Group
                        let prev = std::mem::replace(
                            last,
                            DisplayItem::Group {
                                items: Vec::with_capacity(2),
                            },
                        );
                        if let DisplayItem::Group { items: group_items } = last {
                            group_items.push(prev);
                            group_items.push(item);
                        }
                    }
                }
            } else {
                // No previous item, just append
                items.push(item);
            }
        }
        StreamEvent::UpdateToolResult {
            tool_use_id,
            result,
        } => {
            update_tool_result_recursive(items, &tool_use_id, &result);
        }
        StreamEvent::UpdateTask { task } => {
            // Find and update a matching task in any TaskList
            for item in items.iter_mut() {
                match item {
                    DisplayItem::TaskList { tasks, .. } => {
                        if let Some(existing) = tasks.iter_mut().find(|t| t.id == task.id) {
                            existing.status = task.status.clone();
                            if !task.subject.is_empty() {
                                existing.subject = task.subject.clone();
                            }
                        }
                    }
                    DisplayItem::Group { items: group_items } => {
                        for group_item in group_items.iter_mut() {
                            if let DisplayItem::TaskList { tasks, .. } = group_item
                                && let Some(existing) = tasks.iter_mut().find(|t| t.id == task.id)
                            {
                                existing.status = task.status.clone();
                                if !task.subject.is_empty() {
                                    existing.subject = task.subject.clone();
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Recursively find a ToolUse by id and update its result.
fn update_tool_result_recursive(
    items: &mut [DisplayItem],
    tool_use_id: &str,
    result: &ccmux_core::display::ToolResultData,
) {
    for item in items.iter_mut() {
        match item {
            DisplayItem::ToolUse {
                tool_use_id: id,
                result: r,
                ..
            } if id == tool_use_id => {
                *r = Some(result.clone());
                return;
            }
            DisplayItem::Group { items: group_items } => {
                update_tool_result_recursive(group_items, tool_use_id, result);
            }
            _ => {}
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

    // Signal to hold live-updated items (initially None, set after load)
    let mut live_items: Signal<Option<Vec<DisplayItem>>> = use_signal(|| None);

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

    match &*session_resource.read() {
        Some(Ok(response)) => {
            let project = response.meta.project.clone();
            let items = live_items
                .read()
                .as_ref()
                .cloned()
                .unwrap_or_else(|| response.items.clone());
            let count = items.len();
            rsx! {
                div { class: "session-view",
                    div { class: "session-header",
                        h1 { class: "session-title", "{project}" }
                        span { class: "session-item-count", "{count} items" }
                    }
                    div { class: "session-items",
                        for (i, item) in items.iter().enumerate() {
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
