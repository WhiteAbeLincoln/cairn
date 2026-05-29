//! `cairn send <target> [-r/--raw] [input...]`: inject bytes into a session.
//! Argv form joins with single spaces and appends `\r` (Enter) unless `--raw`.
//! `\r` matches what a real terminal writes to the pty master on Enter — the
//! line discipline's `ICRNL` translates it to `\n` for cooked-mode readers
//! (shells, REPLs), while raw-mode TUIs receive `\r` directly as they expect.
//! Stdin form streams 8 KiB chunks raw.

use anyhow::Result;
use bytes::Bytes;
use cairn_protocol::client::cairn::daemon::sessions;
use futures::Stream;
use tokio::io::AsyncReadExt as _;

use crate::cli::SessionTarget;
use crate::connect::Endpoint;
use crate::targets;

const CHUNK_SIZE: usize = 8 * 1024;

pub async fn run(
    endpoint: &Endpoint,
    target: &SessionTarget,
    raw: bool,
    input: &[String],
) -> Result<i32> {
    let resolved = match targets::resolve_one(endpoint, target).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return Ok(1);
        }
    };
    let chunks: std::pin::Pin<Box<dyn Stream<Item = Vec<Bytes>> + Send>> = if input.is_empty() {
        Box::pin(stdin_stream())
    } else {
        let chunk = argv_to_chunk(input, raw);
        Box::pin(futures::stream::iter(vec![vec![chunk]]))
    };
    let client = endpoint.client().await?;
    // `sessions::send` returns `Result<(Result<(), Error>, Option<io_future>)>`;
    // the io future drives the underlying transport and must be spawned.
    match sessions::send(&client, (), &resolved.id, chunks).await {
        Ok((wire, io)) => {
            if let Some(io) = io {
                tokio::spawn(async move {
                    let _ = io.await;
                });
            }
            match wire {
                Ok(()) => Ok(0),
                Err(e) => {
                    eprintln!("error: {}: {}", e.code, e.message);
                    Ok(1)
                }
            }
        }
        Err(e) => {
            eprintln!(
                "error: cannot reach cairn-daemon at {}: {e}",
                endpoint.label()
            );
            Ok(1)
        }
    }
}

fn argv_to_chunk(words: &[String], raw: bool) -> Bytes {
    let mut s = words.join(" ");
    if !raw {
        s.push('\r');
    }
    Bytes::from(s.into_bytes())
}

fn stdin_stream() -> impl Stream<Item = Vec<Bytes>> + Send {
    async_stream::stream! {
        let mut stdin = tokio::io::stdin();
        let mut buf = vec![0u8; CHUNK_SIZE];
        loop {
            match stdin.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => yield vec![Bytes::copy_from_slice(&buf[..n])],
                Err(_) => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_joined_with_space_and_enter() {
        let got = argv_to_chunk(&["hello".into(), "world".into()], false);
        assert_eq!(&got[..], b"hello world\r");
    }

    #[test]
    fn argv_raw_omits_trailing_newline() {
        let got = argv_to_chunk(&["hello".into()], true);
        assert_eq!(&got[..], b"hello");
    }

    #[test]
    fn argv_empty_in_raw_is_empty_chunk() {
        let got = argv_to_chunk(&[], true);
        assert!(got.is_empty());
    }
}
