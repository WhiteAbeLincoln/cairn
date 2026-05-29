//! `cairn kick` / `cairn detach`: detach attached clients without killing the
//! session. Multi-target; idempotent on `session.not_found` (the desired
//! terminal state is already true).

use anyhow::Result;
use cairn_protocol::client::cairn::daemon::sessions;
use futures::stream::{FuturesUnordered, StreamExt as _};

use crate::cli::SessionTargets;
use crate::connect::Client;
use crate::targets;

pub async fn run(
    client: &Client,
    sessions_arg: &SessionTargets,
    client_filter: Option<&str>,
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
    let mut tasks = FuturesUnordered::new();
    for t in &resolved.matched {
        let client = client.clone();
        let id = t.id.clone();
        let token = t.name.clone().unwrap_or_else(|| t.id.clone());
        let client_filter = client_filter.map(|s| s.to_string());
        tasks.push(async move {
            let result = sessions::kick(&client, (), &id, client_filter).await;
            (token, result)
        });
    }
    while let Some((token, result)) = tasks.next().await {
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) if e.code == "session.not_found" => {
                tracing::info!(target = %token, "kick: session already gone (no-op success)");
            }
            Ok(Err(e)) => {
                eprintln!("error: {}: {}: {}", token, e.code, e.message);
                exit_code = 1;
            }
            Err(e) => {
                eprintln!("error: {token}: {e}");
                exit_code = 1;
            }
        }
    }
    Ok(exit_code)
}
