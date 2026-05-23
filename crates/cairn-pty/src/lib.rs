//! Pseudo-terminal session abstraction.
//!
//! See `docs/superpowers/specs/2026-05-22-pty-session-trait-design.md`.

mod client_id;
mod error;
mod ghostty;
mod session;
mod subscription;
mod types;

pub use client_id::ClientId;
pub use error::PtyError;
pub use ghostty::ExitStatus;
pub use ghostty::GhosttyPty;
pub use session::PtySession;
pub use subscription::Subscription;
pub use types::{SpawnOptions, TermSize};
