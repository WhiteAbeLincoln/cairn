use ccmux_core::display::ToolResultData;
use dioxus::prelude::*;
use serde_json::Value;

#[component]
pub fn WriteView(input: Value, result: Option<ToolResultData>) -> Element {
    let file_path = input
        .get("file_path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let content = input
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let is_error = result.as_ref().map(|r| r.error.is_some()).unwrap_or(false);

    let error_text = result
        .as_ref()
        .and_then(|r| r.error.clone())
        .unwrap_or_default();

    rsx! {
        div { class: "tool-details",
            if !file_path.is_empty() {
                div { class: "write-file-header", "{file_path}" }
            }
            if !content.is_empty() {
                pre { class: "write-content",
                    code { "{content}" }
                }
            }
            if is_error && !error_text.is_empty() {
                div { class: "tool-section",
                    div { class: "tool-section-label", "Output" }
                    pre { class: "tool-result-error", "{error_text}" }
                }
            }
        }
    }
}
