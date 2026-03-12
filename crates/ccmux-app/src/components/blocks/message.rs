use dioxus::prelude::*;

#[component]
pub fn MessageBlock(label: String, border_class: String, children: Element) -> Element {
    let mut open = use_signal(|| true);

    rsx! {
        div { class: "message-block {border_class}",
            div {
                class: "message-header",
                onclick: move |_| open.toggle(),
                span { class: "message-label", "{label}" }
                span { class: "message-toggle",
                    if open() { "^" } else { "v" }
                }
            }
            if open() {
                div { class: "message-body", {children} }
            }
        }
    }
}
