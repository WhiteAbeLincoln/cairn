//! Integration tests for the non-interactive `cairn` commands. Each test
//! spins a fresh in-process daemon (via `common::Harness`) and invokes the
//! real `cairn` binary against it.

mod common;

use common::{Harness, stderr_str, stdout_str};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn harness_smoke() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    // No sessions yet — list_all is non-fatal and returns empty.
    let xs = h.list_all().await?;
    assert!(xs.is_empty(), "fresh daemon has no sessions; got {xs:?}");
    Ok(())
}
