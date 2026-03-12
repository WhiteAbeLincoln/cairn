use ccmux_core::display::ToolResultData;
use dioxus::prelude::*;
use serde_json::Value;

#[derive(Clone, PartialEq)]
enum DiffLineType {
    Remove,
    Add,
}

struct DiffLine {
    line_type: DiffLineType,
    text: String,
}

fn build_diff_lines(old_str: &str, new_str: &str) -> Vec<DiffLine> {
    let mut lines = Vec::new();
    for line in old_str.split('\n') {
        lines.push(DiffLine {
            line_type: DiffLineType::Remove,
            text: line.to_string(),
        });
    }
    for line in new_str.split('\n') {
        lines.push(DiffLine {
            line_type: DiffLineType::Add,
            text: line.to_string(),
        });
    }
    lines
}

#[component]
pub fn EditView(input: Value, result: Option<ToolResultData>) -> Element {
    let file_path = input
        .get("file_path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let old_string = input
        .get("old_string")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let new_string = input
        .get("new_string")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let replace_all = input
        .get("replace_all")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let has_strings = old_string.is_some() && new_string.is_some();

    rsx! {
        div { class: "tool-details",
            if !file_path.is_empty() || replace_all {
                div { class: "edit-header",
                    if !file_path.is_empty() {
                        span { class: "tool-filepath", "{file_path}" }
                    }
                    if replace_all {
                        span { class: "edit-badge", "all" }
                    }
                }
            }
            if has_strings {
                {
                    let old = old_string.unwrap();
                    let new = new_string.unwrap();
                    let diff_lines = build_diff_lines(&old, &new);
                    rsx! {
                        div { class: "edit-diff",
                            for line in diff_lines {
                                div {
                                    class: match line.line_type {
                                        DiffLineType::Add => "diff-line diff-add",
                                        DiffLineType::Remove => "diff-line diff-remove",
                                    },
                                    span { class: "diff-marker",
                                        match line.line_type {
                                            DiffLineType::Add => "+",
                                            DiffLineType::Remove => "-",
                                        }
                                    }
                                    span { "{line.text}" }
                                }
                            }
                        }
                    }
                }
            } else {
                {
                    let input_str = serde_json::to_string_pretty(&input).unwrap_or_default();
                    rsx! {
                        div { class: "tool-section",
                            div { class: "tool-section-label", "Input" }
                            pre { code { "{input_str}" } }
                        }
                    }
                }
            }
            if let Some(res) = result {
                if let Some(err) = &res.error {
                    div { class: "tool-section",
                        div { class: "tool-section-label", "Result" }
                        pre { class: "tool-result-error", "{err}" }
                    }
                }
            }
        }
    }
}
