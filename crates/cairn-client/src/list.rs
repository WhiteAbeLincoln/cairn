//! `cairn list` / `cairn ls`: plain-text table of sessions ordered by
//! `created_at_unix_ms`.

use anyhow::Result;
use cairn_protocol::cairn::daemon::types::SessionInfo;
use cairn_protocol::client::cairn::daemon::sessions;

use crate::connect::Endpoint;

pub async fn run(endpoint: &Endpoint) -> Result<i32> {
    let client = endpoint.client();
    let mut sessions = match sessions::list_all(&client, ()).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot reach cairn-daemon at {}: {e}", endpoint.label());
            return Ok(1);
        }
    };
    if sessions.is_empty() {
        println!("no sessions");
        return Ok(0);
    }
    sessions.sort_by_key(|s| s.created_at_unix_ms);
    let rows: Vec<[String; 5]> = sessions.iter().map(render_row).collect();
    print_table(&["NAME", "ID", "SIZE", "CLIENTS", "STATE"], &rows);
    Ok(0)
}

fn render_row(s: &SessionInfo) -> [String; 5] {
    let name = s.name.clone().unwrap_or_else(|| "(unnamed)".into());
    let short_id = s.id.chars().take(12).collect::<String>();
    let size = format!("{}x{}", s.cols, s.rows);
    let clients = s.attached_clients.len().to_string();
    let state = match &s.exit {
        None => "running".to_string(),
        Some(e) => match (e.code, e.signal) {
            (Some(c), _) => format!("exited code={c}"),
            (_, Some(sig)) => format!("exited signal={sig}"),
            _ => "exited".into(),
        },
    };
    [truncate(&name, 40), short_id, size, clients, state]
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn print_table(headers: &[&str], rows: &[[String; 5]]) {
    let widths: Vec<usize> = (0..headers.len())
        .map(|i| {
            let cell_max = rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0);
            cell_max.max(headers[i].chars().count())
        })
        .collect();
    let mut line = String::new();
    for (i, h) in headers.iter().enumerate() {
        if i > 0 {
            line.push_str("  ");
        }
        line.push_str(h);
        for _ in h.chars().count()..widths[i] {
            line.push(' ');
        }
    }
    println!("{line}");
    for row in rows {
        let mut line = String::new();
        for (i, cell) in row.iter().enumerate() {
            if i > 0 {
                line.push_str("  ");
            }
            line.push_str(cell);
            for _ in cell.chars().count()..widths[i] {
                line.push(' ');
            }
        }
        println!("{line}");
    }
}
