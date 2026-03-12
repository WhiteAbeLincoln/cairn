use dioxus::prelude::*;

use ccmux_core::display::{DisplayItem, DisplayItemWithMode};

use crate::components::blocks::json_tree::JsonTree;

use super::group::GroupBlock;
use super::message::MessageBlock;
use super::prose::Prose;
use super::tool_use::ToolUseBlock;

#[component]
pub fn DisplayItemModedView(item: DisplayItemWithMode) -> Element {
    match item {
        DisplayItemWithMode::Full(item) => rsx! {
            DisplayItemView { item, minimal: false }
        },
        DisplayItemWithMode::Collapsed(item) => rsx! {
            DisplayItemView { item, minimal: true }
        },
        DisplayItemWithMode::Grouped(items) => match items.len() {
            // would like to match on vec as a slice here, but then we have to clone
            // the value since we get a reference back
            0 => rsx! {},
            // If there's only one item in the group, render it directly in minimal mode (no summary)
            1 => {
                let single_item = items.into_iter().next().unwrap();
                rsx! { DisplayItemView { item: single_item, minimal: true } }
            }
            _ => rsx! {
                GroupBlock { items }
            },
        },
        DisplayItemWithMode::Hidden(_) => rsx! {}, // Don't render hidden items at
    }
}

#[component]
pub fn DisplayItemView(item: DisplayItem, minimal: bool) -> Element {
    match item {
        DisplayItem::TurnDuration { duration_ms, .. } => {
            let secs = duration_ms as f64 / 1000.0;
            rsx! {
                div { class: "turn-duration", "data-role": "system", "Turn completed in {secs:.1}s" }
            }
        }
        DisplayItem::UserMessage { content, meta, raw } => rsx! {
            MessageBlock {
                label: rsx!{"User"},
                role: "user",
                meta,
                raw,
                minimal,

                Prose { content }
            }
        },
        DisplayItem::AssistantMessage { text, meta, raw } => rsx! {
            MessageBlock {
                label: rsx!{"Assistant"},
                role: "assistant",
                meta,
                raw,
                minimal,

                Prose { content: text }
            }
        },
        DisplayItem::Thinking { text, meta, raw } => rsx! {
            MessageBlock {
                label: rsx!{"Thinking"},
                role: "thinking",
                meta,
                raw,
                minimal,

                Prose { content: text }
            }
        },
        DisplayItem::ToolUse {
            name,
            input,
            result,
            meta,
            raw,
            ..
        } => rsx! {
            ToolUseBlock { name, input, result, meta, raw, minimal }
        },
        // DisplayItem::TaskList { tasks, meta, raw } => {
        //     // TaskList rendering is broken — display as a simple pre-formatted list for now
        //     let text = tasks
        //         .iter()
        //         .map(|t| {
        //             let check = match t.status {
        //                 ccmux_core::display::TaskStatus::Completed => "[x]",
        //                 ccmux_core::display::TaskStatus::InProgress => "[~]",
        //                 ccmux_core::display::TaskStatus::Cancelled => "[-]",
        //                 ccmux_core::display::TaskStatus::Pending => "[ ]",
        //             };
        //             format!("{check} {}", t.subject)
        //         })
        //         .collect::<Vec<_>>()
        //         .join("\n");
        //     rsx! {
        //         MessageBlock {
        //             label: "Tasks",
        //             border_class: "border-tool",
        //             meta,
        //             raw,

        //             default_open: true,
        //             pre { class: "task-list-text", "{text}" }
        //         }
        //     }
        // }
        DisplayItem::Compaction { content, meta, raw } => rsx! {
            MessageBlock {
                label: rsx!{"Compaction"},
                role: "compaction",
                meta,
                raw,
                minimal,

                Prose { content }
            }
        },
        DisplayItem::Other { raw } => rsx! {
            MessageBlock {
                label: rsx!{"Other"},
                role: "other",
                minimal,

                JsonTree { value: raw }
            }
        },
    }
}
