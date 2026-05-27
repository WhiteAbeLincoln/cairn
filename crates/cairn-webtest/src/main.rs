//! Throwaway web UI for poking at the cairn daemon over its Unix socket.
//!
//! Run the daemon (`cargo run -p cairn-daemon`), then this
//! (`cargo run -p cairn-webtest`) and open the printed URL from your phone on
//! the same network. Not a product — a disposable test harness. The pty I/O
//! views deliberately show escaped bytes rather than rendering a terminal.

use std::net::SocketAddr;
use std::path::PathBuf;

use axum::Router;
use axum::extract::{Form, Path, State};
use axum::response::{Html, IntoResponse, Redirect};
use axum::routing::{get, post};
use bytes::Bytes;
use futures::StreamExt as _;
use serde::Deserialize;

use cairn_protocol::cairn::daemon::types as t;
use cairn_protocol::client::cairn::daemon as api;

#[derive(Clone)]
struct AppState {
    socket: PathBuf,
}

type Client = wrpc_transport::unix::Client<PathBuf>;

fn default_socket() -> PathBuf {
    if let Ok(s) = std::env::var("CAIRN_SOCKET") {
        return PathBuf::from(s);
    }
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("TMPDIR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("cairn").join("cairn.sock")
}

/// A fresh client per invocation — UDS opens one connection per wRPC call.
fn wc(st: &AppState) -> Client {
    wrpc_transport::unix::Client::from(st.socket.clone())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let st = AppState {
        socket: default_socket(),
    };
    let app = Router::new()
        .route("/", get(home))
        .route("/create", post(create))
        .route("/s/:id", get(session))
        .route("/s/:id/state", get(state))
        .route("/s/:id/stream", get(stream))
        .route("/s/:id/send", post(send))
        .route("/s/:id/kill", post(kill))
        .route("/s/:id/rename", post(rename))
        .route("/s/:id/restart", post(restart))
        .route("/s/:id/kick", post(kick))
        .with_state(st.clone());

    let addr: SocketAddr = std::env::var("WEBTEST_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8088".to_string())
        .parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!(
        "cairn-webtest listening on http://{addr}  (daemon socket: {})",
        st.socket.display()
    );
    axum::serve(listener, app).await?;
    Ok(())
}

// ── HTML helpers ───────────────────────────────────────────────────────────

fn h(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// escape_default form (`\x1b`, `\n`, …). Single-line printable ASCII, so it is
/// safe to drop straight into an SSE `data:` field (no raw newlines).
fn esc_raw(b: &[u8]) -> String {
    let mut s = String::new();
    for &c in b {
        for e in std::ascii::escape_default(c) {
            s.push(e as char);
        }
    }
    s
}

/// As [`esc_raw`] but HTML-escaped for embedding directly in a page.
fn esc_bytes(b: &[u8]) -> String {
    h(&esc_raw(b))
}

fn page(title: &str, body: &str) -> Html<String> {
    Html(format!(
        "<!doctype html><html><head><meta charset=utf-8>\
<meta name=viewport content='width=device-width,initial-scale=1'>\
<title>{}</title><style>\
body{{font-family:system-ui,sans-serif;margin:0;padding:1rem;line-height:1.4;max-width:48rem}}\
a{{color:#06c}} pre{{white-space:pre-wrap;word-break:break-all;background:#111;color:#0f0;padding:.5rem;border-radius:6px;overflow:auto}}\
input,textarea,button,select{{font:inherit;padding:.5rem;margin:.2rem 0;width:100%;box-sizing:border-box}}\
button{{width:auto;cursor:pointer}} table{{width:100%;border-collapse:collapse}}\
td,th{{text-align:left;padding:.3rem;border-bottom:1px solid #ddd;font-size:.9rem}}\
.card{{border:1px solid #ddd;border-radius:8px;padding:.75rem;margin:.5rem 0}}\
.row{{display:flex;gap:.5rem;flex-wrap:wrap}} .muted{{color:#666;font-size:.85rem}}\
</style></head><body>{}</body></html>",
        h(title),
        body
    ))
}

fn err_page(what: &str, e: impl std::fmt::Display) -> Html<String> {
    page(
        "error",
        &format!(
            "<p><a href=/>&larr; home</a></p><h2>{} failed</h2><pre>{}</pre>",
            h(what),
            esc_bytes(format!("{e}").as_bytes())
        ),
    )
}

// ── Forms ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct CreateForm {
    name: String,
    command: String,
}

#[derive(Deserialize)]
struct SendForm {
    input: String,
}

#[derive(Deserialize)]
struct RenameForm {
    new_name: String,
}

// ── Handlers ─────────────────────────────────────────────────────────────────

async fn home(State(st): State<AppState>) -> Html<String> {
    let version = match api::meta::version(&wc(&st), ()).await {
        Ok(v) => format!("{} · {}", h(&v.daemon), h(&v.protocol)),
        Err(e) => return err_page("version", e),
    };
    let who = match api::meta::whoami(&wc(&st), ()).await {
        Ok(Ok(s)) => h(&s),
        Ok(Err(e)) => format!("err {}", h(&e.code)),
        Err(e) => return err_page("whoami", e),
    };
    let sessions = match api::sessions::list_all(&wc(&st), ()).await {
        Ok(v) => v,
        Err(e) => return err_page("list", e),
    };

    let mut rows = String::new();
    for s in &sessions {
        let exit = match &s.exit {
            Some(e) => format!("exited code={:?} sig={:?}", e.code, e.signal),
            None => "running".to_string(),
        };
        rows.push_str(&format!(
            "<tr><td><a href='/s/{id}'>{name}</a><div class=muted>{id}</div></td>\
<td>{cols}×{rows_}</td><td>{att}</td><td>{exit}</td></tr>",
            id = h(&s.id),
            name = h(s.name.as_deref().unwrap_or("(unnamed)")),
            cols = s.cols,
            rows_ = s.rows,
            att = s.attached_clients.len(),
            exit = h(&exit),
        ));
    }

    let body = format!(
        "<h1>cairn-webtest</h1>\
<p class=muted>daemon: {version}<br>whoami: {who}</p>\
<div class=card><h3>create session</h3>\
<form method=post action=/create>\
<input name=name placeholder='name (optional)'>\
<input name=command placeholder=\"command, e.g. bash -i  or  sh -c 'while true; do date; sleep 1; done'  (blank = default shell)\">\
<button>create</button></form></div>\
<h3>sessions ({n})</h3>\
<table id=sess><tr><th>name</th><th>size</th><th>clients</th><th>state</th></tr>{rows}</table>\
<p class=muted>auto-refreshing</p>\
<script>setInterval(function(){{fetch('/').then(function(r){{return r.text();}}).then(function(t){{var d=new DOMParser().parseFromString(t,'text/html');var n=d.getElementById('sess');if(n)document.getElementById('sess').innerHTML=n.innerHTML;}}).catch(function(){{}});}},3000);</script>",
        n = sessions.len(),
    );
    page("cairn-webtest", &body)
}

async fn create(State(st): State<AppState>, Form(f): Form<CreateForm>) -> impl IntoResponse {
    let spec = t::SessionSpec {
        name: {
            let n = f.name.trim();
            if n.is_empty() { None } else { Some(n.to_string()) }
        },
        // Shell-tokenize so `sh -c 'while …; done'` keeps its quoted arg;
        // fall back to whitespace split on unbalanced quotes. Empty -> the
        // daemon uses its default shell.
        command: shlex::split(&f.command)
            .unwrap_or_else(|| f.command.split_whitespace().map(str::to_string).collect()),
        env: vec![],
        env_inherit: true,
        workdir: None,
        tty: true,
        stdin: true,
        idle_timeout_secs: None,
        scrollback_lines: 1000,
    };
    match api::sessions::create(&wc(&st), (), &spec).await {
        Ok(Ok(info)) => Redirect::to(&format!("/s/{}", info.id)).into_response(),
        Ok(Err(e)) => err_page("create", format!("{}: {}", e.code, e.message)).into_response(),
        Err(e) => err_page("create", e).into_response(),
    }
}

async fn session(State(st): State<AppState>, Path(id): Path<String>) -> Html<String> {
    let info = match api::sessions::inspect(&wc(&st), (), &id).await {
        Ok(Ok(i)) => i,
        Ok(Err(e)) => return err_page("inspect", format!("{}: {}", e.code, e.message)),
        Err(e) => return err_page("inspect", e),
    };

    let exit = state_str(&info);

    let body = format!(
        "<p><a href=/>&larr; home</a></p>\
<h2>{name}</h2><p class=muted>{id}<br>{cols}×{rows} · pid {pid:?} · {att} client(s) · <span id=state>{exit}</span><br>cmd: {cmd}</p>\
<div class=row>\
<form method=post action='/s/{id}/kill'><button>kill (TERM)</button></form>\
<form method=post action='/s/{id}/restart'><button>restart (force)</button></form>\
<form method=post action='/s/{id}/kick'><button>kick all</button></form>\
</div>\
<div class=card><form method=post action='/s/{id}/rename' class=row>\
<input name=new_name placeholder='new name'><button>rename</button></form></div>\
<div class=card><h3>send input</h3>\
<form method=post action='/s/{id}/send'>\
<textarea name=input rows=3 placeholder='bytes to inject (a newline is sent literally)'></textarea>\
<button>send</button></form>\
<p class=muted>tip: end with a newline to submit a shell command.</p></div>\
<h3>output (live · escaped bytes)</h3><pre id=out>(connecting…)</pre>\
<p class=muted><span id=st>streaming</span> — <a href='/s/{id}'>reload page</a></p>\
<script>\
const out=document.getElementById('out'),st=document.getElementById('st');out.textContent='';\
const es=new EventSource('/s/{id}/stream');\
es.onmessage=function(e){{out.textContent+=e.data;window.scrollTo(0,document.body.scrollHeight);}};\
es.addEventListener('eof',function(){{es.close();st.textContent='stream ended';}});\
es.onerror=function(){{st.textContent='reconnecting…';}};\
setInterval(function(){{fetch('/s/{id}/state').then(function(r){{return r.text();}}).then(function(t){{var el=document.getElementById('state');if(el)el.textContent=t;}}).catch(function(){{}});}},2000);\
</script>",
        name = h(info.name.as_deref().unwrap_or("(unnamed)")),
        id = h(&id),
        cols = info.cols,
        rows = info.rows,
        pid = info.pid,
        att = info.attached_clients.len(),
        exit = h(&exit),
        cmd = h(&info.spec.command.join(" ")),
    );
    page(&format!("session {}", info.name.as_deref().unwrap_or(&id)), &body)
}

fn state_str(i: &t::SessionInfo) -> String {
    match &i.exit {
        Some(e) => format!("exited code={:?} sig={:?} at={}", e.code, e.signal, e.unix_ms),
        None => "running".to_string(),
    }
}

/// Current state line for a session (`running` / `exited …`). Polled by the
/// detail page so the header reflects exit without a reload.
async fn state(State(st): State<AppState>, Path(id): Path<String>) -> String {
    match api::sessions::inspect(&wc(&st), (), &id).await {
        Ok(Ok(i)) => state_str(&i),
        Ok(Err(e)) => format!("err {}", e.code),
        Err(e) => format!("error: {e}"),
    }
}

/// SSE endpoint: drive `logs(follow=true)` and forward each chunk as an event.
/// `logs` emits the snapshot first, then live output until the session closes;
/// we send an `eof` event at the end so the browser stops reconnecting.
///
/// wRPC returns `(stream, Option<io_future>)`; the io future pumps the
/// transport and is driven concurrently for the connection's lifetime.
async fn stream(State(st): State<AppState>, Path(id): Path<String>) -> axum::response::Response {
    use axum::response::sse::{Event, KeepAlive, Sse};
    use std::convert::Infallible;

    match api::sessions::logs(&wc(&st), (), &id, &t::LogWindow::All, true).await {
        Ok((stream, io)) => {
            if let Some(io) = io {
                tokio::spawn(async move {
                    let _ = io.await;
                });
            }
            let body = stream
                .map(|batch| {
                    let mut s = String::new();
                    for chunk in &batch {
                        s.push_str(&esc_raw(chunk));
                    }
                    Ok::<Event, Infallible>(Event::default().data(s))
                })
                .chain(futures::stream::once(async {
                    Ok::<Event, Infallible>(Event::default().event("eof").data("end"))
                }));
            Sse::new(body).keep_alive(KeepAlive::default()).into_response()
        }
        Err(e) => {
            let body = futures::stream::iter(vec![
                Ok::<Event, Infallible>(Event::default().data(esc_raw(format!("(logs error: {e})").as_bytes()))),
                Ok(Event::default().event("eof").data("end")),
            ]);
            Sse::new(body).into_response()
        }
    }
}

async fn send(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Form(f): Form<SendForm>,
) -> impl IntoResponse {
    let chunk: Bytes = Bytes::from(f.input.into_bytes());
    let chunks: std::pin::Pin<Box<dyn futures::Stream<Item = Vec<Bytes>> + Send>> =
        Box::pin(futures::stream::iter(vec![vec![chunk]]));
    let _ = api::sessions::send(&wc(&st), (), &id, chunks).await;
    Redirect::to(&format!("/s/{id}"))
}

async fn kill(State(st): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    let sig = t::Signal::Named(t::SignalName::Term);
    let _ = api::sessions::kill(&wc(&st), (), &id, &sig, None).await;
    Redirect::to(&format!("/s/{id}"))
}

async fn restart(State(st): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    let _ = api::sessions::restart(&wc(&st), (), &id, true).await;
    Redirect::to(&format!("/s/{id}"))
}

async fn kick(State(st): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    let _ = api::sessions::kick(&wc(&st), (), &id, None).await;
    Redirect::to(&format!("/s/{id}"))
}

async fn rename(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Form(f): Form<RenameForm>,
) -> impl IntoResponse {
    let _ = api::sessions::rename(&wc(&st), (), &id, &f.new_name).await;
    Redirect::to(&format!("/s/{id}"))
}
