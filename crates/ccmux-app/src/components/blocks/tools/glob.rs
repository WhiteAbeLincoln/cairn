use ccmux_core::display::ToolResultData;
use dioxus::prelude::*;
use serde_json::Value;

#[component]
pub fn GlobView(input: Value, result: Option<ToolResultData>) -> Element {
    let pattern = input
        .get("pattern")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let path = input
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    rsx! {
        div { class: "tool-details",
            if !pattern.is_empty() {
                div { class: "glob-pattern",
                    span { class: "glob-pattern-label", "pattern " }
                    span { class: "tool-filepath", "{pattern}" }
                }
            }
            if !path.is_empty() {
                div { class: "glob-search-path",
                    span { class: "glob-search-path-label", "in " }
                    span { "{path}" }
                }
            }
            if let Some(res) = result {
                GlobResult { result: res }
            }
        }
    }
}

#[component]
fn GlobResult(result: ToolResultData) -> Element {
    let is_error = result.error.is_some();

    let text = if let Some(err) = &result.error {
        err.clone()
    } else if let Some(out) = &result.output {
        out.clone()
    } else {
        String::new()
    };

    if text.is_empty() {
        return rsx! {};
    }

    rsx! {
        div { class: "tool-section",
            pre {
                class: if is_error { "tool-result-error" } else { "glob-results" },
                "{text}"
            }
        }
    }
}
