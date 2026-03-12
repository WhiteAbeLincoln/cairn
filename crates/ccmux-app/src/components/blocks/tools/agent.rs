use ccmux_core::display::ToolResultData;
use dioxus::prelude::*;
use serde_json::Value;

/// Extract text from agent tool result content blocks (array of {text: "..."} objects).
fn agent_output_text(result: &ToolResultData) -> String {
    // Try to parse structured content array from raw
    let content = result.raw.get("content");
    if let Some(Value::Array(arr)) = content {
        let texts: Vec<&str> = arr
            .iter()
            .filter_map(|item| item.get("text").and_then(|v| v.as_str()))
            .collect();
        if !texts.is_empty() {
            return texts.join("\n");
        }
    }
    // Fallback to output/error strings
    result
        .output
        .as_deref()
        .or(result.error.as_deref())
        .unwrap_or("")
        .to_string()
}

#[component]
pub fn AgentView(input: Value, result: Option<ToolResultData>) -> Element {
    let subagent_type = input
        .get("subagent_type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let description = input
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Agent ID from tool_use_result.agentId.
    // NOTE: agentId is not currently populated through the pipeline, so this link
    // will not render in practice. AgentView still displays subagent_type, description,
    // and output text correctly. This is a known limitation.
    let agent_id = result.as_ref().and_then(|r| {
        r.tool_use_result
            .as_ref()
            .and_then(|v| v.get("agentId"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    });

    let output_text = result.as_ref().map(agent_output_text);

    let output_open = use_signal(|| false);

    rsx! {
        div { class: "agent-expanded",
            if !subagent_type.is_empty() {
                div { class: "agent-subtype", "{subagent_type}" }
            }
            if !description.is_empty() {
                div { class: "agent-description", "{description}" }
            }
            if let Some(aid) = agent_id {
                a {
                    class: "agent-link",
                    href: "/session/{aid}",
                    "View subagent session \u{2192}"
                }
            }
            if let Some(text) = output_text {
                div { class: "agent-output-section",
                    button {
                        class: "agent-output-toggle",
                        onclick: move |_| {
                            let open = *output_open.read();
                            output_open.clone().set(!open);
                        },
                        if *output_open.read() { "\u{25BE} Output" } else { "\u{25B8} Output" }
                    }
                    if *output_open.read() {
                        div { class: "agent-output", "{text}" }
                    }
                }
            }
        }
    }
}
