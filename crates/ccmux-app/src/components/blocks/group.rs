use std::collections::{HashMap, HashSet};

use dioxus::prelude::*;
use serde_json::Value;

use ccmux_core::display::{DisplayItem, ItemMeta};

use super::message::MessageBlock;
use super::thinking::ThinkingBlock;
use super::tool_use::ToolUseBlock;

/// Extract a contextual extra label string from a tool's input data.
pub fn tool_extra_label(name: &str, input: &Value) -> Option<String> {
    let obj = input.as_object()?;
    match name {
        "Read" | "Write" | "Edit" => obj
            .get("file_path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        "Bash" => obj
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        "Grep" | "Glob" => obj
            .get("pattern")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        "Agent" => {
            let subagent = obj.get("subagent_type").and_then(|v| v.as_str());
            let description = obj.get("description").and_then(|v| v.as_str());
            match (subagent, description) {
                (Some(s), Some(d)) => Some(format!("{s} · {d}")),
                (Some(s), None) => Some(s.to_string()),
                (None, Some(d)) => Some(d.to_string()),
                (None, None) => None,
            }
        }
        "WebSearch" | "ToolSearch" => obj
            .get("query")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        _ => None,
    }
}

/// Returns the kind label for a DisplayItem shown in the summary (e.g. "Thinking", "Read", "Glob").
fn item_kind_name(item: &DisplayItem) -> Option<String> {
    match item {
        DisplayItem::Thinking { .. } => Some("Thinking".to_string()),
        DisplayItem::ToolUse { name, .. } => Some(name.clone()),
        _ => None,
    }
}

#[derive(Clone)]
struct SummaryEntry {
    label: String,
    count: usize,
    extra: Option<String>,
}

fn build_summary(items: &[DisplayItem]) -> Vec<SummaryEntry> {
    // Track first-seen order using a Vec, deduplicate with HashSet
    let mut order: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut counts: HashMap<String, usize> = HashMap::new();
    // For extra label, store the last seen extra per key
    let mut extras: HashMap<String, Option<String>> = HashMap::new();

    for item in items {
        let Some(key) = item_kind_name(item) else {
            continue;
        };
        // Insert into ordered list if first occurrence
        if seen.insert(key.clone()) {
            order.push(key.clone());
        }
        // Count occurrences
        *counts.entry(key.clone()).or_insert(0) += 1;
        // Track extra label from last occurrence
        let extra = match item {
            DisplayItem::ToolUse { name, input, .. } => tool_extra_label(name, input),
            _ => None,
        };
        extras.insert(key, extra);
    }

    order
        .into_iter()
        .map(|key| SummaryEntry {
            label: key.clone(),
            count: counts.get(&key).copied().unwrap_or(1),
            extra: extras.get(&key).cloned().flatten(),
        })
        .collect()
}

#[component]
pub fn GroupBlock(items: Vec<DisplayItem>, meta: ItemMeta) -> Element {
    let mut open = use_signal(|| false);

    let summary = build_summary(&items);

    rsx! {
        div { class: "group-block",
            // Summary / toggle row
            div {
                class: "group-summary",
                onclick: move |_| open.toggle(),
                span { class: "group-caret",
                    if open() { "\u{25BE}" } else { "\u{25B8}" }
                }
                // Summary items with · separators
                for (i, entry) in summary.iter().enumerate() {
                    if i > 0 {
                        span { class: "step-dot", "\u{00B7}" }
                    }
                    span { class: "group-summary-item",
                        "{entry.label}"
                        if entry.count > 1 {
                            span { class: "group-count", "\u{00D7}{entry.count}" }
                        }
                        if let Some(extra) = &entry.extra {
                            " {extra}"
                        }
                    }
                }
                // Aggregate meta on right
                if let Some(tokens) = meta.tokens {
                    span { class: "group-meta-tokens", "{tokens} tok" }
                }
            }
            // Expanded children
            if open() {
                div { class: "group-expanded",
                    for (i, item) in items.into_iter().enumerate() {
                        GroupChild { key: "{i}", item }
                    }
                }
            }
        }
    }
}

/// Renders a single child item in minimal mode within a group.
#[component]
fn GroupChild(item: DisplayItem) -> Element {
    match item {
        DisplayItem::Thinking { text, .. } => rsx! {
            MessageBlock {
                label: "Thinking",
                border_class: "border-thinking",
                minimal: true,
                ThinkingBlock { text }
            }
        },
        DisplayItem::ToolUse {
            name,
            input,
            result,
            ..
        } => {
            let extra = tool_extra_label(&name, &input);
            rsx! {
                MessageBlock {
                    label: "{name}",
                    border_class: "border-tool",
                    extra_label: extra,
                    minimal: true,
                    ToolUseBlock { name, input, result }
                }
            }
        }
        // Fallback: nothing to render
        _ => rsx! {},
    }
}
