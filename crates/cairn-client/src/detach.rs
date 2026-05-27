//! Detach-key sequence parsing and matching.
//!
//! `--detach-keys` is a comma-separated list of `ctrl-<char>` or single-char
//! tokens (docker-style), e.g. `ctrl-q,ctrl-q` or `ctrl-a,d`. Each key is
//! recognized in TWO encodings: the raw control byte, and the Kitty CSI-u
//! `\x1b[<code>;<mods>u` form — because an inferior program inside the session
//! can flip the outer terminal into Kitty mode via passthrough, after which
//! keystrokes arrive as CSI-u rather than raw bytes.

/// One key in the detach sequence, with both byte encodings precomputed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetachKey {
    raw: u8,
    csiu: Vec<u8>,
}

/// A parsed detach-key sequence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetachKeys {
    keys: Vec<DetachKey>,
}

impl DetachKeys {
    /// Parse a comma-separated detach-key spec.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let mut keys = Vec::new();
        for token in spec.split(',') {
            let token = token.trim();
            if token.is_empty() {
                return Err(format!("empty key in detach sequence {spec:?}"));
            }
            keys.push(DetachKey::parse(token)?);
        }
        if keys.is_empty() {
            return Err("detach sequence is empty".to_string());
        }
        Ok(Self { keys })
    }

    /// Parse the spec, defaulting to `ctrl-q,ctrl-q` when none is given.
    pub fn parse_or_default(spec: Option<&str>) -> Result<Self, String> {
        Self::parse(spec.unwrap_or("ctrl-q,ctrl-q"))
    }

    pub(crate) fn keys(&self) -> &[DetachKey] {
        &self.keys
    }
}

impl DetachKey {
    pub(crate) fn raw(&self) -> u8 {
        self.raw
    }
    pub(crate) fn csiu(&self) -> &[u8] {
        &self.csiu
    }

    fn parse(token: &str) -> Result<Self, String> {
        if let Some(rest) = token.strip_prefix("ctrl-") {
            let c = single_ascii(rest, token)?;
            let lower = c.to_ascii_lowercase();
            let code = lower as u32;
            Ok(DetachKey {
                raw: (lower as u8) & 0x1f,
                csiu: format!("\x1b[{code};5u").into_bytes(), // mods 5 = ctrl
            })
        } else {
            let c = single_ascii(token, token)?;
            let code = c as u32;
            Ok(DetachKey {
                raw: c as u8,
                csiu: format!("\x1b[{code}u").into_bytes(), // unmodified
            })
        }
    }
}

fn single_ascii(s: &str, token: &str) -> Result<char, String> {
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii() => Ok(c),
        (Some(_), None) => Err(format!("detach key {token:?} must be ASCII")),
        _ => Err(format!("detach key {token:?} must be a single char or ctrl-<char>")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_ctrl_q_sequence() {
        let keys = DetachKeys::parse("ctrl-q,ctrl-q").unwrap();
        assert_eq!(keys.keys().len(), 2);
        assert_eq!(keys.keys()[0].raw(), 0x11); // 'q' & 0x1f
        assert_eq!(keys.keys()[0].csiu(), b"\x1b[113;5u"); // 'q' = 113
    }

    #[test]
    fn parses_mixed_ctrl_and_literal() {
        let keys = DetachKeys::parse("ctrl-a,d").unwrap();
        assert_eq!(keys.keys()[0].raw(), 0x01);
        assert_eq!(keys.keys()[0].csiu(), b"\x1b[97;5u"); // 'a' = 97
        assert_eq!(keys.keys()[1].raw(), b'd');
        assert_eq!(keys.keys()[1].csiu(), b"\x1b[100u"); // 'd' = 100, unmodified
    }

    #[test]
    fn ctrl_is_case_insensitive() {
        let keys = DetachKeys::parse("ctrl-Q").unwrap();
        assert_eq!(keys.keys()[0].raw(), 0x11);
        assert_eq!(keys.keys()[0].csiu(), b"\x1b[113;5u");
    }

    #[test]
    fn rejects_empty_and_malformed_tokens() {
        assert!(DetachKeys::parse("ctrl-q,,ctrl-q").is_err()); // empty token
        assert!(DetachKeys::parse("ctrl-").is_err()); // no char after ctrl-
        assert!(DetachKeys::parse("ab").is_err()); // two-char literal
        assert!(DetachKeys::parse("").is_err()); // empty spec
    }
}
