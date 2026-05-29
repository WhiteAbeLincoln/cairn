//! The cairn session-manager daemon: serves the `cairn:daemon@0.1.0` wRPC
//! surface over a Unix domain socket against an in-process session registry.

pub mod auth;
pub mod config;
pub mod daemon;
pub mod error;
pub mod handlers;
pub mod identity;
pub mod listen;
pub mod registry;
pub mod serve;
pub mod signal;
pub mod spawn;
pub mod tls;
