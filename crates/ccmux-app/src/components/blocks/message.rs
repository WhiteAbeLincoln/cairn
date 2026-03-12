use dioxus::prelude::*;
use serde_json::Value;

use ccmux_core::display::ItemMeta;

use super::json_tree::JsonTree;
use crate::components::session_view::RawModeContext;

#[component]
pub fn MessageBlock(
    label: String,
    border_class: String,
    #[props(default)] extra_label: Option<String>,
    #[props(default)] meta: Option<ItemMeta>,
    #[props(default)] raw: Option<Value>,
    /// For tool calls: the raw tool_result event, shown alongside the tool_use raw event.
    #[props(default)]
    result_raw: Option<Value>,
    #[props(default = true)] collapsible: bool,
    #[props(default = true)] default_open: bool,
    #[props(default = false)] minimal: bool,
    children: Element,
) -> Element {
    let mut open = use_signal(|| default_open);
    let mut raw_open = use_signal(|| false);
    let mut kebab_open = use_signal(|| false);

    let global_raw: Option<Signal<bool>> =
        try_use_context::<RawModeContext>().map(|ctx| ctx.global_raw);

    let show_raw = raw_open() || global_raw.map(|s| s()).unwrap_or(false);
    let has_raw = raw.is_some();

    // Build outer class
    let block_class = if minimal {
        if open() {
            format!("message-block {border_class} message-block-minimal message-block-expanded")
        } else {
            format!("message-block {border_class} message-block-minimal")
        }
    } else {
        format!("message-block {border_class}")
    };

    rsx! {
        div { class: "{block_class}",
            // Header — same structure for both full and minimal, styling differs via CSS
            div {
                class: "message-header",
                onclick: move |_| {
                    if minimal || collapsible {
                        open.toggle();
                    }
                },
                // Fixed start: caret (minimal) + label
                if minimal {
                    span { class: "message-caret",
                        if open() { "\u{25BE}" } else { "\u{25B8}" }
                    }
                }
                span { class: "message-label", "{label}" }

                // Scrollable middle: extra label + spacer + metadata
                span { class: "header-middle",
                    if let Some(extra) = &extra_label {
                        span { class: "message-extra-label", "{extra}" }
                    }
                    span { class: "header-spacer" }
                    if let Some(ref m) = meta {
                        MetaFields { meta: m.clone() }
                    }
                }

                // Fixed end: action buttons
                div { class: "message-actions",
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
                    if !minimal && collapsible {
                        button {
                            class: "message-collapse-toggle",
                            title: if open() { "Collapse" } else { "Expand" },
                            onclick: move |e| {
                                e.stop_propagation();
                                open.toggle();
                            },
                            if open() { "\u{25B4}" } else { "\u{25BE}" }
                        }
                    }

                    // Kebab menu (mobile, <=768px)
                    if !minimal && (has_raw || collapsible) {
                        div { class: "kebab-menu",
                            button {
                                class: "kebab-trigger",
                                title: "More options",
                                onclick: move |e| {
                                    e.stop_propagation();
                                    kebab_open.toggle();
                                },
                                "\u{22EE}"
                            }
                            if kebab_open() {
                                div {
                                    class: "kebab-backdrop",
                                    onclick: move |e| {
                                        e.stop_propagation();
                                        kebab_open.set(false);
                                    },
                                }
                                div { class: "kebab-dropdown",
                                    if has_raw {
                                        button {
                                            class: if show_raw { "kebab-item kebab-item-active" } else { "kebab-item" },
                                            onclick: move |e| {
                                                e.stop_propagation();
                                                raw_open.toggle();
                                                kebab_open.set(false);
                                            },
                                            "{{}}"
                                        }
                                    }
                                    if collapsible {
                                        button {
                                            class: "kebab-item",
                                            onclick: move |e| {
                                                e.stop_propagation();
                                                open.toggle();
                                                kebab_open.set(false);
                                            },
                                            if open() { "\u{25B4}" } else { "\u{25BE}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
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
