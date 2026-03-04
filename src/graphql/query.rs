use std::path::PathBuf;

use async_graphql::{Context, Json, Object, Result};

use super::types::{AgentMapping, PageInput, Session, SessionEvents};
use crate::session::loader;

pub struct Query;

#[Object]
impl Query {
    /// List all discovered sessions, optionally filtered by project name.
    async fn sessions(
        &self,
        ctx: &Context<'_>,
        project: Option<String>,
    ) -> Result<Vec<Session>> {
        let base_path = ctx.data::<PathBuf>()?;
        let sessions = loader::discover_sessions(base_path)
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;

        let sessions: Vec<Session> = sessions
            .iter()
            .filter(|s| {
                // Hide sessions with no user messages and sidechain/subagent sessions
                s.first_message.is_some()
                    && !s.is_sidechain
                    && project
                        .as_ref()
                        .map_or(true, |p| s.project.contains(p.as_str()))
            })
            .map(Session::from)
            .collect();

        Ok(sessions)
    }

    /// Get metadata for a single session by ID.
    async fn session_info(
        &self,
        ctx: &Context<'_>,
        id: String,
    ) -> Result<Option<Session>> {
        let base_path = ctx.data::<PathBuf>()?;
        let sessions = loader::discover_sessions(base_path)
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;

        Ok(sessions.iter().find(|s| s.id == id).map(Session::from))
    }

    /// Get the raw JSONL content of a session file.
    async fn session_raw_log(
        &self,
        ctx: &Context<'_>,
        id: String,
    ) -> Result<Option<String>> {
        let base_path = ctx.data::<PathBuf>()?;
        let sessions = loader::discover_sessions(base_path)
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;

        let Some(session_info) = sessions.iter().find(|s| s.id == id) else {
            return Ok(None);
        };

        let content = std::fs::read_to_string(&session_info.path)
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;

        Ok(Some(content))
    }

    /// Get the mapping from tool_use_id to agent_id for subagent calls in a session.
    async fn session_agent_map(
        &self,
        ctx: &Context<'_>,
        id: String,
    ) -> Result<Vec<AgentMapping>> {
        let base_path = ctx.data::<PathBuf>()?;
        let sessions = loader::discover_sessions(base_path)
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;

        let Some(session_info) = sessions.iter().find(|s| s.id == id) else {
            return Ok(vec![]);
        };

        let mappings = loader::extract_agent_map(&session_info.path)
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;

        Ok(mappings
            .into_iter()
            .map(|(tool_use_id, agent_id)| AgentMapping {
                tool_use_id,
                agent_id,
            })
            .collect())
    }

    /// Load a session by ID, returning events as raw JSON.
    /// When `page` is provided, returns a paginated slice; otherwise returns all events.
    async fn session(
        &self,
        ctx: &Context<'_>,
        id: String,
        page: Option<PageInput>,
    ) -> Result<Option<SessionEvents>> {
        let base_path = ctx.data::<PathBuf>()?;
        let sessions = loader::discover_sessions(base_path)
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;

        let Some(session_info) = sessions.iter().find(|s| s.id == id) else {
            return Ok(None);
        };

        let all_events = loader::load_session_raw(&session_info.path)
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;

        let total = all_events.len() as i32;

        let events = match page {
            Some(p) => {
                let start = (p.offset as usize).min(all_events.len());
                let end = (start + p.limit as usize).min(all_events.len());
                all_events[start..end].to_vec()
            }
            None => all_events,
        };

        Ok(Some(SessionEvents {
            events: events.into_iter().map(Json).collect(),
            total,
        }))
    }
}
