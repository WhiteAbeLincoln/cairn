//! Unit-level tests of the libghostty-vt callbacks. Construct a bare
//! Terminal (no PTY, no worker), install the gated callbacks against
//! a hand-rolled Arc<AtomicUsize>, feed VT bytes via vt_write, and
//! assert what reaches the pending-writes queue.
//!
//! # Note on Terminal ownership
//!
//! libghostty-vt stores `&self.vtable` as a raw C pointer in the C library
//! when `on_pty_write` is called. Moving the `Terminal` after callback
//! installation invalidates that pointer. All tests therefore construct
//! the terminal directly in the test body and never return it from a helper —
//! only the `Rc<RefCell<VecDeque<Bytes>>>` queue is shared via clone.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bytes::Bytes;
use libghostty_vt::{Terminal, TerminalOptions};

#[test]
fn on_pty_write_emits_da1_reply_when_no_primary() {
    let counter: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
    let pending: Rc<RefCell<VecDeque<Bytes>>> = Rc::default();

    let mut term = Terminal::new(TerminalOptions {
        cols: 80,
        rows: 24,
        max_scrollback: 100,
    })
    .expect("Terminal::new");

    let pending_cb = pending.clone();
    let pc = counter.clone();
    term.on_pty_write(move |_t, data| {
        if pc.load(Ordering::Relaxed) == 0 {
            pending_cb.borrow_mut().push_back(Bytes::copy_from_slice(data));
        }
    })
    .expect("on_pty_write");

    term.vt_write(b"\x1b[c"); // DA1
    let chunks: Vec<_> = pending.borrow_mut().drain(..).collect();
    assert_eq!(chunks.len(), 1, "expected one reply chunk, got {chunks:?}");
    assert_eq!(
        chunks[0].as_ref(),
        b"\x1b[?62;22c",
        "DA1 wire reply mismatch"
    );
}

#[test]
fn on_pty_write_suppresses_da1_reply_when_primary_attached() {
    let counter: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(1));
    let pending: Rc<RefCell<VecDeque<Bytes>>> = Rc::default();

    let mut term = Terminal::new(TerminalOptions {
        cols: 80,
        rows: 24,
        max_scrollback: 100,
    })
    .expect("Terminal::new");

    let pending_cb = pending.clone();
    let pc = counter.clone();
    term.on_pty_write(move |_t, data| {
        if pc.load(Ordering::Relaxed) == 0 {
            pending_cb.borrow_mut().push_back(Bytes::copy_from_slice(data));
        }
    })
    .expect("on_pty_write");

    term.vt_write(b"\x1b[c"); // DA1
    assert!(
        pending.borrow().is_empty(),
        "expected no reply when count >= 1, got {:?}",
        pending.borrow()
    );
}

#[test]
fn on_pty_write_gates_decrqm_reply() {
    let counter: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
    let pending: Rc<RefCell<VecDeque<Bytes>>> = Rc::default();

    let mut term = Terminal::new(TerminalOptions {
        cols: 80,
        rows: 24,
        max_scrollback: 100,
    })
    .expect("Terminal::new");

    let pending_cb = pending.clone();
    let pc = counter.clone();
    term.on_pty_write(move |_t, data| {
        if pc.load(Ordering::Relaxed) == 0 {
            pending_cb.borrow_mut().push_back(Bytes::copy_from_slice(data));
        }
    })
    .expect("on_pty_write");

    // Count == 0: DECRQM reply queued.
    term.vt_write(b"\x1b[?7$p");
    assert_eq!(pending.borrow().len(), 1, "DECRQM reply missing at count 0");
    pending.borrow_mut().clear();

    // Count == 1: DECRQM reply suppressed.
    counter.store(1, Ordering::Relaxed);
    term.vt_write(b"\x1b[?7$p");
    assert!(
        pending.borrow().is_empty(),
        "DECRQM reply leaked at count 1: {:?}",
        pending.borrow()
    );
}
