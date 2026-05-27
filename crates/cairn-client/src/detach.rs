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

enum Step {
    Full,
    Partial,
    NotPrefix,
}

/// Streaming matcher: feed input bytes, get back the bytes to forward to the
/// session and whether the detach sequence completed.
pub struct Matcher {
    keys: Vec<DetachKey>,
    withheld: Vec<u8>,
}

impl Matcher {
    pub fn new(keys: DetachKeys) -> Self {
        Self { keys: keys.keys, withheld: Vec::new() }
    }

    /// Feed `input`; append forwardable bytes to `out`. Returns true when the
    /// detach sequence has completed (bytes after the sequence in this call are
    /// dropped — detach ends the stream anyway).
    pub fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) -> bool {
        for &b in input {
            self.withheld.push(b);
            loop {
                match self.try_match() {
                    Step::Full => {
                        self.withheld.clear();
                        return true;
                    }
                    Step::Partial => break,
                    Step::NotPrefix => {
                        // The front byte can't begin a match: release it as input.
                        out.push(self.withheld.remove(0));
                        if self.withheld.is_empty() {
                            break;
                        }
                    }
                }
            }
        }
        false
    }

    /// Match the key sequence against the front of `withheld`. At each position
    /// the next byte selects the encoding: `0x1b` => Kitty CSI-u, else raw byte.
    fn try_match(&self) -> Step {
        let buf = &self.withheld;
        let mut j = 0;
        for key in &self.keys {
            if j >= buf.len() {
                return Step::Partial;
            }
            if buf[j] == 0x1b {
                let need = key.csiu();
                let avail = &buf[j..];
                let n = avail.len().min(need.len());
                if avail[..n] != need[..n] {
                    return Step::NotPrefix;
                }
                if n < need.len() {
                    return Step::Partial;
                }
                j += need.len();
            } else {
                if buf[j] != key.raw() {
                    return Step::NotPrefix;
                }
                j += 1;
            }
        }
        Step::Full
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

    fn feed_all(spec: &str, input: &[u8]) -> (Vec<u8>, bool) {
        let mut m = Matcher::new(DetachKeys::parse(spec).unwrap());
        let mut out = Vec::new();
        let detached = m.feed(input, &mut out);
        (out, detached)
    }

    #[test]
    fn raw_sequence_detaches_and_forwards_nothing() {
        let (out, detached) = feed_all("ctrl-q,ctrl-q", &[0x11, 0x11]);
        assert!(detached);
        assert!(out.is_empty());
    }

    #[test]
    fn partial_then_mismatch_flushes_withheld_bytes() {
        let mut m = Matcher::new(DetachKeys::parse("ctrl-q,ctrl-q").unwrap());
        let mut out = Vec::new();
        // First ctrl-q is withheld (could start the sequence).
        assert!(!m.feed(&[0x11], &mut out));
        assert!(out.is_empty());
        // A non-ctrl-q breaks it: both bytes are released as input.
        assert!(!m.feed(&[b'x'], &mut out));
        assert_eq!(out, vec![0x11, b'x']);
    }

    #[test]
    fn csiu_sequence_detaches() {
        let (out, detached) = feed_all("ctrl-q,ctrl-q", b"\x1b[113;5u\x1b[113;5u");
        assert!(detached, "Kitty CSI-u encoding of ctrl-q,ctrl-q should detach");
        assert!(out.is_empty());
    }

    #[test]
    fn mixed_csiu_then_raw_detaches() {
        // ctrl-a as CSI-u, then literal `d` as a raw byte.
        let (out, detached) = feed_all("ctrl-a,d", b"\x1b[97;5ud");
        assert!(detached);
        assert!(out.is_empty());
    }

    #[test]
    fn non_matching_escape_sequence_is_forwarded() {
        // An up-arrow (\x1b[A) shares the \x1b[ prefix with CSI-u but isn't a
        // detach key — it must be forwarded intact.
        let (out, detached) = feed_all("ctrl-q,ctrl-q", b"\x1b[A");
        assert!(!detached);
        assert_eq!(out, b"\x1b[A");
    }

    #[test]
    fn lone_ctrl_q_inside_a_run_does_not_detach() {
        let (out, detached) = feed_all("ctrl-q,ctrl-q", b"a\x11b");
        assert!(!detached);
        assert_eq!(out, b"a\x11b");
    }
}
