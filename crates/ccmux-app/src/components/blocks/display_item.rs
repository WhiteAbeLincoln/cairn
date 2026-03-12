use dioxus::prelude::*;

use ccmux_core::display::DisplayItem;

use super::message::MessageBlock;
use super::prose::Prose;
use super::task_list::TaskListBlock;
use super::thinking::ThinkingBlock;
use super::tool_use::ToolUseBlock;

#[component]
pub fn DisplayItemView(item: DisplayItem) -> Element {
    match item {
        DisplayItem::UserMessage { content, .. } => rsx! {
            MessageBlock { label: "User", border_class: "border-user",
                Prose { content }
            }
        },
        DisplayItem::AssistantMessage { text, .. } => rsx! {
            MessageBlock { label: "Assistant", border_class: "border-assistant",
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
            ..
        } => rsx! {
            ToolUseBlock { name, input, result }
        },
        DisplayItem::TaskList { tasks, .. } => rsx! {
            TaskListBlock { tasks }
        },
        DisplayItem::TurnDuration { duration_ms, .. } => {
            let secs = duration_ms as f64 / 1000.0;
            rsx! {
                div { class: "turn-duration", "{secs:.1}s" }
            }
        }
        DisplayItem::Compaction { content, .. } => rsx! {
            MessageBlock { label: "Compaction", border_class: "border-compaction",
                Prose { content }
            }
        },
        DisplayItem::Group { items } => rsx! {
            div { class: "group-block",
                for (i, sub_item) in items.into_iter().enumerate() {
                    DisplayItemView { key: "{i}", item: sub_item }
                }
            }
        },
        DisplayItem::Other { .. } => rsx! {
            div { class: "other-block", "(other event)" }
        },
    }
}
