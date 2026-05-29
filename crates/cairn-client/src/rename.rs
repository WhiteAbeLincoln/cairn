//! `cairn rename <target> --to <new-name>`.

use anyhow::Result;
use cairn_protocol::client::cairn::daemon::sessions;

use crate::cli::SessionTarget;
use crate::connect::Endpoint;
use crate::targets;

pub async fn run(endpoint: &Endpoint, target: &SessionTarget, new_name: &str) -> Result<i32> {
    let resolved = match targets::resolve_one(endpoint, target).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return Ok(1);
        }
    };
    let client = endpoint.client().await?;
    match sessions::rename(&client, (), &resolved.id, new_name).await {
        Ok(Ok(())) => Ok(0),
        Ok(Err(e)) => {
            eprintln!("error: {}: {}", e.code, e.message);
            Ok(1)
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
