//! Phase 2 check: the daemon creates a desktop, serves its status, a
//! thumbnail and a VNC stream, enforces view-only input, and leaves no sway
//! or wayvnc behind when a desktop stops or the daemon exits.
//!
//! The agent half needs a live herdr. Set CUL_TEST_HERDR=1 to run it with a
//! stand-in agent instead of Claude.

mod common;

use common::{clean_env, missing, read_file, wait_for};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/nested/mouse_report.py"
);

fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .expect("find a free port")
}

struct Daemon {
    child: Child,
    port: u16,
    web: PathBuf,
}

impl Daemon {
    fn start(max: usize) -> Daemon {
        let port = free_port();
        let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
        let web = PathBuf::from(runtime).join(format!("cul-web-{port}"));
        std::fs::create_dir_all(&web).expect("create web dir");
        std::fs::write(web.join("index.html"), "<title>placeholder</title>").expect("write page");
        let mut command = Command::new(env!("CARGO_BIN_EXE_computer-use-linux"));
        command
            .args(["desktops", "serve", "--port", &port.to_string()])
            .args(["--max", &max.to_string(), "--web-dir"])
            .arg(&web)
            .env_clear()
            .envs(clean_env())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        if let Ok(agent) = std::env::var("CUL_DESKTOPS_AGENT") {
            command.env("CUL_DESKTOPS_AGENT", agent);
        }
        let child = command.spawn().expect("start daemon");
        let daemon = Daemon { child, port, web };
        wait_for(Duration::from_secs(10), || {
            TcpStream::connect(("127.0.0.1", port)).ok()
        })
        .expect("daemon did not listen");
        daemon
    }

    fn root(&self) -> PathBuf {
        let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
        PathBuf::from(runtime).join(format!("agent-desktops-{}", self.port))
    }

    /// One HTTP/1.1 request. Returns the status, the content type and the body.
    fn request(&self, method: &str, path: &str, body: Option<Value>) -> (u16, String, Vec<u8>) {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        let body = body.map(|value| value.to_string()).unwrap_or_default();
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut reader = BufReader::new(stream);
        let mut status_line = String::new();
        reader.read_line(&mut status_line).unwrap();
        let status: u16 = status_line.split(' ').nth(1).unwrap().parse().unwrap();
        let mut content_type = String::new();
        let mut length = None;
        let mut chunked = false;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            let (name, value) = line.split_once(':').unwrap();
            match name.to_ascii_lowercase().as_str() {
                "content-type" => content_type = value.trim().to_string(),
                "content-length" => length = value.trim().parse::<usize>().ok(),
                "transfer-encoding" => chunked = value.contains("chunked"),
                _ => {}
            }
        }
        let mut body = Vec::new();
        if chunked {
            loop {
                let mut size = String::new();
                reader.read_line(&mut size).unwrap();
                let size = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
                if size == 0 {
                    break;
                }
                let mut chunk = vec![0; size + 2];
                reader.read_exact(&mut chunk).unwrap();
                body.extend_from_slice(&chunk[..size]);
            }
        } else if let Some(length) = length {
            body.resize(length, 0);
            reader.read_exact(&mut body).unwrap();
        } else {
            reader.read_to_end(&mut body).unwrap();
        }
        (status, content_type, body)
    }

    fn json(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let (status, _, body) = self.request(method, path, body);
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // SAFETY: a plain signal to the child we started.
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGTERM);
        }
        let _ = wait_for(Duration::from_secs(25), || {
            self.child.try_wait().ok().flatten()
        });
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.web);
    }
}

/// A minimal WebSocket client: enough for binary RFB frames.
struct Ws {
    stream: TcpStream,
    buffer: Vec<u8>,
}

impl Ws {
    fn connect(port: u16, path: &str) -> Ws {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect ws");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\nSec-WebSocket-Protocol: binary\r\n\r\n"
        )
        .unwrap();
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
        }
        let head = String::from_utf8_lossy(&head);
        assert!(head.starts_with("HTTP/1.1 101"), "{head}");
        Ws {
            stream,
            buffer: Vec::new(),
        }
    }

    fn send(&mut self, payload: &[u8]) {
        let mut frame = vec![0x82];
        match payload.len() {
            length if length < 126 => frame.push(0x80 | length as u8),
            length => {
                frame.push(0x80 | 126);
                frame.extend((length as u16).to_be_bytes());
            }
        }
        let mask = [1u8, 2, 3, 4];
        frame.extend(mask);
        frame.extend(
            payload
                .iter()
                .enumerate()
                .map(|(index, byte)| byte ^ mask[index % 4]),
        );
        self.stream.write_all(&frame).unwrap();
    }

    fn read_frame(&mut self) -> Vec<u8> {
        let mut head = [0u8; 2];
        self.stream.read_exact(&mut head).unwrap();
        let mut length = (head[1] & 0x7f) as usize;
        if length == 126 {
            let mut extended = [0u8; 2];
            self.stream.read_exact(&mut extended).unwrap();
            length = u16::from_be_bytes(extended) as usize;
        } else if length == 127 {
            let mut extended = [0u8; 8];
            self.stream.read_exact(&mut extended).unwrap();
            length = u64::from_be_bytes(extended) as usize;
        }
        let mut payload = vec![0; length];
        self.stream.read_exact(&mut payload).unwrap();
        payload
    }

    fn need(&mut self, count: usize) -> Vec<u8> {
        while self.buffer.len() < count {
            let frame = self.read_frame();
            self.buffer.extend(frame);
        }
        self.buffer.drain(..count).collect()
    }

    /// Run the RFB 3.8 handshake with no security. Returns the screen size.
    fn rfb_handshake(&mut self) -> (u16, u16) {
        assert_eq!(&self.need(12)[..4], b"RFB ");
        self.send(b"RFB 003.008\n");
        let count = self.need(1)[0] as usize;
        assert!(self.need(count).contains(&1), "no None security type");
        self.send(&[1]);
        assert_eq!(self.need(4), [0, 0, 0, 0], "security failed");
        self.send(&[1]);
        let init = self.need(24);
        let name_length = u32::from_be_bytes([init[20], init[21], init[22], init[23]]) as usize;
        self.need(name_length);
        (
            u16::from_be_bytes([init[0], init[1]]),
            u16::from_be_bytes([init[2], init[3]]),
        )
    }

    fn click(&mut self, x: u16, y: u16) {
        for mask in [0u8, 1, 0] {
            let mut event = vec![5, mask];
            event.extend(x.to_be_bytes());
            event.extend(y.to_be_bytes());
            self.send(&event);
            std::thread::sleep(Duration::from_millis(60));
        }
    }
}

fn swaymsg_exec(socket: &Path, command: &str) {
    let status = Command::new("swaymsg")
        .arg("-s")
        .arg(socket)
        .arg("exec")
        .arg(command)
        .stdout(Stdio::null())
        .status()
        .expect("swaymsg exec");
    assert!(status.success());
}

fn pids(dir: &Path) -> Vec<i32> {
    std::fs::read_to_string(dir.join("pids"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.split_once(' ')?.1.trim().parse().ok())
        .collect()
}

fn alive(pid: i32) -> bool {
    // A zombie still has a /proc entry. Read its state to tell.
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit(')')
                .next()
                .map(|rest| !rest.trim_start().starts_with('Z'))
        })
        .unwrap_or(false)
}

#[test]
fn the_daemon_runs_desktops_end_to_end() {
    if let Some(program) = missing(&["sway", "swaymsg", "wayvnc", "grim", "foot", "python3"]) {
        eprintln!("skipping: {program} is not installed");
        return;
    }
    let daemon = Daemon::start(1);

    let (status, created) = daemon.json(
        "POST",
        "/api/desktops",
        Some(json!({"name": "Test Desk", "resolution": "1024x768"})),
    );
    assert_eq!(status, 201, "{created}");
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["name"], "test-desk");
    assert_eq!(created["res"], "1024×768");
    assert_eq!(created["status"], "idle");
    assert_eq!(created["control"], "agent");
    let dir = daemon.root().join(&id);
    let started = pids(&dir);
    assert_eq!(started.len(), 2, "sway and wayvnc pids");

    // The limit holds.
    let (status, refused) = daemon.json("POST", "/api/desktops", Some(json!({})));
    assert_eq!(status, 409, "{refused}");

    let (status, listed) = daemon.json("GET", "/api/desktops", None);
    assert_eq!(status, 200);
    assert_eq!(listed.as_array().map(Vec::len), Some(1));

    let (status, content_type, thumb) =
        daemon.request("GET", &format!("/api/desktops/{id}/thumb.webp"), None);
    assert_eq!(status, 200);
    assert_eq!(content_type, "image/webp");
    assert_eq!(&thumb[..4], b"RIFF");
    assert_eq!(&thumb[8..12], b"WEBP");

    let (status, content_type, page) = daemon.request("GET", "/", None);
    assert_eq!(status, 200);
    assert!(content_type.starts_with("text/html"));
    assert!(String::from_utf8_lossy(&page).contains("placeholder"));
    let (status, _) = daemon.json("GET", "/api/nothing", None);
    assert_eq!(status, 404);

    // View-only: a click through the proxy must not land.
    let socket = dir.join("sway.sock");
    let report = dir.join("click.txt");
    swaymsg_exec(
        &socket,
        &format!(
            "foot -a probe -o pad=0x0 python3 {FIXTURE} {} press",
            report.display()
        ),
    );
    std::thread::sleep(Duration::from_millis(1500));
    let mut viewer = Ws::connect(daemon.port, &format!("/api/desktops/{id}/vnc"));
    assert_eq!(viewer.rfb_handshake(), (1024, 768));
    viewer.click(512, 384);
    std::thread::sleep(Duration::from_millis(800));
    assert!(read_file(&report).is_none(), "a view-only click landed");

    // Take over: the same connection now drives.
    let (status, taken) = daemon.json("POST", &format!("/api/desktops/{id}/takeover"), None);
    assert_eq!(status, 200);
    assert_eq!(taken["control"], "user");
    viewer.click(512, 384);
    let landed = wait_for(Duration::from_secs(5), || read_file(&report));
    assert!(
        landed.as_deref().is_some_and(|text| text.starts_with("0;")),
        "{landed:?}"
    );

    let (status, returned) = daemon.json("POST", &format!("/api/desktops/{id}/handback"), None);
    assert_eq!(status, 200);
    assert_eq!(returned["control"], "agent");

    if std::env::var_os("CUL_TEST_HERDR").is_some() && missing(&["herdr"]).is_none() {
        // The stand-in prints the prompt and waits, like an agent would.
        let (status, assigned) = daemon.json(
            "POST",
            &format!("/api/desktops/{id}/agent"),
            Some(json!({"prompt": "stand-in task"})),
        );
        assert_eq!(status, 200, "{assigned}");
        assert_eq!(assigned["task"], "stand-in task");
        let pane = assigned["pane"].as_str().unwrap().to_string();
        let (status, _) = daemon.json(
            "POST",
            &format!("/api/desktops/{id}/agent"),
            Some(json!({"prompt": "again"})),
        );
        assert_eq!(status, 409);
        let (status, _, _) = daemon.request("DELETE", &format!("/api/desktops/{id}"), None);
        assert_eq!(status, 204);
        // herdr finishes closing a tab a moment after it answers.
        let mut last = String::new();
        let gone = wait_for(Duration::from_secs(5), || {
            let output = Command::new("herdr")
                .args(["pane", "get", &pane])
                .output()
                .expect("herdr pane get");
            // herdr reports errors on stderr.
            last = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            last.contains("pane_not_found").then_some(())
        });
        assert!(gone.is_some(), "pane {pane} survived its desktop: {last}");
    } else {
        let (status, _, _) = daemon.request("DELETE", &format!("/api/desktops/{id}"), None);
        assert_eq!(status, 204);
    }

    assert!(
        wait_for(Duration::from_secs(5), || started
            .iter()
            .all(|pid| !alive(*pid))
            .then_some(()))
        .is_some(),
        "sway or wayvnc outlived its desktop"
    );
    assert!(!dir.exists(), "runtime dir left behind");
    let (_, listed) = daemon.json("GET", "/api/desktops", None);
    assert_eq!(listed, json!([]));

    // A desktop still running at exit goes down with the daemon.
    let (status, created) = daemon.json("POST", "/api/desktops", Some(json!({})));
    assert_eq!(status, 201, "{created}");
    let last = pids(&daemon.root().join(created["id"].as_str().unwrap()));
    // An open event stream, like a PWA tab, must not hold the daemon up.
    let mut events = TcpStream::connect(("127.0.0.1", daemon.port)).expect("connect events");
    write!(
        events,
        "GET /api/events HTTP/1.1\r\nHost: localhost\r\nAccept: text/event-stream\r\n\r\n"
    )
    .unwrap();
    let mut first = [0u8; 64];
    let read = events.read(&mut first).unwrap();
    assert!(String::from_utf8_lossy(&first[..read]).starts_with("HTTP/1.1 200"));
    let root = daemon.root();
    let stopping = std::time::Instant::now();
    drop(daemon);
    assert!(
        stopping.elapsed() < Duration::from_secs(10),
        "the daemon took {:?} to stop with an event stream open",
        stopping.elapsed()
    );
    assert!(
        last.iter().all(|pid| !alive(*pid)),
        "a desktop outlived the daemon"
    );
    assert!(!root.exists(), "the runtime root outlived the daemon");
    drop(events);
}
