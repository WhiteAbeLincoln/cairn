use std::io::{Read as _, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;

use async_graphql::{Context, Result, Subscription};
use async_stream::stream;
use futures_core::Stream;
use notify::{RecursiveMode, Watcher};
use serde_json::Value;

use super::types::{Event, SessionData};
use crate::session::loader;

pub struct SubscriptionRoot;

#[Subscription]
impl SubscriptionRoot {
    /// Watch a session's log file and emit new events as they are appended.
    async fn session_events(
        &self,
        ctx: &Context<'_>,
        id: String,
    ) -> Result<impl Stream<Item = Event>> {
        let base_path = ctx.data::<PathBuf>()?;
        let sessions = loader::discover_sessions(base_path)
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        let session = sessions
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| async_graphql::Error::new(format!("Session {id} not found")))?;
        let path = session.path.clone();

        // Start tailing from current end of file.
        let initial_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

        // Channel for file-change notifications.
        let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(16);
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res
                && event.kind.is_modify()
            {
                let _ = tx.try_send(());
            }
        })
        .map_err(|e| async_graphql::Error::new(format!("Failed to create watcher: {e}")))?;

        watcher
            .watch(&path, RecursiveMode::NonRecursive)
            .map_err(|e| async_graphql::Error::new(format!("Failed to watch file: {e}")))?;

        Ok(stream! {
            let _watcher = watcher; // prevent drop
            let mut byte_pos = initial_size;

            while rx.recv().await.is_some() {
                // Drain buffered notifications so we read once per burst of writes.
                while rx.try_recv().is_ok() {}

                let new_size = match std::fs::metadata(&path) {
                    Ok(m) => m.len(),
                    Err(_) => continue,
                };
                if new_size <= byte_pos {
                    continue;
                }

                let mut file = match std::fs::File::open(&path) {
                    Ok(f) => f,
                    Err(_) => continue,
                };
                if file.seek(SeekFrom::Start(byte_pos)).is_err() {
                    continue;
                }
                let mut buf = String::new();
                if file.read_to_string(&mut buf).is_err() {
                    continue;
                }
                byte_pos = new_size;

                let new_events: Vec<Value> = buf
                    .lines()
                    .filter(|l| !l.is_empty())
                    .filter_map(|l| serde_json::from_str(l).ok())
                    .collect();

                if new_events.is_empty() {
                    continue;
                }

                let data = Arc::new(SessionData::new(
                    new_events,
                    path.display().to_string(),
                ));
                for i in 0..data.raw.len() {
                    yield data.make_event(i);
                }
            }
        })
    }
}
