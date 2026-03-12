use dioxus::prelude::*;

#[component]
pub fn ThinkingBlock(text: String) -> Element {
    let mut open = use_signal(|| false);

    let preview: String = text.chars().take(100).collect();
    let has_more = text.len() > 100;

    rsx! {
        div { class: "thinking-block",
            div {
                class: "thinking-header",
                onclick: move |_| open.toggle(),
                span { class: "thinking-label", "Thinking" }
                if !open() {
                    span { class: "thinking-preview",
                        "{preview}"
                        if has_more { "..." }
                    }
                }
                span { class: "thinking-toggle",
                    if open() { "^" } else { "v" }
                }
            }
            if open() {
                div { class: "thinking-body",
                    pre { class: "thinking-text", "{text}" }
                }
            }
        }
    }
}
