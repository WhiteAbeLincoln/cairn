//! Spawn a bash and send it an echo command, then print what came back.
//!
//! Run with: `nix develop --command cargo run -p cairn-core --example echo`

use cairn_core::pty::{GhosttyPty, PtySession, SpawnOptions};

#[tokio::main]
async fn main() {
    let mut cmd = std::process::Command::new("bash");
    cmd.arg("-i");
    let opts = SpawnOptions::new(cmd);
    let pty = GhosttyPty::spawn(opts).expect("spawn");
    let mut sub = pty.subscribe().await.expect("subscribe");
    println!("snapshot length: {}", sub.snapshot.len());
    pty.write(bytes::Bytes::from_static(b"echo hello-from-cairn\n"))
        .await
        .expect("write");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let mut total = 0usize;
    while let Ok(chunk) = sub.stream.try_recv() {
        total += chunk.len();
    }
    println!("received {total} bytes from bash");
    pty.kill().expect("kill");
}
