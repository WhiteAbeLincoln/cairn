//! Resolve `SessionTarget` / `SessionTargets` against the daemon's `list_all`.
//! One `list_all` call per command; literal name-or-id match, glob match
//! against names (`*`, `?`, `[`), `--latest`, and `--all`. Per-token misses
//! inside a positional list are collected, not fatal.

use anyhow::Result;
use cairn_protocol::cairn::daemon::types::SessionInfo;

use crate::cli::{SessionTarget, SessionTargets};
use crate::connect::Endpoint;

/// A session that the user wants to operate on.
#[derive(Debug, Clone)]
pub struct ResolvedTarget {
    pub id: String,
    pub name: Option<String>,
    /// Full snapshot from `list_all`. Currently unused by callers — kept on
    /// the struct so future commands can skip a follow-up `inspect` round-trip
    /// when staleness is acceptable.
    #[allow(dead_code)]
    pub info: SessionInfo,
}

/// Outcome of resolving a `SessionTargets` set.
#[derive(Debug, Default)]
pub struct ResolvedMany {
    /// Sessions that matched, in stable first-occurrence order, de-duplicated.
    pub matched: Vec<ResolvedTarget>,
    /// Positional-list tokens (literal or glob) that matched nothing; surfaced
    /// per-target by the calling command, not fatal here.
    pub unresolved: Vec<String>,
}

pub async fn resolve_one(ep: &Endpoint, t: &SessionTarget) -> Result<ResolvedTarget> {
    let sessions = list_all(ep).await?;
    resolve_one_in(&sessions, t)
}

pub async fn resolve_many(ep: &Endpoint, t: &SessionTargets) -> Result<ResolvedMany> {
    let sessions = list_all(ep).await?;
    Ok(resolve_many_in(&sessions, t))
}

// ── Pure-logic core (tested below via fixtures, no daemon) ────────────────

fn resolve_one_in(sessions: &[SessionInfo], t: &SessionTarget) -> Result<ResolvedTarget> {
    if t.latest {
        let latest = sessions
            .iter()
            .max_by_key(|s| s.created_at_unix_ms)
            .ok_or_else(|| anyhow::anyhow!("no sessions to operate on"))?;
        return Ok(into_target(latest));
    }
    if let Some(s) = &t.session {
        return match find_literal(sessions, s) {
            Some(info) => Ok(into_target(info)),
            None => Err(anyhow::anyhow!("no session matches {s}")),
        };
    }
    anyhow::bail!("no session specified")
}

fn resolve_many_in(sessions: &[SessionInfo], t: &SessionTargets) -> ResolvedMany {
    let mut matched: Vec<ResolvedTarget> = Vec::new();
    let mut unresolved: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    let push = |sess: &SessionInfo,
                matched: &mut Vec<ResolvedTarget>,
                seen: &mut std::collections::HashSet<String>| {
        if seen.insert(sess.id.clone()) {
            matched.push(into_target(sess));
        }
    };

    if t.latest {
        if let Some(latest) = sessions.iter().max_by_key(|s| s.created_at_unix_ms) {
            push(latest, &mut matched, &mut seen);
        }
    } else if t.all {
        for s in sessions.iter().filter(|s| s.exit.is_none()) {
            push(s, &mut matched, &mut seen);
        }
    } else {
        for tok in &t.sessions {
            if is_glob(tok) {
                let any = match build_glob(tok) {
                    Ok(matcher) => {
                        let mut hit = false;
                        for s in sessions {
                            if let Some(name) = &s.name
                                && matcher.is_match(name)
                            {
                                push(s, &mut matched, &mut seen);
                                hit = true;
                            }
                        }
                        hit
                    }
                    Err(e) => {
                        eprintln!("warning: {tok}: invalid glob pattern: {e}");
                        false
                    }
                };
                if !any {
                    unresolved.push(tok.clone());
                }
            } else {
                match find_literal(sessions, tok) {
                    Some(info) => push(info, &mut matched, &mut seen),
                    None => unresolved.push(tok.clone()),
                }
            }
        }
    }
    ResolvedMany { matched, unresolved }
}

fn into_target(s: &SessionInfo) -> ResolvedTarget {
    ResolvedTarget { id: s.id.clone(), name: s.name.clone(), info: s.clone() }
}

fn find_literal<'a>(sessions: &'a [SessionInfo], tok: &str) -> Option<&'a SessionInfo> {
    sessions.iter().find(|s| s.name.as_deref() == Some(tok)).or_else(|| sessions.iter().find(|s| s.id == tok))
}

fn is_glob(tok: &str) -> bool {
    tok.contains('*') || tok.contains('?') || tok.contains('[')
}

fn build_glob(pat: &str) -> Result<globset::GlobMatcher, globset::Error> {
    Ok(globset::Glob::new(pat)?.compile_matcher())
}

async fn list_all(ep: &Endpoint) -> Result<Vec<SessionInfo>> {
    use cairn_protocol::client::cairn::daemon::sessions;
    let client = ep.client();
    sessions::list_all(&client, ())
        .await
        .map_err(|e| anyhow::anyhow!("cannot reach cairn-daemon at {}: {e}", ep.label()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_protocol::cairn::daemon::types::{ExitStatus, SessionSpec};

    fn spec() -> SessionSpec {
        SessionSpec {
            name: None,
            command: vec![],
            env: vec![],
            env_inherit: true,
            workdir: None,
            tty: false,
            stdin: false,
            idle_timeout_secs: None,
            scrollback_lines: 1000,
        }
    }

    fn s(id: &str, name: Option<&str>, created: u64, exited: bool) -> SessionInfo {
        SessionInfo {
            id: id.into(),
            name: name.map(str::to_string),
            pid: None,
            cols: 80,
            rows: 24,
            attached_clients: vec![],
            created_at_unix_ms: created,
            exit: if exited {
                Some(ExitStatus { code: Some(0), signal: None, unix_ms: created + 1 })
            } else {
                None
            },
            spec: spec(),
        }
    }

    fn many(tokens: &[&str], latest: bool, all: bool) -> SessionTargets {
        SessionTargets {
            sessions: tokens.iter().map(|s| (*s).to_string()).collect(),
            latest,
            all,
        }
    }

    #[test]
    fn one_latest_picks_max_created_at() {
        let xs = vec![s("a", Some("old"), 10, false), s("b", Some("new"), 20, false)];
        let t = SessionTarget { session: None, latest: true };
        let got = resolve_one_in(&xs, &t).unwrap();
        assert_eq!(got.id, "b");
    }

    #[test]
    fn one_literal_matches_name_then_id() {
        let xs = vec![s("a", Some("bash"), 1, false), s("b", Some("zsh"), 2, false)];
        let t = SessionTarget { session: Some("bash".into()), latest: false };
        assert_eq!(resolve_one_in(&xs, &t).unwrap().id, "a");
        let t = SessionTarget { session: Some("b".into()), latest: false };
        assert_eq!(resolve_one_in(&xs, &t).unwrap().id, "b");
    }

    #[test]
    fn one_unmatched_literal_errors_with_token() {
        let xs = vec![s("a", Some("bash"), 1, false)];
        let t = SessionTarget { session: Some("zsh".into()), latest: false };
        let err = resolve_one_in(&xs, &t).unwrap_err().to_string();
        assert!(err.contains("zsh"), "got {err}");
    }

    #[test]
    fn many_all_excludes_exited() {
        let xs = vec![s("a", Some("live"), 1, false), s("b", Some("dead"), 2, true)];
        let r = resolve_many_in(&xs, &many(&[], false, true));
        let ids: Vec<&str> = r.matched.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["a"]);
        assert!(r.unresolved.is_empty());
    }

    #[test]
    fn many_glob_matches_names_only() {
        let xs = vec![
            s("a", Some("bash-3f"), 1, false),
            s("b", Some("bash-7c"), 2, false),
            s("c", Some("zsh-99"), 3, false),
            s("d", None, 4, false), // no name -> not matched by a glob
        ];
        let r = resolve_many_in(&xs, &many(&["bash-*"], false, false));
        let ids: Vec<&str> = r.matched.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn many_dedups_literal_and_overlapping_glob() {
        let xs = vec![s("a", Some("bash-3f"), 1, false), s("b", Some("bash-7c"), 2, false)];
        let r = resolve_many_in(&xs, &many(&["bash-3f", "bash-*"], false, false));
        let ids: Vec<&str> = r.matched.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b"]); // a appears once, even though both expressions match it
    }

    #[test]
    fn many_unresolved_tokens_collected_not_fatal() {
        let xs = vec![s("a", Some("bash-3f"), 1, false)];
        let r = resolve_many_in(&xs, &many(&["bash-3f", "ghost", "z-*"], false, false));
        assert_eq!(r.matched.len(), 1);
        assert_eq!(r.unresolved, vec!["ghost", "z-*"]);
    }

    #[test]
    fn many_latest_returns_max_only() {
        let xs = vec![s("a", Some("old"), 1, false), s("b", Some("new"), 2, false)];
        let r = resolve_many_in(&xs, &many(&[], true, false));
        let ids: Vec<&str> = r.matched.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["b"]);
    }

    #[test]
    fn one_latest_on_empty_list_errors() {
        let xs: Vec<SessionInfo> = vec![];
        let t = SessionTarget { session: None, latest: true };
        let err = resolve_one_in(&xs, &t).unwrap_err().to_string();
        assert!(err.contains("no sessions to operate on"), "got {err}");
    }

    #[test]
    fn literal_name_takes_precedence_over_id_collision() {
        let xs = vec![
            // Session A: id "other-id", name "target-id" (name happens to look like another session's id).
            s("other-id", Some("target-id"), 1, false),
            // Session B: id "target-id", name "different".
            s("target-id", Some("different"), 2, false),
        ];
        let t = SessionTarget { session: Some("target-id".into()), latest: false };
        // Should match A by name, not B by id.
        assert_eq!(resolve_one_in(&xs, &t).unwrap().id, "other-id");
    }
}
