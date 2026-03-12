use ccmux_core::display::{ToolResultData, format::parse_grep_output};
use dioxus::prelude::*;
use serde_json::Value;

#[component]
pub fn GrepView(input: Value, result: Option<ToolResultData>) -> Element {
    let output_mode = input
        .get("output_mode")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let is_plain_mode = output_mode == "files_with_matches" || output_mode == "count";

    rsx! {
        div { class: "tool-details",
            if let Some(res) = result {
                GrepResult { result: res, is_plain_mode }
            }
        }
    }
}

#[component]
fn GrepResult(result: ToolResultData, is_plain_mode: bool) -> Element {
    let is_error = result.error.is_some();

    let text = if let Some(err) = &result.error {
        err.clone()
    } else if let Some(out) = &result.output {
        out.clone()
    } else {
        String::new()
    };

    if is_error || is_plain_mode {
        return rsx! {
            div { class: "tool-section",
                pre {
                    class: if is_error { "tool-result-error" } else { "" },
                    "{text}"
                }
            }
        };
    }

    let groups = parse_grep_output(&text);

    rsx! {
        div { class: "grep-results",
            for group in groups {
                div { class: "grep-group",
                    if !group.file.is_empty() {
                        div { class: "grep-file-header", "{group.file}" }
                    }
                    div { class: "grep-code-block",
                        for line in &group.lines {
                            div {
                                class: if line.is_match {
                                    "grep-line grep-match"
                                } else {
                                    "grep-line"
                                },
                                if let Some(num) = line.line_num {
                                    span { class: "grep-linenum", "{num}" }
                                }
                                span { class: "grep-content", "{line.content}" }
                            }
                        }
                    }
                }
            }
        }
    }
}
