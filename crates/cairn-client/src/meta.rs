//! `cairn whoami` and `cairn version`.

use anyhow::Result;
use cairn_protocol::client::cairn::daemon::meta;

use crate::connect::Client;

pub async fn whoami(client: &Client) -> Result<i32> {
    match meta::whoami(client, ()).await {
        Ok(Ok(identity)) => {
            println!("{identity}");
            Ok(0)
        }
        Ok(Err(e)) => {
            eprintln!("error: {}: {}", e.code, e.message);
            Ok(1)
        }
        Err(e) => {
            eprintln!("error: cannot reach cairn-daemon: {e}");
            Ok(1)
        }
    }
}

pub async fn version(client: &Client) -> Result<i32> {
    println!("cairn {}", env!("CARGO_PKG_VERSION"));
    match meta::version(client, ()).await {
        Ok(v) => println!("daemon: {} (protocol {})", v.daemon, v.protocol),
        Err(e) => println!("daemon: unreachable: {e}"),
    }
    Ok(0)
}
