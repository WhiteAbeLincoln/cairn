use dioxus::prelude::*;
use serde_json::Value;

use ccmux_core::display::{ItemMeta, ToolResultData};

use super::group::tool_extra_label;
use super::message::MessageBlock;
use super::tools;

#[component]
pub fn ToolUseBlock(
    name: String,
    input: Value,
    result: Option<ToolResultData>,
    #[props(default)] meta: Option<ItemMeta>,
    #[props(default)] raw: Option<Value>,
    #[props(default = false)] minimal: bool,
) -> Element {
    let extra = tool_extra_label(&name, &input);

    // Bash and AskUserQuestion: default open, collapsible
    // All other tools: default closed, collapsible
    let default_open = matches!(name.as_str(), "Bash" | "AskUserQuestion");

    rsx! {
        MessageBlock {
            label: name.clone(),
            border_class: "border-tool",
            extra_label: extra,
            meta,
            raw,
            collapsible: true,
            default_open,
            minimal,
            match name.as_str() {
                "Bash" => rsx! {
                    tools::bash::BashView { input, result }
                },
                "Edit" => rsx! {
                    tools::edit::EditView { input, result }
                },
                "Read" => rsx! {
                    tools::read::ReadView { input, result }
                },
                _ => rsx! {
                    GenericToolView { input, result }
                },
            }
        }
    }
}

#[component]
fn GenericToolView(input: Value, result: Option<ToolResultData>) -> Element {
    let input_str = serde_json::to_string_pretty(&input).unwrap_or_default();

    rsx! {
        div { class: "tool-details",
            div { class: "tool-section",
                div { class: "tool-section-label", "Input" }
                pre { code { "{input_str}" } }
            }
            if let Some(res) = result {
                div { class: "tool-section",
                    div { class: "tool-section-label", "Result" }
                    if let Some(err) = &res.error {
                        pre { class: "tool-result-error", "{err}" }
                    }
                    if let Some(out) = &res.output {
                        pre { class: "tool-result-output", "{out}" }
                    }
                }
            }
        }
    }
}
