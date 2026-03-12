use std::collections::{HashMap, HashSet};

use dioxus::prelude::*;
use serde_json::Value;

use ccmux_core::display::DisplayItem;

use crate::components::blocks::{display_item::DisplayItemView, message::MessageBlock};

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

#[derive(Clone, PartialEq, Eq)]
struct SummaryEntry {
    label: String,
    count: usize,
}

fn build_summary(items: &[DisplayItem]) -> Vec<SummaryEntry> {
    let mut order: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut counts: HashMap<String, usize> = HashMap::new();

    for item in items {
        let Some(key) = item_kind_name(item) else {
            continue;
        };
        if seen.insert(key.clone()) {
            order.push(key.clone());
        }
        *counts.entry(key.clone()).or_insert(0) += 1;
    }

    order
        .into_iter()
        .map(|key| SummaryEntry {
            label: key.clone(),
            count: counts.get(&key).copied().unwrap_or(1),
        })
        .collect()
}

#[component]
fn GroupSummary(summary: Vec<SummaryEntry>) -> Element {
    rsx! {
        div {
            class: "group-summary",
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
                }
            }
        }
    }
}

#[component]
pub fn GroupBlock(items: Vec<DisplayItem>) -> Element {
    let summary = build_summary(&items);

    rsx! {
        MessageBlock {
            label: rsx!{ GroupSummary { summary } },
            role: "group",
            minimal: true,
            div {
                class: "group-expanded",
                for (i, item) in items.into_iter().enumerate() {
                    DisplayItemView {  key: "{i}", item, minimal: true }
                }
            }
        }
    }
}
