use dioxus::prelude::*;
use serde_json::Value;

/// A collapsible JSON tree viewer.
///
/// Expands objects/arrays up to `default_expand_depth` levels deep.
/// Deeper levels start collapsed and can be toggled open.
#[component]
pub fn JsonTree(value: Value, #[props(default = 1)] default_expand_depth: usize) -> Element {
    rsx! {
        div { class: "json-tree",
            JsonNode { value, depth: 0, default_expand_depth }
        }
    }
}

#[component]
fn JsonNode(value: Value, depth: usize, default_expand_depth: usize) -> Element {
    match &value {
        Value::Null => rsx! { span { class: "jt-null", "null" } },
        Value::Bool(b) => rsx! { span { class: "jt-bool", "{b}" } },
        Value::Number(n) => rsx! { span { class: "jt-number", "{n}" } },
        Value::String(s) => rsx! { JsonString { value: s.clone() } },
        Value::Array(arr) => {
            if arr.is_empty() {
                return rsx! { span { class: "jt-bracket", "[]" } };
            }
            rsx! {
                JsonCollection {
                    entries: arr.iter().enumerate().map(|(i, v)| CollectionEntry {
                        key: CollectionKey::Index(i),
                        value: v.clone(),
                    }).collect::<Vec<_>>(),
                    open_char: "[",
                    close_char: "]",
                    count_label: format!("{} items", arr.len()),
                    depth,
                    default_expand_depth,
                }
            }
        }
        Value::Object(obj) => {
            if obj.is_empty() {
                return rsx! { span { class: "jt-bracket", "{{}}" } };
            }
            let open = "{".to_string();
            let close = "}".to_string();
            rsx! {
                JsonCollection {
                    entries: obj.iter().map(|(k, v)| CollectionEntry {
                        key: CollectionKey::Key(k.clone()),
                        value: v.clone(),
                    }).collect::<Vec<_>>(),
                    open_char: open,
                    close_char: close,
                    count_label: format!("{} keys", obj.len()),
                    depth,
                    default_expand_depth,
                }
            }
        }
    }
}

/// Copy button that copies the JSON value to clipboard.
/// Shows ⧉ by default, briefly shows ✓ after copying.
#[component]
fn CopyButton(value: Value) -> Element {
    let copied = use_signal(|| false);

    let copy_text = match &value {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    };

    rsx! {
        button {
            class: if copied() { "jt-copy jt-copy-ok" } else { "jt-copy" },
            title: "Copy value",
            onclick: move |e| {
                e.stop_propagation();
                let text = copy_text.clone();
                let mut copied = copied;
                spawn(async move {
                    let js = format!(
                        "await navigator.clipboard.writeText({}); await new Promise(r => setTimeout(r, 1500))",
                        serde_json::to_string(&text).unwrap_or_default()
                    );
                    copied.set(true);
                    let _ = document::eval(&js).await;
                    copied.set(false);
                });
            },
            if copied() { "\u{2713}" } else { "\u{29C9}" }
        }
    }
}

#[derive(Clone, PartialEq)]
enum CollectionKey {
    Key(String),
    Index(usize),
}

#[derive(Clone, PartialEq)]
struct CollectionEntry {
    key: CollectionKey,
    value: Value,
}

#[component]
fn JsonCollection(
    entries: Vec<CollectionEntry>,
    open_char: String,
    close_char: String,
    count_label: String,
    depth: usize,
    default_expand_depth: usize,
) -> Element {
    let mut expanded = use_signal(|| depth < default_expand_depth);

    if expanded() {
        rsx! {
            span { class: "jt-bracket",
                button {
                    class: "jt-toggle",
                    onclick: move |_| expanded.set(false),
                    "\u{25BE}"
                }
                "{open_char}"
            }
            div { class: "jt-children",
                for (i, entry) in entries.iter().enumerate() {
                    div { class: "jt-entry",
                        key: "{i}",
                        match &entry.key {
                            CollectionKey::Key(k) => rsx! {
                                span { class: "jt-key", "\"{k}\"" }
                                span { class: "jt-colon", ": " }
                            },
                            CollectionKey::Index(_) => rsx! {},
                        }
                        JsonNode {
                            value: entry.value.clone(),
                            depth: depth + 1,
                            default_expand_depth,
                        }
                        if i < entries.len() - 1 {
                            span { class: "jt-comma", "," }
                        }
                        CopyButton { value: entry.value.clone() }
                    }
                }
            }
            span { class: "jt-bracket", "{close_char}" }
        }
    } else {
        rsx! {
            span { class: "jt-bracket",
                button {
                    class: "jt-toggle",
                    onclick: move |_| expanded.set(true),
                    "\u{25B8}"
                }
                "{open_char}"
            }
            span {
                class: "jt-collapsed-badge",
                onclick: move |_| expanded.set(true),
                "{count_label}"
            }
            span { class: "jt-bracket", "{close_char}" }
        }
    }
}

#[component]
fn JsonString(value: String) -> Element {
    const TRUNCATE_LEN: usize = 200;

    let mut show_full = use_signal(|| false);

    if value.len() <= TRUNCATE_LEN || show_full() {
        // Escape for display
        let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
        rsx! {
            span { class: "jt-string", "\"{escaped}\"" }
        }
    } else {
        let truncated = &value[..TRUNCATE_LEN];
        let escaped = truncated.replace('\\', "\\\\").replace('"', "\\\"");
        let remaining = value.len() - TRUNCATE_LEN;
        rsx! {
            span { class: "jt-string", "\"{escaped}" }
            button {
                class: "jt-more",
                onclick: move |_| show_full.set(true),
                "...{remaining} more"
            }
            span { class: "jt-string", "\"" }
        }
    }
}
