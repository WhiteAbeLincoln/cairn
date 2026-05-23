//! Classify a write payload as "user input" or "terminal back-chatter."
//!
//! Used by the worker to decide whether a write from a non-leader
//! client should promote that client to leader. Mirrors zmx's
//! `util.isUserInput` (`zmx/src/util.zig:446-477`) with one deliberate
//! divergence: mouse press/release/scroll/drag DO qualify as user
//! input. See the spec ("Divergences from zmx") for the rationale.

use vte::{Params, Parser, Perform};

/// Returns true if any byte or recognized escape sequence in `data`
/// could only have come from intentional human interaction (typing,
/// clicking, scrolling) and not from terminal-emitted back-chatter
/// (mouse motion, focus events, query replies).
pub(crate) fn is_user_input(data: &[u8]) -> bool {
    // X10 mouse: ESC [ M <btn> <col> <row>. vte may consume the three
    // trailing bytes inside its state machine differently across
    // versions; this pre-check covers it explicitly.
    if data.len() >= 4 && data[0] == 0x1b && data[1] == b'[' && data[2] == b'M' {
        return true;
    }

    let mut classifier = Classifier::default();
    let mut parser = Parser::new();
    for &b in data {
        parser.advance(&mut classifier, b);
    }
    classifier.found
}

#[derive(Default)]
struct Classifier {
    found: bool,
}

impl Perform for Classifier {
    fn print(&mut self, _c: char) {
        self.found = true;
    }

    fn execute(&mut self, byte: u8) {
        if matches!(byte, 0x08 | 0x09 | 0x0A | 0x0D | 0x7F) {
            self.found = true;
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &Params,
        intermediates: &[u8],
        _ignore: bool,
        action: char,
    ) {
        match (intermediates, action) {
            // Focus in/out — terminal back-chatter, not user input.
            (b"", 'I') | (b"", 'O') => {}

            // SGR mouse press / release / scroll / drag / motion.
            (b"<", 'M') | (b"<", 'm') => {
                let button = params
                    .iter()
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(0);
                let motion = button & 32 != 0;
                let scroll = button & 64 != 0;
                let button_held = (button & 0b11) != 0b11;
                // Promote unless this is a pure motion event with no
                // button held and no scroll. Drag (motion + button) and
                // scroll both qualify.
                if !motion || button_held || scroll {
                    self.found = true;
                }
            }

            // Kitty keyboard protocol.
            (b"", 'u') => self.found = true,

            // Legacy modified-key sequences: CSI 1 ; <mod> <final>.
            (b"", a)
                if matches!(a, 'A' | 'B' | 'C' | 'D' | 'F' | 'H' | 'P' | 'Q' | 'R' | 'S') =>
            {
                let first = params.iter().next().and_then(|p| p.first().copied());
                let mod_param = params.iter().nth(1).and_then(|p| p.first().copied());
                if first == Some(1) && mod_param.is_some_and(|m| m >= 2) {
                    self.found = true;
                }
            }

            // Everything else (DA1/DA2 replies, DSR, DECRQM, etc.) is
            // not user input.
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, _params: &[&[u8]], _bell: bool) {}
    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, _byte: u8) {}
    fn hook(
        &mut self,
        _params: &Params,
        _intermediates: &[u8],
        _ignore: bool,
        _action: char,
    ) {
    }
    fn put(&mut self, _byte: u8) {}
    fn unhook(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::is_user_input;

    #[test]
    fn empty_payload_is_not_user_input() {
        assert!(!is_user_input(b""));
    }

    #[test]
    fn printable_ascii_is_user_input() {
        assert!(is_user_input(b"a"));
        assert!(is_user_input(b"hello"));
    }

    #[test]
    fn carriage_return_is_user_input() {
        assert!(is_user_input(b"\r"));
    }

    #[test]
    fn backspace_is_user_input() {
        assert!(is_user_input(b"\x08"));
        assert!(is_user_input(b"\x7F"));
    }

    #[test]
    fn tab_and_newline_are_user_input() {
        assert!(is_user_input(b"\t"));
        assert!(is_user_input(b"\n"));
    }

    #[test]
    fn ctrl_up_arrow_is_user_input() {
        // ESC [ 1 ; 5 A
        assert!(is_user_input(b"\x1b[1;5A"));
    }

    #[test]
    fn kitty_modified_key_is_user_input() {
        // ESC [ 97 ; 5 u (Ctrl-a in kitty protocol)
        assert!(is_user_input(b"\x1b[97;5u"));
    }

    #[test]
    fn da1_query_is_not_user_input() {
        // ESC [ c — the shell would send this; classifier sees the
        // shape and refuses to promote on it.
        assert!(!is_user_input(b"\x1b[c"));
    }

    #[test]
    fn da1_reply_is_not_user_input() {
        // ESC [ ? 6 2 ; 2 2 c
        assert!(!is_user_input(b"\x1b[?62;22c"));
    }

    #[test]
    fn focus_events_are_not_user_input() {
        assert!(!is_user_input(b"\x1b[I"));
        assert!(!is_user_input(b"\x1b[O"));
    }

    #[test]
    fn mouse_press_is_user_input() {
        // ESC [ < 0 ; 10 ; 20 M  (button 0 press at col 10 row 20)
        assert!(is_user_input(b"\x1b[<0;10;20M"));
    }

    #[test]
    fn mouse_release_is_user_input() {
        assert!(is_user_input(b"\x1b[<0;10;20m"));
    }

    #[test]
    fn mouse_right_button_is_user_input() {
        assert!(is_user_input(b"\x1b[<2;10;20M"));
    }

    #[test]
    fn mouse_scroll_is_user_input() {
        // Button 64 = scroll up, 65 = scroll down.
        assert!(is_user_input(b"\x1b[<64;10;20M"));
        assert!(is_user_input(b"\x1b[<65;10;20M"));
    }

    #[test]
    fn mouse_drag_is_user_input() {
        // Button 32 = motion flag + button 0 held (drag).
        assert!(is_user_input(b"\x1b[<32;10;20M"));
    }

    #[test]
    fn mouse_motion_only_is_not_user_input() {
        // Button 35 = motion flag (32) + button-none (3).
        assert!(!is_user_input(b"\x1b[<35;10;20M"));
    }

    #[test]
    fn x10_mouse_is_user_input() {
        // ESC [ M <btn> <col> <row>  (button 0 press)
        assert!(is_user_input(b"\x1b[M\x20\x20\x20"));
    }

    #[test]
    fn x10_mouse_at_high_column_is_user_input() {
        // X10 encodes button/col/row with a +32 offset. When all three
        // coordinates exceed 95 (i.e. the encoded bytes are > 0x7F), vte
        // dispatches each as execute() rather than print(), and none of
        // those bytes match any execute() arm that sets `found = true`.
        // The csi_dispatch for `([], 'M')` also falls through to `_ => {}`.
        // Without the X10-specific pre-filter in is_user_input, this
        // sequence would be incorrectly classified as non-user-input.
        // e.g. button=100-32=68, col=100-32=68, row=100-32=68, all encoded as 0x84.
        assert!(is_user_input(b"\x1b[M\x84\x84\x84"));
    }

    #[test]
    fn disjunctive_payload_promotes() {
        // Motion-only followed by 'a' — qualifies because 'a' qualifies.
        assert!(is_user_input(b"\x1b[<35;10;20Ma"));
    }

}
