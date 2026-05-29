//! `cairn exec` / `cairn run`: build a SessionSpec, create the session, then
//! (unless detached) attach to it.

use std::path::PathBuf;

use anyhow::{Context, Result};
use cairn_protocol::cairn::daemon::types::SessionSpec;
use cairn_protocol::client::cairn::daemon::sessions;

use crate::attach::{self, AttachOptions};
use crate::cli::ExecArgs;
use crate::connect::{Client, Endpoint};
use crate::detach::DetachKeys;

/// Merge `--env-file` files (lowest precedence, applied in order) with `-e`
/// args (highest; `KEY=VALUE` sets, bare `KEY` copies from the client env).
/// Returns the explicit env list for the spec; the daemon overlays it on the
/// inherited env (explicit wins).
pub fn merge_env(env_files: &[PathBuf], env_args: &[String]) -> Result<Vec<(String, String)>> {
    let mut map: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for path in env_files {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("reading --env-file {}", path.display()))?;
        for (i, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (k, v) = line
                .split_once('=')
                .with_context(|| format!("{}:{}: expected KEY=VALUE", path.display(), i + 1))?;
            map.insert(k.trim().to_string(), v.to_string());
        }
    }
    for item in env_args {
        match item.split_once('=') {
            Some((k, v)) => {
                map.insert(k.to_string(), v.to_string());
            }
            None => {
                // bare KEY: pass through from the client env if it's set (docker parity).
                if let Some(v) = std::env::var_os(item) {
                    map.insert(item.clone(), v.to_string_lossy().into_owned());
                }
            }
        }
    }
    Ok(map.into_iter().collect())
}

/// Shared body for `exec` (default `-it` off) and `run` (default `-it` on).
pub async fn run_exec(
    args: &ExecArgs,
    default_tty: bool,
    default_interactive: bool,
    endpoint: &Endpoint,
    client: &Client,
) -> Result<i32> {
    let tty = args.tty_with_default(default_tty);
    let stdin = args.interactive_with_default(default_interactive);
    // Auto-populate workdir from the client's cwd only when the endpoint is
    // local — over UDS, client and daemon share a filesystem so the path is
    // meaningful. Over WebTransport the daemon is on a different machine and
    // the client's cwd almost never exists there (`session.spawn_failed`).
    let workdir = match (&args.workdir, endpoint) {
        (Some(w), _) => Some(w.to_string_lossy().into_owned()),
        (None, Endpoint::Unix(_)) => std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().into_owned()),
        (None, Endpoint::WebTransport { .. }) => None,
    };

    let spec = SessionSpec {
        name: args.name.clone(),
        command: args.command.clone(),
        env: merge_env(&args.env_file, &args.env)?,
        env_inherit: !args.no_inherit_env,
        workdir,
        tty,
        stdin,
        idle_timeout_secs: args.timeout.map(|d| d.as_secs()),
        scrollback_lines: 1000,
    };

    let info = match sessions::create(client, (), &spec)
        .await
        .context("create session")?
    {
        Ok(info) => info,
        Err(e) => {
            eprintln!("cairn: create failed: {}: {}", e.code, e.message);
            return Ok(1);
        }
    };

    let label = info.name.clone().unwrap_or_else(|| info.id.clone());
    if args.detach {
        println!("{label}");
        eprintln!("cairn: session created detached; attach with `cairn attach {label}`");
        return Ok(0);
    }

    let opts = AttachOptions {
        no_stdin: !stdin,
        detach_keys: DetachKeys::parse_or_default(args.detach_keys.as_deref())
            .map_err(|e| anyhow::anyhow!(e))?,
        pty: tty,
    };
    attach::run(endpoint, &info.id, opts).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_tmp(contents: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn parses_env_file_skipping_comments_and_blanks() {
        let f = write_tmp("# comment\n\nFOO=bar\nBAZ=qux\n");
        let env = merge_env(&[f.path().to_path_buf()], &[]).unwrap();
        assert!(env.contains(&("FOO".to_string(), "bar".to_string())));
        assert!(env.contains(&("BAZ".to_string(), "qux".to_string())));
    }

    #[test]
    fn dash_e_overrides_env_file() {
        let f = write_tmp("FOO=from_file\n");
        let env = merge_env(&[f.path().to_path_buf()], &["FOO=from_flag".to_string()]).unwrap();
        assert!(env.contains(&("FOO".to_string(), "from_flag".to_string())));
        assert!(!env.contains(&("FOO".to_string(), "from_file".to_string())));
    }

    #[test]
    fn bare_key_copies_from_client_env_when_set() {
        // CARGO_PKG_NAME is set during `cargo test` -> "cairn".
        let env = merge_env(&[], &["CARGO_PKG_NAME".to_string()]).unwrap();
        assert!(
            env.iter()
                .any(|(k, v)| k == "CARGO_PKG_NAME" && v == "cairn")
        );
    }

    #[test]
    fn bare_key_absent_is_skipped() {
        let env = merge_env(&[], &["CAIRN_DEFINITELY_UNSET_VAR_XYZ".to_string()]).unwrap();
        assert!(
            env.iter()
                .all(|(k, _)| k != "CAIRN_DEFINITELY_UNSET_VAR_XYZ")
        );
    }

    #[test]
    fn malformed_env_file_line_errors() {
        let f = write_tmp("NOEQUALS\n");
        assert!(merge_env(&[f.path().to_path_buf()], &[]).is_err());
    }
}
