use dioxus::prelude::*;

use ccmux_core::display::ToolResultData;
use serde_json::Value;

#[component]
pub fn ToolUseBlock(name: String, input: Value, result: Option<ToolResultData>) -> Element {
    let mut open = use_signal(|| false);

    let input_str = serde_json::to_string_pretty(&input).unwrap_or_default();
    let has_result = result.is_some();

    rsx! {
        div { class: "tool-use-block",
            div {
                class: "tool-use-header",
                onclick: move |_| open.toggle(),
                span { class: "tool-use-name", "{name}" }
                if has_result {
                    span { class: "tool-use-badge", "result" }
                }
                span { class: "tool-use-toggle",
                    if open() { "^" } else { "v" }
                }
            }
            if open() {
                div { class: "tool-use-body",
                    div { class: "tool-use-input",
                        h4 { "Input" }
                        pre { code { "{input_str}" } }
                    }
                    if let Some(res) = &result {
                        div { class: "tool-use-result",
                            h4 { "Result" }
                            if let Some(err) = &res.error {
                                pre { class: "tool-result-error", "{err}" }
                            }
                            if let Some(output) = &res.output {
                                pre { class: "tool-result-output", "{output}" }
                            }
                        }
                    }
                }
            }
        }
    }
}
