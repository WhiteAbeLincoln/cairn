use dioxus::prelude::*;

use crate::components::app::AppLayout;
use crate::components::session_list::SessionList;
use crate::components::session_view::SessionView;

#[derive(Clone, Debug, PartialEq, Routable)]
pub enum Route {
    #[layout(AppLayout)]
    #[route("/")]
    SessionList {},
    #[route("/session/:id")]
    SessionView { id: String },
}
