use dioxus::prelude::*;

use ccmux_core::display::DisplayItem;

use super::group::GroupBlock;
use super::message::MessageBlock;
use super::prose::Prose;
use super::thinking::ThinkingBlock;
use super::tool_use::ToolUseBlock;

#[component]
pub fn DisplayItemView(item: DisplayItem) -> Element {
    match item {
        DisplayItem::UserMessage { content, meta, raw } => rsx! {
            MessageBlock {
                label: "User",
                border_class: "border-user",
                meta,
                raw,

                default_open: true,
                Prose { content }
            }
        },
        DisplayItem::AssistantMessage { text, meta, raw } => rsx! {
            MessageBlock {
                label: "Assistant",
                border_class: "border-assistant",
                meta,
                raw,

                default_open: true,
                Prose { content: text }
            }
        },
        DisplayItem::Thinking { text, .. } => rsx! {
            ThinkingBlock { text }
        },
        DisplayItem::ToolUse {
            name,
            input,
            result,
            meta,
            raw,
            ..
        } => rsx! {
            ToolUseBlock { name, input, result, meta, raw }
        },
        DisplayItem::TaskList { tasks, meta, raw } => {
            // TaskList rendering is broken — display as a simple pre-formatted list for now
            let text = tasks
                .iter()
                .map(|t| {
                    let check = match t.status {
                        ccmux_core::display::TaskStatus::Completed => "[x]",
                        ccmux_core::display::TaskStatus::InProgress => "[~]",
                        ccmux_core::display::TaskStatus::Cancelled => "[-]",
                        ccmux_core::display::TaskStatus::Pending => "[ ]",
                    };
                    format!("{check} {}", t.subject)
                })
                .collect::<Vec<_>>()
                .join("\n");
            rsx! {
                MessageBlock {
                    label: "Tasks",
                    border_class: "border-tool",
                    meta,
                    raw,

                    default_open: true,
                    pre { class: "task-list-text", "{text}" }
                }
            }
        }
        DisplayItem::TurnDuration { duration_ms, .. } => {
            let secs = duration_ms as f64 / 1000.0;
            rsx! {
                div { class: "turn-duration", "{secs:.1}s" }
            }
        }
        DisplayItem::Compaction { content, meta, raw } => rsx! {
            MessageBlock {
                label: "Compaction",
                border_class: "border-compaction",
                meta,
                raw,

                default_open: true,
                Prose { content }
            }
        },
        DisplayItem::Group { items, meta, .. } => rsx! {
            GroupBlock { items, meta }
        },
        DisplayItem::Other { .. } => rsx! {
            div { class: "other-block", "(other event)" }
        },
    }
}
