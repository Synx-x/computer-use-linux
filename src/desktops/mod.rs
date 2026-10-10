//! `computer-use-linux desktops serve`: the agent desktops daemon.
//!
//! The daemon owns a set of headless sway desktops. Each desktop has a
//! wayvnc stream and, once assigned, a Claude Code agent in a herdr pane.
//! It serves a JSON API, server-sent status events, WebP thumbnails, a
//! proxied VNC WebSocket and the PWA, all on one port bound to 127.0.0.1.

pub mod agent;
pub mod desktop;
pub mod rfb;
mod web;

use anyhow::{anyhow, bail, Context, Result};
use axum::extract::ws::{Message as ClientMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{header, StatusCode, Uri};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use desktop::{Desktop, LogEntry, Status};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::{broadcast, Mutex};

pub const DEFAULT_PORT: u16 = 7420;
pub const DEFAULT_MAX_DESKTOPS: usize = 4;
const POLL_INTERVAL: Duration = Duration::from_millis(1500);
const THUMB_WIDTH: u32 = 480;
/// The wall counts as watched for this long after a thumbnail request.
const WALL_WATCH_WINDOW: Duration = Duration::from_secs(5);
const THUMB_FRESH: Duration = Duration::from_millis(1400);
/// How long open connections get to close after the desktops stop.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

struct App {
    desktops: Mutex<BTreeMap<String, Arc<Desktop>>>,
    /// Serializes create, so two requests never pick the same VNC port.
    create_lock: Mutex<()>,
    next_id: AtomicU32,
    max: usize,
    events: broadcast::Sender<String>,
    sse_clients: AtomicUsize,
    last_thumb_request: std::sync::Mutex<Option<Instant>>,
    web: web::Web,
    root: PathBuf,
}

type Shared = Arc<App>;

struct ServeOptions {
    port: u16,
    max: usize,
    web_dir: Option<PathBuf>,
}

fn parse_serve(args: &[String]) -> Result<ServeOptions> {
    let mut options = ServeOptions {
        port: DEFAULT_PORT,
        max: DEFAULT_MAX_DESKTOPS,
        web_dir: None,
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .cloned()
                .ok_or_else(|| anyhow!("{arg} needs a value"))
        };
        match arg.as_str() {
            "--port" => options.port = value()?.parse().context("bad --port")?,
            "--max" => options.max = value()?.parse().context("bad --max")?,
            "--web-dir" => options.web_dir = Some(PathBuf::from(value()?)),
            other => bail!("unknown option {other} for desktops serve"),
        }
    }
    if options.max == 0 {
        bail!("--max must be at least 1");
    }
    Ok(options)
}

pub async fn run(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("serve") => serve(parse_serve(&args[1..])?).await,
        Some("install-unit") => install_unit(&args[1..]),
        _ => {
            println!(
                "Usage:\n  computer-use-linux desktops serve [--port {DEFAULT_PORT}] \
                 [--max {DEFAULT_MAX_DESKTOPS}] [--web-dir DIR]\n  \
                 computer-use-linux desktops install-unit [serve options]"
            );
            Ok(())
        }
    }
}

pub const UNIT_NAME: &str = "agent-desktops.service";

/// The systemd user unit that runs `desktops serve` with `serve_args`.
pub fn unit_text(exe: &std::path::Path, serve_args: &[String]) -> String {
    let mut exec = format!("{} desktops serve", exe.display());
    for arg in serve_args {
        exec.push(' ');
        exec.push_str(arg);
    }
    format!(
        "[Unit]\n\
         Description=Agent desktops daemon (computer-use-linux)\n\
         After=graphical-session.target\n\
         PartOf=graphical-session.target\n\
         \n\
         [Service]\n\
         ExecStart={exec}\n\
         # herdr, sway, wayvnc and grim must be on PATH.\n\
         Environment=PATH=%h/.local/bin:/usr/local/bin:/usr/bin:/bin\n\
         # SIGTERM reaches the daemon first, so it closes agents and desktops.\n\
         KillMode=mixed\n\
         TimeoutStopSec=20\n\
         Restart=on-failure\n\
         RestartSec=2\n\
         \n\
         [Install]\n\
         WantedBy=graphical-session.target\n"
    )
}

/// Write the user unit. It does not enable or start it.
fn install_unit(serve_args: &[String]) -> Result<()> {
    parse_serve(serve_args)?;
    let exe = std::env::current_exe().context("find this executable")?;
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| anyhow!("neither XDG_CONFIG_HOME nor HOME is set"))?;
    let dir = config.join("systemd/user");
    std::fs::create_dir_all(&dir).context("create the systemd user unit directory")?;
    let path = dir.join(UNIT_NAME);
    std::fs::write(&path, unit_text(&exe, serve_args)).context("write the unit")?;
    println!("Wrote {}", path.display());
    println!("Start it with: systemctl --user daemon-reload && systemctl --user enable --now {UNIT_NAME}");
    Ok(())
}

async fn serve(options: ServeOptions) -> Result<()> {
    // Bind first. A second daemon fails here and leaves the first one's
    // desktops alone.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", options.port))
        .await
        .with_context(|| format!("bind 127.0.0.1:{}", options.port))?;
    let root = desktop::runtime_root(options.port);
    for orphan in desktop::clean_orphans(&root) {
        eprintln!("desktops: killed orphan {orphan}");
    }
    let (events, _) = broadcast::channel(32);
    let app = Arc::new(App {
        desktops: Mutex::new(BTreeMap::new()),
        create_lock: Mutex::new(()),
        next_id: AtomicU32::new(1),
        max: options.max,
        events,
        sse_clients: AtomicUsize::new(0),
        last_thumb_request: std::sync::Mutex::new(None),
        web: web::Web::pick(options.web_dir),
        root,
    });
    eprintln!(
        "desktops: listening on http://127.0.0.1:{} with {}, at most {} desktops",
        options.port,
        app.web.describe(),
        app.max
    );
    let poller = tokio::spawn(poll_loop(Arc::clone(&app)));
    let router = router(Arc::clone(&app));
    let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
    let mut server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = stop_rx.changed().await;
            })
            .await
    });
    tokio::select! {
        result = &mut server => {
            poller.abort();
            shut_down(&app).await;
            return result
                .context("desktops API task failed")?
                .context("serve the desktops API");
        }
        _ = shutdown_signal() => {}
    }
    // Stop the desktops first. Graceful shutdown waits for open event
    // streams and VNC sockets, and an open PWA tab never closes its own.
    poller.abort();
    let _ = stop_tx.send(true);
    shut_down(&app).await;
    let _ = tokio::time::timeout(SHUTDOWN_GRACE, server).await;
    Ok(())
}

async fn shutdown_signal() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("install SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

/// Stop every desktop and close every agent.
async fn shut_down(app: &App) {
    let desktops: Vec<Arc<Desktop>> = app.desktops.lock().await.values().cloned().collect();
    for desktop in desktops {
        stop_desktop(desktop).await;
    }
    app.desktops.lock().await.clear();
    // Leaves the root in place if anything is still inside it.
    let _ = std::fs::remove_dir(&app.root);
}

async fn stop_desktop(desktop: Arc<Desktop>) {
    let agent = desktop.agent.lock().ok().and_then(|mut slot| slot.take());
    if let Some(agent) = agent {
        agent::close(&agent).await;
    }
    let _ = tokio::task::spawn_blocking(move || desktop.stop()).await;
}

fn router(app: Shared) -> Router {
    Router::new()
        .route("/api/desktops", get(list).post(create))
        .route(
            "/api/desktops/{id}",
            axum::routing::delete(remove).get(get_one),
        )
        .route("/api/desktops/{id}/agent", post(assign))
        .route("/api/desktops/{id}/takeover", post(takeover))
        .route("/api/desktops/{id}/handback", post(handback))
        .route("/api/desktops/{id}/thumb.webp", get(thumb))
        .route("/api/desktops/{id}/vnc", get(vnc))
        .route("/api/events", get(events))
        .fallback(static_files)
        .with_state(app)
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({"error": message.into()}))).into_response()
}

// Handlers return the error response as-is, so it is not worth boxing.
#[allow(clippy::result_large_err)]
async fn lookup(app: &App, id: &str) -> Result<Arc<Desktop>, Response> {
    app.desktops
        .lock()
        .await
        .get(id)
        .cloned()
        .ok_or_else(|| error(StatusCode::NOT_FOUND, format!("no desktop {id}")))
}

/// Local wall-clock time as HH:MM:SS.
fn clock(time: SystemTime) -> String {
    let seconds = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as libc::time_t)
        .unwrap_or(0);
    // SAFETY: localtime_r writes only into the tm we pass it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::localtime_r(&seconds, &mut tm);
    }
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

fn ago(time: SystemTime) -> String {
    let seconds = SystemTime::now()
        .duration_since(time)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m", seconds / 60),
        _ => format!("{}h", seconds / 3600),
    }
}

fn log_json(entry: &LogEntry) -> Value {
    json!({"t": clock(entry.time), "verb": entry.verb, "detail": entry.detail})
}

/// One desktop in the shape the PWA's `normalize()` reads.
fn desktop_json(desktop: &Desktop) -> Value {
    let snapshot = desktop.snapshot.lock().map(|s| s.clone()).ok();
    let agent = desktop.agent.lock().ok().and_then(|slot| slot.clone());
    let status = match (&agent, &snapshot) {
        (Some(_), Some(snapshot)) => snapshot.status,
        _ => Status::Idle,
    };
    let log = snapshot.as_ref().map(|s| s.log.clone()).unwrap_or_default();
    let last = log
        .first()
        .map(|entry| {
            format!("{} {}", entry.verb, entry.detail)
                .trim()
                .to_string()
        })
        .unwrap_or_default();
    let id = &desktop.id;
    json!({
        "id": id,
        "name": desktop.name,
        "display": desktop.display,
        "compositor": "sway headless",
        "res": format!("{}×{}", desktop.width, desktop.height),
        "resolution": format!("{}x{}", desktop.width, desktop.height),
        "width": desktop.width,
        "height": desktop.height,
        "agent": if agent.is_some() { format!("Claude · {}", desktop.name) } else { "Unassigned".to_string() },
        "task": agent.as_ref().map(|agent| agent.prompt.clone()).unwrap_or_else(|| "Ready for the next agent".to_string()),
        "status": status.word(),
        "last": last,
        "ago": log.first().map(|entry| ago(entry.time)).unwrap_or_default(),
        "windowTitle": snapshot.as_ref().map(|s| s.window_title.clone()).unwrap_or_default(),
        "lines": [],
        "log": log.iter().map(log_json).collect::<Vec<_>>(),
        "control": if desktop.user_has_control() { "user" } else { "agent" },
        "pane": agent.as_ref().map(|agent| agent.pane_id.clone()),
        "vnc": format!("/api/desktops/{id}/vnc"),
        "thumb": format!("/api/desktops/{id}/thumb.webp"),
        "vnc_port": desktop.vnc_port,
        "created": clock(desktop.created),
    })
}

async fn snapshot_json(app: &App) -> Value {
    let desktops: Vec<Arc<Desktop>> = app.desktops.lock().await.values().cloned().collect();
    Value::Array(
        desktops
            .iter()
            .map(|desktop| desktop_json(desktop))
            .collect(),
    )
}

async fn announce(app: &App) {
    let payload = json!({"type": "desktops", "desktops": snapshot_json(app).await});
    let _ = app.events.send(payload.to_string());
}

async fn list(State(app): State<Shared>) -> Json<Value> {
    Json(snapshot_json(&app).await)
}

async fn get_one(State(app): State<Shared>, Path(id): Path<String>) -> Response {
    match lookup(&app, &id).await {
        Ok(desktop) => Json(desktop_json(&desktop)).into_response(),
        Err(response) => response,
    }
}

#[derive(Deserialize, Default)]
struct CreateBody {
    name: Option<String>,
    resolution: Option<String>,
}

async fn create(State(app): State<Shared>, body: Option<Json<CreateBody>>) -> Response {
    let body = body.map(|Json(body)| body).unwrap_or_default();
    let (width, height) =
        match desktop::parse_resolution(body.resolution.as_deref().unwrap_or("1280x800")) {
            Ok(size) => size,
            Err(error) => return self::error(StatusCode::BAD_REQUEST, format!("{error:#}")),
        };
    let _creating = app.create_lock.lock().await;
    let (taken, names): (Vec<u16>, Vec<String>) = {
        let desktops = app.desktops.lock().await;
        if desktops.len() >= app.max {
            return self::error(
                StatusCode::CONFLICT,
                format!("the limit is {} desktops. Stop one first", app.max),
            );
        }
        (
            desktops.values().map(|desktop| desktop.vnc_port).collect(),
            desktops
                .values()
                .map(|desktop| desktop.name.clone())
                .collect(),
        )
    };
    let number = app.next_id.fetch_add(1, Ordering::SeqCst);
    let id = format!("d{number}");
    let mut name = desktop::sanitize_name(body.name.as_deref().unwrap_or_default());
    if name.is_empty() {
        name = format!("desktop-{number}");
    }
    if names.contains(&name) {
        name = format!("{name}-{number}");
    }
    let port = match desktop::pick_vnc_port(&taken) {
        Ok(port) => port,
        Err(error) => return self::error(StatusCode::SERVICE_UNAVAILABLE, format!("{error:#}")),
    };
    match Desktop::start(&app.root, id.clone(), name, width, height, port).await {
        Ok(desktop) => {
            let desktop = Arc::new(desktop);
            app.desktops.lock().await.insert(id, Arc::clone(&desktop));
            announce(&app).await;
            (StatusCode::CREATED, Json(desktop_json(&desktop))).into_response()
        }
        Err(error) => self::error(StatusCode::INTERNAL_SERVER_ERROR, format!("{error:#}")),
    }
}

async fn remove(State(app): State<Shared>, Path(id): Path<String>) -> Response {
    let Some(desktop) = app.desktops.lock().await.remove(&id) else {
        return error(StatusCode::NOT_FOUND, format!("no desktop {id}"));
    };
    stop_desktop(desktop).await;
    announce(&app).await;
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
struct AssignBody {
    prompt: String,
}

async fn assign(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(body): Json<AssignBody>,
) -> Response {
    let desktop = match lookup(&app, &id).await {
        Ok(desktop) => desktop,
        Err(response) => return response,
    };
    if body.prompt.trim().is_empty() {
        return error(StatusCode::BAD_REQUEST, "prompt is empty");
    }
    if desktop
        .agent
        .lock()
        .map(|slot| slot.is_some())
        .unwrap_or(true)
    {
        return error(
            StatusCode::CONFLICT,
            format!("desktop {id} already has an agent"),
        );
    }
    match agent::assign(&desktop, body.prompt.trim()).await {
        Ok(started) => {
            if let Ok(mut slot) = desktop.agent.lock() {
                *slot = Some(started);
            }
            if let Ok(mut snapshot) = desktop.snapshot.lock() {
                snapshot.status = Status::Running;
                snapshot.log.insert(
                    0,
                    LogEntry {
                        time: SystemTime::now(),
                        verb: "launch".to_string(),
                        detail: "claude in herdr".to_string(),
                    },
                );
            }
            announce(&app).await;
            Json(desktop_json(&desktop)).into_response()
        }
        Err(error) => self::error(StatusCode::BAD_GATEWAY, format!("{error:#}")),
    }
}

async fn takeover(State(app): State<Shared>, Path(id): Path<String>) -> Response {
    let desktop = match lookup(&app, &id).await {
        Ok(desktop) => desktop,
        Err(response) => return response,
    };
    let agent = desktop.agent.lock().ok().and_then(|slot| slot.clone());
    if let Some(agent) = &agent {
        if let Err(error) = agent::interrupt(agent).await {
            return self::error(StatusCode::BAD_GATEWAY, format!("{error:#}"));
        }
    }
    desktop.user_control.store(true, Ordering::SeqCst);
    announce(&app).await;
    Json(desktop_json(&desktop)).into_response()
}

async fn handback(State(app): State<Shared>, Path(id): Path<String>) -> Response {
    let desktop = match lookup(&app, &id).await {
        Ok(desktop) => desktop,
        Err(response) => return response,
    };
    // Input stops before the agent hears it may continue.
    desktop.user_control.store(false, Ordering::SeqCst);
    let agent = desktop.agent.lock().ok().and_then(|slot| slot.clone());
    if let Some(agent) = &agent {
        if let Err(error) = agent::resume(agent).await {
            return self::error(StatusCode::BAD_GATEWAY, format!("{error:#}"));
        }
    }
    announce(&app).await;
    Json(desktop_json(&desktop)).into_response()
}

fn mark_wall_watched(app: &App) {
    if let Ok(mut last) = app.last_thumb_request.lock() {
        *last = Some(Instant::now());
    }
}

fn wall_watched(app: &App) -> bool {
    app.sse_clients.load(Ordering::SeqCst) > 0
        || app
            .last_thumb_request
            .lock()
            .ok()
            .and_then(|last| *last)
            .is_some_and(|last| last.elapsed() < WALL_WATCH_WINDOW)
}

async fn refresh_thumb(desktop: &Desktop) -> Result<Vec<u8>> {
    let bytes = desktop.capture_webp(THUMB_WIDTH).await?;
    if let Ok(mut slot) = desktop.thumb.lock() {
        *slot = Some((Instant::now(), bytes.clone()));
    }
    Ok(bytes)
}

async fn thumb(State(app): State<Shared>, Path(id): Path<String>) -> Response {
    let desktop = match lookup(&app, &id).await {
        Ok(desktop) => desktop,
        Err(response) => return response,
    };
    mark_wall_watched(&app);
    let cached = desktop.thumb.lock().ok().and_then(|slot| {
        slot.as_ref()
            .filter(|(taken, _)| taken.elapsed() < THUMB_FRESH)
            .map(|(_, bytes)| bytes.clone())
    });
    let bytes = match cached {
        Some(bytes) => bytes,
        None => match refresh_thumb(&desktop).await {
            Ok(bytes) => bytes,
            Err(error) => return self::error(StatusCode::BAD_GATEWAY, format!("{error:#}")),
        },
    };
    (
        [
            (header::CONTENT_TYPE, "image/webp"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        bytes,
    )
        .into_response()
}

/// Counts open event streams, so thumbnails refresh only while someone
/// watches.
struct ClientGuard(Shared);

impl Drop for ClientGuard {
    fn drop(&mut self) {
        self.0.sse_clients.fetch_sub(1, Ordering::SeqCst);
    }
}

async fn events(
    State(app): State<Shared>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    app.sse_clients.fetch_add(1, Ordering::SeqCst);
    let guard = ClientGuard(Arc::clone(&app));
    let first = json!({"type": "desktops", "desktops": snapshot_json(&app).await}).to_string();
    let receiver = app.events.subscribe();
    let stream = futures_util::stream::unfold(
        (Some(first), receiver, guard),
        |(first, mut receiver, guard)| async move {
            if let Some(first) = first {
                return Some((Ok(Event::default().data(first)), (None, receiver, guard)));
            }
            loop {
                match receiver.recv().await {
                    Ok(payload) => {
                        return Some((Ok(Event::default().data(payload)), (None, receiver, guard)))
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        },
    );
    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn vnc(
    State(app): State<Shared>,
    Path(id): Path<String>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let desktop = match lookup(&app, &id).await {
        Ok(desktop) => desktop,
        Err(response) => return response,
    };
    upgrade
        .protocols(["binary"])
        .on_upgrade(move |socket| async move {
            if let Err(error) = proxy_vnc(socket, desktop).await {
                eprintln!("desktops: VNC proxy closed: {error:#}");
            }
        })
}

/// Relay one viewer to wayvnc. The viewer speaks RFB inside WebSocket
/// binary frames and wayvnc speaks plain RFB over TCP. Viewer bytes pass the
/// RFB filter, which drops input unless the user has taken over.
async fn proxy_vnc(client: WebSocket, desktop: Arc<Desktop>) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let upstream = tokio::net::TcpStream::connect(("127.0.0.1", desktop.vnc_port))
        .await
        .context("connect to wayvnc")?;
    upstream.set_nodelay(true).ok();
    let (mut up_rx, mut up_tx) = upstream.into_split();
    let (mut client_tx, mut client_rx) = client.split();
    let viewer = Arc::clone(&desktop);
    let to_server = async move {
        let mut filter = rfb::ClientFilter::default();
        while let Some(message) = client_rx.next().await {
            match message.context("read from viewer")? {
                ClientMessage::Binary(bytes) => {
                    let forward = filter.feed(&bytes, viewer.user_has_control())?;
                    if !forward.is_empty() {
                        up_tx.write_all(&forward).await.context("write to wayvnc")?;
                    }
                }
                ClientMessage::Close(_) => break,
                _ => {}
            }
        }
        let _ = up_tx.shutdown().await;
        Ok::<(), anyhow::Error>(())
    };
    let to_viewer = async move {
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read = up_rx.read(&mut buffer).await.context("read from wayvnc")?;
            if read == 0 {
                break;
            }
            client_tx
                .send(ClientMessage::Binary(buffer[..read].to_vec().into()))
                .await
                .context("write to viewer")?;
        }
        let _ = client_tx.close().await;
        Ok::<(), anyhow::Error>(())
    };
    tokio::select! {
        result = to_server => result,
        result = to_viewer => result,
    }
}

async fn static_files(State(app): State<Shared>, uri: Uri) -> Response {
    if uri.path().starts_with("/api/") {
        return error(StatusCode::NOT_FOUND, format!("no route {}", uri.path()));
    }
    app.web.serve(uri).await
}

/// Refresh every desktop's status, log and window title, refresh the
/// thumbnails while the wall is watched, and announce any change.
async fn poll_loop(app: Shared) {
    let mut last_payload = String::new();
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        let desktops: Vec<Arc<Desktop>> = app.desktops.lock().await.values().cloned().collect();
        let watched = wall_watched(&app);
        for desktop in &desktops {
            let agent = desktop.agent.lock().ok().and_then(|slot| slot.clone());
            let window_title = desktop.focused_title().await;
            let (status, log) = match &agent {
                Some(agent) => {
                    let status = agent::status(agent).await;
                    let reader = agent.clone();
                    let log = tokio::task::spawn_blocking(move || agent::action_log(&reader))
                        .await
                        .unwrap_or_default();
                    (status, Some(log))
                }
                None => (Status::Idle, None),
            };
            if let Ok(mut snapshot) = desktop.snapshot.lock() {
                snapshot.status = status;
                snapshot.window_title = window_title;
                if let Some(log) = log.filter(|log| !log.is_empty()) {
                    snapshot.log = log;
                }
            }
            if watched {
                let _ = refresh_thumb(desktop).await;
            }
        }
        let payload = snapshot_json(&app).await;
        // "ago" ticks every second. Compare without it so a quiet desktop
        // does not announce itself on every poll.
        let mut comparable = payload.clone();
        if let Some(rows) = comparable.as_array_mut() {
            for row in rows {
                row["ago"] = Value::Null;
            }
        }
        let comparable = comparable.to_string();
        if comparable != last_payload {
            last_payload = comparable;
            let _ = app
                .events
                .send(json!({"type": "desktops", "desktops": payload}).to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serve_options_parse_and_default() {
        let options = parse_serve(&[]).unwrap();
        assert_eq!(options.port, DEFAULT_PORT);
        assert_eq!(options.max, DEFAULT_MAX_DESKTOPS);
        let args: Vec<String> = ["--port", "7999", "--max", "2"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let options = parse_serve(&args).unwrap();
        assert_eq!((options.port, options.max), (7999, 2));
        assert!(parse_serve(&["--max".to_string(), "0".to_string()]).is_err());
        assert!(parse_serve(&["--bogus".to_string()]).is_err());
    }

    #[test]
    fn the_unit_runs_serve_with_the_given_options() {
        let text = unit_text(
            std::path::Path::new("/opt/cul"),
            &["--max".to_string(), "2".to_string()],
        );
        assert!(text.contains("ExecStart=/opt/cul desktops serve --max 2\n"));
        assert!(text.contains("KillMode=mixed"));
        assert!(text.contains("WantedBy=graphical-session.target"));
    }

    #[test]
    fn ages_read_in_the_largest_whole_unit() {
        let now = SystemTime::now();
        assert_eq!(ago(now - Duration::from_secs(4)), "4s");
        assert_eq!(ago(now - Duration::from_secs(130)), "2m");
        assert_eq!(ago(now - Duration::from_secs(7300)), "2h");
    }
}
