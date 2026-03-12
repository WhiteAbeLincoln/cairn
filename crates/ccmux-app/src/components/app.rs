use dioxus::prelude::*;

use crate::routes::Route;

#[component]
pub fn AppLayout() -> Element {
    rsx! {
        nav { class: "app-nav",
            Link { to: Route::SessionList {}, class: "nav-home", "ccmux" }
        }
        main { class: "app-main",
            Outlet::<Route> {}
        }
    }
}
