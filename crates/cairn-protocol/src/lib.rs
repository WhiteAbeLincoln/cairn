//! `cairn:daemon@0.1.0` wire protocol bindings.
//!
//! WIT schema lives at `wit/cairn.wit`. The `wit-bindgen-wrpc` macro
//! below produces Rust trait definitions for server-side `Handler`
//! impls plus free functions for client-side invocations. See
//! `docs/superpowers/specs/2026-05-26-daemon-protocol-design.md`
//! for the design rationale.

wit_bindgen_wrpc::generate!({
    world: "daemon",
    with: {
        "cairn:daemon/types@0.1.0": generate,
        "cairn:daemon/sessions@0.1.0": generate,
        "cairn:daemon/meta@0.1.0": generate,
    },
});

// Client-side invocation functions, generated from the `daemon-client` world.
// External callers reach them at `cairn_protocol::client::cairn::daemon::{sessions,meta}::*`
// — the `client::` segment scopes the second generate! invocation so it doesn't
// clash with the server-side bindings above.
pub mod client {
    wit_bindgen_wrpc::generate!({
        world: "daemon-client",
        with: {
            "cairn:daemon/types@0.1.0": crate::cairn::daemon::types,
            "cairn:daemon/sessions@0.1.0": generate,
            "cairn:daemon/meta@0.1.0": generate,
        },
    });
}
