//! `cairn logs <targets> [-f] [-n N] [--prefix] [--strip]`.

use anyhow::Result;
use bytes::Bytes;
use cairn_protocol::cairn::daemon::types::LogWindow;
use cairn_protocol::client::cairn::daemon::sessions;
use futures::stream::StreamExt as _;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::mpsc;

use crate::cli::SessionTargets;
use crate::connect::Client;
use crate::targets;

pub async fn run(
    client: &Client,
    sessions_arg: &SessionTargets,
    strip: bool,
    prefix: bool,
    follow: bool,
    tail: Option<usize>,
) -> Result<i32> {
    let resolved = match targets::resolve_many(client, sessions_arg).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return Ok(1);
        }
    };
    let mut exit_code = 0;
    for t in &resolved.unresolved {
        eprintln!("error: {t}: no session matches");
        exit_code = 1;
    }
    if resolved.matched.is_empty() {
        eprintln!("no sessions matched");
        return Ok(2);
    }

    let window = tail
        .map(|n| LogWindow::Tail(n as u32))
        .unwrap_or(LogWindow::All);
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(64);

    let mut stream_tasks = Vec::new();
    for t in &resolved.matched {
        let id = t.id.clone();
        let label = display_label(&t.name, &t.id);
        let out_tx = out_tx.clone();
        let client = client.clone();
        stream_tasks.push(tokio::spawn(async move {
            let result = sessions::logs(&client, (), &id, &window, follow).await;
            let (mut stream, io) = match result {
                Ok(pair) => pair,
                Err(e) => {
                    eprintln!("error: {label}: {e}");
                    return 1i32;
                }
            };
            if let Some(io) = io {
                tokio::spawn(async move {
                    let _ = io.await;
                });
            }
            let mut buf = LineBuffer::new(if prefix { Some(label.clone()) } else { None });
            while let Some(batch) = stream.next().await {
                for chunk in batch {
                    let bytes: &[u8] = &chunk;
                    let bytes = if strip {
                        Bytes::from(strip_ansi_escapes::strip(bytes))
                    } else {
                        Bytes::copy_from_slice(bytes)
                    };
                    for piece in buf.feed(&bytes) {
                        if out_tx.send(piece).await.is_err() {
                            return 0;
                        }
                    }
                }
            }
            if let Some(rest) = buf.flush() {
                let _ = out_tx.send(rest).await;
            }
            0
        }));
    }
    drop(out_tx); // close the channel once all stream tasks finish

    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(batch) = out_rx.recv().await {
            if stdout.write_all(&batch).await.is_err() {
                break;
            }
        }
        let _ = stdout.flush().await;
    });

    for t in stream_tasks {
        if let Ok(code) = t.await
            && code != 0
        {
            exit_code = code;
        }
    }
    let _ = writer.await;
    Ok(exit_code)
}

fn display_label(name: &Option<String>, id: &str) -> String {
    match name {
        Some(n) => n.clone(),
        None => id.chars().take(8).collect(),
    }
}

/// Re-emit bytes line-by-line, optionally prefixing each completed line with
/// `<label>: `. Partial trailing line is buffered and flushed on stream end.
pub struct LineBuffer {
    prefix: Option<String>,
    partial: Vec<u8>,
}

impl LineBuffer {
    pub fn new(prefix: Option<String>) -> Self {
        Self {
            prefix,
            partial: Vec::new(),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut cursor = 0;
        for (i, b) in bytes.iter().enumerate() {
            if *b == b'\n' {
                self.partial.extend_from_slice(&bytes[cursor..=i]);
                out.push(self.take_line());
                cursor = i + 1;
            }
        }
        if cursor < bytes.len() {
            self.partial.extend_from_slice(&bytes[cursor..]);
        }
        out
    }

    pub fn flush(&mut self) -> Option<Vec<u8>> {
        if self.partial.is_empty() {
            return None;
        }
        Some(self.take_line())
    }

    fn take_line(&mut self) -> Vec<u8> {
        let line = std::mem::take(&mut self.partial);
        match &self.prefix {
            None => line,
            Some(p) => {
                let mut prefixed = Vec::with_capacity(p.len() + 2 + line.len());
                prefixed.extend_from_slice(p.as_bytes());
                prefixed.extend_from_slice(b": ");
                prefixed.extend_from_slice(&line);
                prefixed
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linebuf_no_prefix_passthrough() {
        let mut b = LineBuffer::new(None);
        let out = b.feed(b"hello\nworld\n");
        assert_eq!(out, vec![b"hello\n".to_vec(), b"world\n".to_vec()]);
        assert!(b.flush().is_none());
    }

    #[test]
    fn linebuf_prefix_per_complete_line() {
        let mut b = LineBuffer::new(Some("x".into()));
        let out = b.feed(b"a\nb\n");
        assert_eq!(out, vec![b"x: a\n".to_vec(), b"x: b\n".to_vec()]);
    }

    #[test]
    fn linebuf_partial_buffered_until_next_newline() {
        let mut b = LineBuffer::new(Some("x".into()));
        let first = b.feed(b"hel");
        assert!(first.is_empty());
        let second = b.feed(b"lo\n");
        assert_eq!(second, vec![b"x: hello\n".to_vec()]);
    }

    #[test]
    fn linebuf_flush_emits_unterminated_tail() {
        let mut b = LineBuffer::new(Some("x".into()));
        let _ = b.feed(b"trail");
        let last = b.flush().expect("partial line should flush");
        assert_eq!(last, b"x: trail".to_vec());
    }
}
