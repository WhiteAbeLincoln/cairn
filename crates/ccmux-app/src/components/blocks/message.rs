use dioxus::prelude::*;
use serde_json::Value;

use ccmux_core::display::ItemMeta;

use super::json_tree::JsonTree;
use crate::components::session_view::RawModeContext;

#[component]
pub fn MessageBlock(
    label: Element,
    role: String,
    #[props(default)] extra_label: Option<Element>,
    #[props(default)] meta: Option<ItemMeta>,
    #[props(default)] raw: Option<Value>,
    /// For tool calls: the raw tool_result event, shown alongside the tool_use raw event.
    #[props(default)]
    result_raw: Option<Value>,
    #[props(default = false)] minimal: bool,
    children: Element,
) -> Element {
    let mut open = use_signal(|| !minimal);
    let mut raw_open = use_signal(|| false);

    let global_raw: Option<Signal<bool>> =
        try_use_context::<RawModeContext>().map(|ctx| ctx.global_raw);

    let show_raw = raw_open() || global_raw.map(|s| s()).unwrap_or(false);
    let has_raw = raw.is_some();

    // Build outer class
    let block_class = if minimal {
        if open() {
            "message-block message-block-minimal message-block-expanded".to_string()
        } else {
            "message-block message-block-minimal".to_string()
        }
    } else {
        "message-block".to_string()
    };

    let header_middle = rsx! {
        span {
            class: "header-middle",
            if let Some(extra) = &extra_label {
                span { class: "message-extra-label", {extra} }
            }
            span { class: "header-spacer" }
            if let Some(ref m) = meta {
                MetaFields { meta: m.clone() }
            }
        }
    };

    let fixed_end = rsx! {
        if has_raw {
            button {
                class: if show_raw { "message-raw-toggle message-raw-toggle-active" } else { "message-raw-toggle" },
                title: "Toggle raw JSON",
                onclick: move |e| {
                    e.stop_propagation();
                    raw_open.toggle();
                },
                "{{}}"
            }
        }
    };

    rsx! {
        div {
            class: "{block_class}",
            "data-role": "{role}",
            // Header — same structure for both full and minimal, styling differs via CSS
            div {
                class: "message-header message-header-clickable",
                onclick: move |_| open.toggle(),
                span { class: "message-label", {label} }
                {header_middle}
                {fixed_end}
            }

            // Body
            if open() {
                div { class: "message-body",
                    {children}
                    if show_raw {
                        div { class: "raw-inline",
                            if let Some(ref v) = raw {
                                if result_raw.is_some() {
                                    div { class: "raw-inline-label", "tool_use" }
                                }
                                JsonTree { value: v.clone(), default_expand_depth: 1 }
                            }
                            if let Some(ref rv) = result_raw {
                                div { class: "raw-inline-label", "tool_result" }
                                JsonTree { value: rv.clone(), default_expand_depth: 1 }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Renders metadata fields (model, tokens, uuid) inline.
#[component]
fn MetaFields(meta: ItemMeta) -> Element {
    rsx! {
        if let Some(ref model) = meta.model {
            span { class: "message-meta-item", "{model}" }
        }
        if meta.model.is_some() && meta.tokens.is_some() {
            span { class: "message-meta-dot", "\u{00B7}" }
        }
        if let Some(tokens) = meta.tokens {
            span { class: "message-meta-item", "{tokens} tok" }
        }
    }
}
