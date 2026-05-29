//! `cairn inspect`: render all known metadata for a single session as an
//! aligned key/value block.

use anyhow::Result;
use cairn_protocol::cairn::daemon::types::SessionInfo;
use cairn_protocol::client::cairn::daemon::sessions;

use crate::cli::SessionTarget;
use crate::connect::Endpoint;
use crate::targets;

pub async fn run(endpoint: &Endpoint, target: &SessionTarget) -> Result<i32> {
    let resolved = match targets::resolve_one(endpoint, target).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return Ok(1);
        }
    };
    let client = endpoint.client().await?;
    let info = match sessions::inspect(&client, (), &resolved.id).await {
        Ok(Ok(info)) => info,
        Ok(Err(e)) => {
            eprintln!("error: {}: {}", e.code, e.message);
            return Ok(1);
        }
        Err(e) => {
            eprintln!(
                "error: cannot reach cairn-daemon at {}: {e}",
                endpoint.label()
            );
            return Ok(1);
        }
    };
    print_block(&info);
    Ok(0)
}

fn print_block(s: &SessionInfo) {
    let rows: Vec<(&str, String)> = vec![
        ("id", s.id.clone()),
        ("name", s.name.clone().unwrap_or_else(|| "(unnamed)".into())),
        (
            "pid",
            s.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
        ),
        ("state", state_of(s)),
        ("size", format!("{}x{}", s.cols, s.rows)),
        ("created_at", rfc3339_or_raw(s.created_at_unix_ms)),
        ("command", shell_quote_join(&s.spec.command)),
        (
            "workdir",
            s.spec
                .workdir
                .clone()
                .unwrap_or_else(|| "(daemon default)".into()),
        ),
        ("tty", s.spec.tty.to_string()),
        ("stdin", s.spec.stdin.to_string()),
        ("env_inherit", s.spec.env_inherit.to_string()),
        (
            "idle_timeout",
            s.spec
                .idle_timeout_secs
                .map(|t| format!("{t}s"))
                .unwrap_or_else(|| "none".into()),
        ),
        ("scrollback_lines", s.spec.scrollback_lines.to_string()),
        ("attached_clients", attached_str(&s.attached_clients)),
    ];
    let key_width = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    for (k, v) in rows {
        println!("{:<width$}  {}", k, v, width = key_width);
    }
}

fn state_of(s: &SessionInfo) -> String {
    match &s.exit {
        None => "running".into(),
        Some(e) => {
            let when = rfc3339_or_raw(e.unix_ms);
            match (e.code, e.signal) {
                (Some(c), _) => format!("exited code={c} at {when}"),
                (_, Some(sig)) => format!("exited signal={sig} at {when}"),
                _ => format!("exited at {when}"),
            }
        }
    }
}

fn rfc3339_or_raw(unix_ms: u64) -> String {
    let secs = (unix_ms / 1000) as i64;
    let nanos = ((unix_ms % 1000) * 1_000_000) as u32;
    let Ok(odt) = time::OffsetDateTime::from_unix_timestamp(secs) else {
        return unix_ms.to_string();
    };
    let odt = odt.replace_nanosecond(nanos).unwrap_or(odt);
    odt.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| unix_ms.to_string())
}

fn shell_quote_join(words: &[String]) -> String {
    if words.is_empty() {
        return "(daemon default)".into();
    }
    words
        .iter()
        .map(|w| {
            if needs_quoting(w) {
                format!("'{}'", w.replace('\'', r"'\''"))
            } else {
                w.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn needs_quoting(w: &str) -> bool {
    w.is_empty()
        || w.chars().any(|c| {
            !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '=' | ':' | ','))
        })
}

fn attached_str(ids: &[String]) -> String {
    if ids.is_empty() {
        "0".into()
    } else {
        let mut sorted = ids.to_vec();
        sorted.sort();
        format!("{} ({})", sorted.len(), sorted.join(", "))
    }
}
