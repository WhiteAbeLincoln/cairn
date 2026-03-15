mod components;
mod routes;
mod server_fns;

#[cfg(not(target_arch = "wasm32"))]
mod api;

use dioxus::prelude::*;
use routes::Route;

fn main() {
    #[cfg(not(target_arch = "wasm32"))]
    {
        dioxus::server::serve(|| async {
            let api_routes = api::build_api_router();
            let dioxus_router = dioxus::server::router(App);
            Ok(api_routes.merge(dioxus_router))
        });
    }

    #[cfg(target_arch = "wasm32")]
    dioxus::launch(App);
}

static MAIN_CSS: Asset = asset!("/assets/style.scss");

#[component]
fn App() -> Element {
    rsx! {
        document::Stylesheet { href: MAIN_CSS }
        Router::<Route> {}
    }
}
