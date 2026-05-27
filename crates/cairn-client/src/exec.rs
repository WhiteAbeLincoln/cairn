//! `cairn exec` / `cairn run`: build a SessionSpec, create the session, then
//! (unless detached) attach to it.

use std::path::PathBuf;

use anyhow::{Context, Result};

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
        assert!(env.iter().any(|(k, v)| k == "CARGO_PKG_NAME" && v == "cairn"));
    }

    #[test]
    fn bare_key_absent_is_skipped() {
        let env = merge_env(&[], &["CAIRN_DEFINITELY_UNSET_VAR_XYZ".to_string()]).unwrap();
        assert!(env.iter().all(|(k, _)| k != "CAIRN_DEFINITELY_UNSET_VAR_XYZ"));
    }

    #[test]
    fn malformed_env_file_line_errors() {
        let f = write_tmp("NOEQUALS\n");
        assert!(merge_env(&[f.path().to_path_buf()], &[]).is_err());
    }
}
