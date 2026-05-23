//! Opaque client identity used to track per-attached-client state in
//! `PtySession` (leader election, detach notifications). Caller-supplied
//! and transport-agnostic — the library does only equality comparisons.
//!
//! See `docs/superpowers/specs/2026-05-22-pty-multi-client-semantics-design.md`.

use std::fmt;
use std::num::NonZeroU64;

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct ClientId(NonZeroU64);

impl ClientId {
    /// Construct a `ClientId` from a daemon counter value.
    ///
    /// The library adds 1 internally so the underlying `NonZeroU64` is
    /// never zero. The daemon may start its counter at 0; the returned
    /// id is opaque.
    ///
    /// # Panics
    ///
    /// Panics if `value == u64::MAX`. At 1M attaches per second this
    /// would take ~584,500 years; in debug builds Rust's overflow check
    /// fires at the `+ 1`, in release builds the `NonZeroU64` invariant
    /// fires on the wrapped result. Both are the desired behavior —
    /// reaching this case means something has gone catastrophically
    /// wrong upstream.
    pub fn from_u64(value: u64) -> Self {
        ClientId(NonZeroU64::new(value + 1).expect("ClientId from u64::MAX is unsupported"))
    }
}

impl fmt::Display for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_u64_zero_maps_to_one() {
        let id = ClientId::from_u64(0);
        assert_eq!(format!("{id}"), "1");
    }

    #[test]
    fn from_u64_preserves_uniqueness() {
        let a = ClientId::from_u64(0);
        let b = ClientId::from_u64(1);
        let c = ClientId::from_u64(0);
        assert_ne!(a, b);
        assert_eq!(a, c);
    }

    #[test]
    fn display_renders_underlying_value() {
        let id = ClientId::from_u64(41);
        assert_eq!(format!("{id}"), "42");
    }

    #[test]
    fn is_copy_and_hashable() {
        use std::collections::HashSet;
        let a = ClientId::from_u64(0);
        let b = a; // Copy
        let mut set = HashSet::new();
        set.insert(a);
        assert!(set.contains(&b));
    }
}
