//! Spawn a bash and send it an echo command, then print what came back.
//!
//! Run with: `nix develop --command cargo run -p cairn-pty --example echo`

use cairn_pty::{ClientId, GhosttyPty, PtySession, SpawnOptions};

#[tokio::main]
async fn main() {
    let mut cmd = tokio::process::Command::new("bash");
    cmd.arg("-i");
    let opts = SpawnOptions::new(cmd);
    let pty = GhosttyPty::spawn(opts).expect("spawn");
    let client = ClientId::from_u64(0);
    let mut sub = pty.subscribe(client).await.expect("subscribe");
    println!("snapshot length: {}", sub.snapshot.len());
    pty.write(client, bytes::Bytes::from_static(b"echo hello-from-cairn\n"))
        .await
        .expect("write");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let mut total = 0usize;
    let mut buff = Vec::new();
    while let Ok(chunk) = sub.stream.try_recv() {
        total += chunk.len();
        buff.extend_from_slice(&chunk);
    }
    println!("received {total} bytes from bash\n");
    println!("output:\n{}\n", String::from_utf8_lossy(&buff));
    // print escaped as well, to show newlines and control characters
    println!("output (escaped):\n{:?}\n", String::from_utf8_lossy(&buff));
    pty.kill().expect("kill");
}
