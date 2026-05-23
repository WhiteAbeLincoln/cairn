//! Unit-level tests of the libghostty-vt callbacks. The callbacks are
//! installed on a `Box<Terminal>` so the C side's userdata pointer
//! (which references `&self.vtable`) remains valid after the helper
//! returns ownership to the caller.
//!
//! # Why box?
//!
//! libghostty-vt 0.1.1's `on_*` installers take `&mut self` and store
//! a raw pointer to `self.vtable` as the C userdata. Idiomatically the
//! type should be `!Unpin` and the installers should require
//! `Pin<&mut Self>` — but they don't, so safe code can move the
//! `Terminal` post-install and trigger UB. Boxing first puts the
//! Terminal at a stable heap address; subsequent installs register
//! pointers into that heap allocation, which stay valid as long as
//! the Box is alive.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use cairn_pty::TermSize;

use bytes::Bytes;
use libghostty_vt::{Terminal, TerminalOptions};

/// Pending-write queue type for clarity.
type Pending = Rc<RefCell<VecDeque<Bytes>>>;

const DEFAULT_CELL_WIDTH_PX: u32 = 10;
const DEFAULT_CELL_HEIGHT_PX: u32 = 20;

/// Construct a Boxed Terminal with the gated callbacks installed,
/// mirroring the worker's setup (currently: `on_pty_write`,
/// `on_xtversion`, `on_size`, `on_color_scheme`).
///
/// IMPORTANT: the Box must be constructed *before* any callback is
/// installed. `Box::new(term)` moves `term` off the stack to the
/// heap; doing this AFTER `term.on_pty_write(...)` would invalidate
/// the userdata pointer the C side stored.
fn build_test_terminal(
    counter: Arc<AtomicUsize>,
    current_size: Rc<Cell<TermSize>>,
) -> (Box<Terminal<'static, 'static>>, Pending) {
    use libghostty_vt::terminal::SizeReportSize;

    let pending: Pending = Rc::default();
    let term = Terminal::new(TerminalOptions {
        cols: 80,
        rows: 24,
        max_scrollback: 100,
    })
    .expect("Terminal::new");

    // Box FIRST, install SECOND.
    let mut boxed = Box::new(term);

    let pending_cb = pending.clone();
    let pc_pty = counter.clone();
    boxed
        .on_pty_write(move |_t, data| {
            if pc_pty.load(Ordering::Relaxed) == 0 {
                pending_cb
                    .borrow_mut()
                    .push_back(Bytes::copy_from_slice(data));
            }
        })
        .expect("on_pty_write");

    const XTVERSION_REPLY: &str = concat!("cairn ", env!("CARGO_PKG_VERSION"));
    let pc_xt = counter.clone();
    boxed
        .on_xtversion(move |_t| {
            if pc_xt.load(Ordering::Relaxed) == 0 {
                Some(XTVERSION_REPLY)
            } else {
                None
            }
        })
        .expect("on_xtversion");

    let pc_size = counter.clone();
    let cs = current_size.clone();
    boxed
        .on_size(move |_t| {
            if pc_size.load(Ordering::Relaxed) == 0 {
                let s = cs.get();
                Some(SizeReportSize {
                    rows: s.rows,
                    columns: s.cols,
                    cell_width: DEFAULT_CELL_WIDTH_PX,
                    cell_height: DEFAULT_CELL_HEIGHT_PX,
                })
            } else {
                None
            }
        })
        .expect("on_size");

    boxed.on_color_scheme(|_t| None).expect("on_color_scheme");

    (boxed, pending)
}

// ─── on_pty_write gating ──────────────────────────────────────────────

#[test]
fn on_pty_write_emits_da1_reply_when_no_primary() {
    let counter = Arc::new(AtomicUsize::new(0));
    let (mut term, pending) =
        build_test_terminal(counter, Rc::new(Cell::new(TermSize { cols: 80, rows: 24 })));
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
    let counter = Arc::new(AtomicUsize::new(1));
    let (mut term, pending) =
        build_test_terminal(counter, Rc::new(Cell::new(TermSize { cols: 80, rows: 24 })));
    term.vt_write(b"\x1b[c");
    assert!(
        pending.borrow().is_empty(),
        "expected no reply when count >= 1, got {:?}",
        pending.borrow()
    );
}

#[test]
fn on_pty_write_gates_decrqm_reply() {
    let counter = Arc::new(AtomicUsize::new(0));
    let (mut term, pending) = build_test_terminal(
        counter.clone(),
        Rc::new(Cell::new(TermSize { cols: 80, rows: 24 })),
    );

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

// ─── on_xtversion override + gating ───────────────────────────────────

#[test]
fn on_xtversion_overrides_default_when_no_primary() {
    let counter = Arc::new(AtomicUsize::new(0));
    let (mut term, pending) =
        build_test_terminal(counter, Rc::new(Cell::new(TermSize { cols: 80, rows: 24 })));
    term.vt_write(b"\x1b[>q"); // XTVERSION query
    let chunks: Vec<_> = pending.borrow_mut().drain(..).collect();
    assert_eq!(chunks.len(), 1, "expected one reply, got {chunks:?}");
    let reply = std::str::from_utf8(&chunks[0]).unwrap_or("<non-utf8>");
    assert!(
        reply.contains("cairn "),
        "reply should brand as cairn, got {reply:?}"
    );
    assert!(
        reply.contains(env!("CARGO_PKG_VERSION")),
        "reply should include the crate version, got {reply:?}"
    );
    assert!(
        !reply.contains("libghostty"),
        "default libghostty fingerprint leaked: {reply:?}"
    );
}

#[test]
fn on_xtversion_suppressed_when_primary_attached() {
    let counter = Arc::new(AtomicUsize::new(1));
    let (mut term, pending) =
        build_test_terminal(counter, Rc::new(Cell::new(TermSize { cols: 80, rows: 24 })));
    term.vt_write(b"\x1b[>q");
    assert!(
        pending.borrow().is_empty(),
        "expected no XTVERSION reply with count == 1, got {:?}",
        pending.borrow()
    );
}

// ─── on_size XTWINOPS gating ──────────────────────────────────────────

#[test]
fn on_size_reports_cell_grid_when_no_primary() {
    let counter = Arc::new(AtomicUsize::new(0));
    let size = Rc::new(Cell::new(TermSize {
        cols: 132,
        rows: 50,
    }));
    let (mut term, pending) = build_test_terminal(counter, size);

    // CSI 18 t — report text area in chars. Wire form of reply:
    // CSI 8 ; rows ; cols t.
    term.vt_write(b"\x1b[18t");

    let chunks: Vec<_> = pending.borrow_mut().drain(..).collect();
    assert!(!chunks.is_empty(), "expected at least one reply chunk");
    let joined: Vec<u8> = chunks.iter().flat_map(|c| c.iter().copied()).collect();
    assert_eq!(
        joined.as_slice(),
        b"\x1b[8;50;132t",
        "unexpected XTWINOPS 18t reply"
    );
}

#[test]
fn on_size_suppressed_when_primary_attached() {
    let counter = Arc::new(AtomicUsize::new(1));
    let size = Rc::new(Cell::new(TermSize { cols: 80, rows: 24 }));
    let (mut term, pending) = build_test_terminal(counter, size);
    term.vt_write(b"\x1b[18t");
    assert!(
        pending.borrow().is_empty(),
        "expected no reply with count == 1, got {:?}",
        pending.borrow()
    );
}

// ─── on_color_scheme always None ──────────────────────────────────────

#[test]
fn on_color_scheme_never_replies_regardless_of_count() {
    for count in [0, 1] {
        let counter = Arc::new(AtomicUsize::new(count));
        let size = Rc::new(Cell::new(TermSize { cols: 80, rows: 24 }));
        let (mut term, pending) = build_test_terminal(counter, size);
        term.vt_write(b"\x1b[?996n"); // color scheme query
        assert!(
            pending.borrow().is_empty(),
            "expected no reply at count={count}, got {:?}",
            pending.borrow()
        );
    }
}
