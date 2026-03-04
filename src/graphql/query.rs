use std::path::PathBuf;

use async_graphql::{Context, Object, Result};

use super::types::{PageInput, Session};
use crate::session::loader;

/// Paginated sessions list result.
pub struct SessionsResult {
    sessions: Vec<Session>,
    total: i32,
}

#[Object]
impl SessionsResult {
    async fn sessions(&self) -> &[Session] {
        &self.sessions
    }

    async fn total(&self) -> i32 {
        self.total
    }
}

pub struct Query;

#[Object]
impl Query {
    /// List discovered sessions, optionally filtered by project name and paginated.
    async fn sessions(
        &self,
        ctx: &Context<'_>,
        project: Option<String>,
        page: Option<PageInput>,
    ) -> Result<SessionsResult> {
        let base_path = ctx.data::<PathBuf>()?;
        let all = loader::discover_sessions(base_path)
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;

        let filtered: Vec<Session> = all
            .iter()
            .filter(|s| {
                s.first_message.is_some()
                    && !s.is_sidechain
                    && project
                        .as_ref()
                        .map_or(true, |p| s.project.contains(p.as_str()))
            })
            .map(Session::from)
            .collect();

        let total = filtered.len() as i32;

        let sessions = match page {
            Some(p) => {
                let start = (p.offset as usize).min(filtered.len());
                let iter = filtered.into_iter().skip(start);
                if p.limit > 0 {
                    iter.take(p.limit as usize).collect()
                } else {
                    iter.collect()
                }
            }
            None => filtered,
        };

        Ok(SessionsResult { sessions, total })
    }

    /// Load a session by ID.
    async fn session(
        &self,
        ctx: &Context<'_>,
        id: String,
    ) -> Result<Option<Session>> {
        let base_path = ctx.data::<PathBuf>()?;
        let sessions = loader::discover_sessions(base_path)
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;

        Ok(sessions.iter().find(|s| s.id == id).map(Session::from))
    }
}
