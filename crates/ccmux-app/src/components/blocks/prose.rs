use dioxus::prelude::*;

/// Render markdown content as HTML using pulldown-cmark.
#[component]
pub fn Prose(content: String) -> Element {
    let html = markdown_to_html(&content);

    rsx! {
        div {
            class: "prose",
            dangerous_inner_html: "{html}",
        }
    }
}

fn markdown_to_html(markdown: &str) -> String {
    use pulldown_cmark::{Options, Parser, html};

    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);

    let parser = Parser::new_ext(markdown, options);
    let mut html_output = String::new();
    html::push_html(&mut html_output, parser);
    html_output
}
