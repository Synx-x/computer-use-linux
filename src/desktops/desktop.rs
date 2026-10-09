//! One agent desktop: a headless sway, the wayvnc that streams it, and the
//! runtime directory that holds their sockets.
//!
//! Each child runs in its own process group with a clean environment. A
//! clean environment keeps host keys such as HYPRLAND_INSTANCE_SIGNATURE out
//! of the nested session. Stop kills each whole group, so the apps an agent
//! opened go with its desktop. Each child also gets SIGTERM if the daemon
//! dies, and a `pids` file records the children so the next daemon can clean
//! up after a SIGKILL.

use super::agent::Agent;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

pub const FIRST_VNC_PORT: u16 = 5901;
const READY_TIMEOUT: Duration = Duration::from_secs(10);
const OUTPUT: &str = "HEADLESS-1";

/// The keys a nested child keeps from the daemon's environment. Everything
/// else, the host session's keys included, is dropped.
const KEPT_ENV: &[&str] = &[
    "HOME",
    "USER",
    "LOGNAME",
    "PATH",
    "SHELL",
    "LANG",
    "LC_ALL",
    "XDG_RUNTIME_DIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "DBUS_SESSION_BUS_ADDRESS",
];

pub fn clean_env() -> Vec<(String, String)> {
    KEPT_ENV
        .iter()
        .filter_map(|key| {
            std::env::var(key)
                .ok()
                .map(|value| (key.to_string(), value))
        })
        .collect()
}

/// The root of every desktop's runtime directory for the daemon on `port`.
/// One root per port: the port bind makes one daemon per root, so orphan
/// cleanup never touches another live daemon's desktops.
pub fn runtime_root(port: u16) -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join(format!("agent-desktops-{port}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Running,
    NeedsYou,
    Idle,
}

impl Status {
    /// The word the PWA's `normalize()` reads.
    pub fn word(self) -> &'static str {
        match self {
            Status::Running => "running",
            Status::NeedsYou => "needs",
            Status::Idle => "idle",
        }
    }
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub time: SystemTime,
    pub verb: String,
    pub detail: String,
}

/// What the status poller last saw.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub status: Status,
    pub window_title: String,
    pub log: Vec<LogEntry>,
}

pub struct Desktop {
    pub id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub created: SystemTime,
    pub dir: PathBuf,
    pub display: String,
    pub sway_socket: PathBuf,
    pub vnc_port: u16,
    sway: Mutex<Option<Child>>,
    wayvnc: Mutex<Option<Child>>,
    /// True while the user has taken over input.
    pub user_control: AtomicBool,
    pub agent: Mutex<Option<Agent>>,
    pub snapshot: Mutex<Snapshot>,
    pub thumb: Mutex<Option<(Instant, Vec<u8>)>>,
}

pub fn parse_resolution(value: &str) -> Result<(u32, u32)> {
    let normalized = value.trim().replace(['×', 'X'], "x");
    let (width, height) = normalized
        .split_once('x')
        .ok_or_else(|| anyhow!("resolution must look like 1280x800"))?;
    let width: u32 = width.trim().parse().context("bad resolution width")?;
    let height: u32 = height.trim().parse().context("bad resolution height")?;
    if !(320..=7680).contains(&width) || !(240..=4320).contains(&height) {
        bail!("resolution {width}x{height} is out of range");
    }
    Ok((width, height))
}

/// A desktop name is a label, a herdr tab name and a directory name, so
/// keep it to a safe character set.
pub fn sanitize_name(value: &str) -> String {
    let cleaned: String = value
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('-').to_string();
    cleaned.chars().take(40).collect()
}

fn port_is_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

pub fn pick_vnc_port(taken: &[u16]) -> Result<u16> {
    (FIRST_VNC_PORT..FIRST_VNC_PORT + 200)
        .find(|port| !taken.contains(port) && port_is_free(*port))
        .ok_or_else(|| anyhow!("no free VNC port from {FIRST_VNC_PORT} upward"))
}

/// Spawn a child in its own process group, with a clean environment, that
/// gets SIGTERM when the daemon dies.
fn spawn_group(command: &mut Command, log: &Path) -> Result<Child> {
    let log = std::fs::File::create(log).context("create child log")?;
    command
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0);
    // SAFETY: prctl is async-signal-safe and touches only this child.
    unsafe {
        command.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    command.spawn().context("spawn child")
}

fn tail(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(5)..].join(" | ")
}

/// SIGTERM a process group, wait briefly, then SIGKILL it.
fn kill_group(child: &mut Child) {
    let pgid = child.id() as i32;
    // SAFETY: kill with a negative pid signals a process group we created.
    unsafe {
        libc::kill(-pgid, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // Apps in the group may outlive the leader, so always finish the group.
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
    let _ = child.wait();
}

impl Desktop {
    pub async fn start(
        root: &Path,
        id: String,
        name: String,
        width: u32,
        height: u32,
        vnc_port: u16,
    ) -> Result<Desktop> {
        let dir = root.join(&id);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).context("create desktop runtime dir")?;
        let sway_socket = dir.join("sway.sock");
        let ready = dir.join("ready");
        let config = format!(
            "output {OUTPUT} resolution {width}x{height} position 0 0\n\
             swaybg_command -\n\
             xwayland disable\n\
             default_border none\n\
             exec sh -c 'printf %s \"$WAYLAND_DISPLAY\" > {ready}'\n",
            ready = ready.display()
        );
        std::fs::write(dir.join("config"), config).context("write sway config")?;

        let mut sway = Command::new("sway");
        // Apps sway starts inherit its working directory. Use HOME, not
        // wherever the daemon was started.
        if let Some(home) = std::env::var_os("HOME") {
            sway.current_dir(home);
        }
        sway.arg("-c")
            .arg(dir.join("config"))
            .env_clear()
            .envs(clean_env())
            .env("WLR_BACKENDS", "headless")
            .env("WLR_LIBINPUT_NO_DEVICES", "1")
            .env("SWAYSOCK", &sway_socket);
        let mut sway = spawn_group(&mut sway, &dir.join("sway.log")).context("start sway")?;

        let deadline = Instant::now() + READY_TIMEOUT;
        let display = loop {
            if let Ok(display) = std::fs::read_to_string(&ready) {
                if !display.trim().is_empty() {
                    break display.trim().to_string();
                }
            }
            if let Ok(Some(status)) = sway.try_wait() {
                bail!("sway exited with {status}: {}", tail(&dir.join("sway.log")));
            }
            if Instant::now() >= deadline {
                kill_group(&mut sway);
                bail!("sway did not become ready: {}", tail(&dir.join("sway.log")));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };

        let mut wayvnc = Command::new("wayvnc");
        wayvnc
            // Plain RFB over TCP. The daemon does the WebSocket framing:
            // wayvnc 0.10.2 with neatvnc 1.0.2 segfaults on any WebSocket
            // handshake (seen 2026-10-09, null call in libneatvnc).
            .args(["--disable-resizing", "--output", OUTPUT, "--socket"])
            .arg(dir.join("wayvnc.sock"))
            .args(["127.0.0.1", &vnc_port.to_string()])
            .env_clear()
            .envs(clean_env())
            .env("WAYLAND_DISPLAY", &display);
        let mut wayvnc = match spawn_group(&mut wayvnc, &dir.join("wayvnc.log")) {
            Ok(child) => child,
            Err(error) => {
                kill_group(&mut sway);
                return Err(error.context("start wayvnc"));
            }
        };
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            if tokio::net::TcpStream::connect(("127.0.0.1", vnc_port))
                .await
                .is_ok()
            {
                break;
            }
            let exited = wayvnc.try_wait().ok().flatten();
            if exited.is_some() || Instant::now() >= deadline {
                kill_group(&mut wayvnc);
                kill_group(&mut sway);
                bail!(
                    "wayvnc did not listen on {vnc_port}: {}",
                    tail(&dir.join("wayvnc.log"))
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        std::fs::write(
            dir.join("pids"),
            format!("sway {}\nwayvnc {}\n", sway.id(), wayvnc.id()),
        )
        .context("write pids file")?;

        Ok(Desktop {
            id,
            name,
            width,
            height,
            created: SystemTime::now(),
            dir,
            display,
            sway_socket,
            vnc_port,
            sway: Mutex::new(Some(sway)),
            wayvnc: Mutex::new(Some(wayvnc)),
            user_control: AtomicBool::new(false),
            agent: Mutex::new(None),
            snapshot: Mutex::new(Snapshot {
                status: Status::Idle,
                window_title: String::new(),
                log: vec![LogEntry {
                    time: SystemTime::now(),
                    verb: "start".to_string(),
                    detail: "sway headless".to_string(),
                }],
            }),
            thumb: Mutex::new(None),
        })
    }

    pub fn user_has_control(&self) -> bool {
        self.user_control.load(Ordering::SeqCst)
    }

    /// The environment an agent process on this desktop runs with.
    pub fn nested_env(&self) -> Vec<(String, String)> {
        vec![
            ("WAYLAND_DISPLAY".to_string(), self.display.clone()),
            (
                "SWAYSOCK".to_string(),
                self.sway_socket.display().to_string(),
            ),
            ("XDG_CURRENT_DESKTOP".to_string(), "sway".to_string()),
            ("XDG_SESSION_TYPE".to_string(), "wayland".to_string()),
            ("CUL_NESTED".to_string(), "1".to_string()),
        ]
    }

    /// Run swaymsg against this desktop and parse its JSON reply.
    pub async fn swaymsg(&self, args: &[&str]) -> Result<Value> {
        let output = tokio::process::Command::new("swaymsg")
            .arg("-s")
            .arg(&self.sway_socket)
            .args(args)
            .arg("-r")
            .output()
            .await
            .context("run swaymsg")?;
        serde_json::from_slice(&output.stdout).context("parse swaymsg reply")
    }

    /// The title of the focused view, or empty.
    pub async fn focused_title(&self) -> String {
        fn find(node: &Value) -> Option<String> {
            if node["focused"] == true && node["pid"].is_number() {
                return node["name"].as_str().map(str::to_string);
            }
            ["nodes", "floating_nodes"]
                .iter()
                .filter_map(|key| node[key].as_array())
                .flatten()
                .find_map(find)
        }
        match self.swaymsg(&["-t", "get_tree"]).await {
            Ok(tree) => find(&tree).unwrap_or_default(),
            Err(_) => String::new(),
        }
    }

    /// Capture the desktop with grim and scale it to `width` as WebP.
    pub async fn capture_webp(&self, width: u32) -> Result<Vec<u8>> {
        let output = tokio::process::Command::new("grim")
            .args(["-t", "ppm", "-"])
            .env_clear()
            .envs(clean_env())
            .env("WAYLAND_DISPLAY", &self.display)
            .output()
            .await
            .context("run grim")?;
        if !output.status.success() {
            bail!(
                "grim failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
            let image =
                image::load_from_memory_with_format(&output.stdout, image::ImageFormat::Pnm)
                    .context("decode grim capture")?;
            let height = (u64::from(image.height()) * u64::from(width)
                / u64::from(image.width().max(1))) as u32;
            let scaled = image
                .resize_exact(width, height.max(1), image::imageops::FilterType::Triangle)
                .to_rgba8();
            let mut bytes = Vec::new();
            image::codecs::webp::WebPEncoder::new_lossless(&mut bytes)
                .encode(
                    scaled.as_raw(),
                    scaled.width(),
                    scaled.height(),
                    image::ExtendedColorType::Rgba8,
                )
                .context("encode WebP thumbnail")?;
            Ok(bytes)
        })
        .await
        .context("thumbnail task failed")?
    }

    /// Kill wayvnc and sway, with every app in their groups, and remove the
    /// runtime directory. Safe to call twice.
    pub fn stop(&self) {
        for slot in [&self.wayvnc, &self.sway] {
            if let Ok(mut guard) = slot.lock() {
                if let Some(mut child) = guard.take() {
                    kill_group(&mut child);
                }
            }
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Kill what a previous daemon left behind. A pid is killed only when it is
/// still a sway or wayvnc whose command line names that desktop's directory,
/// so a reused pid is never hit.
pub fn clean_orphans(root: &Path) -> Vec<String> {
    let mut cleaned = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return cleaned;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        let pids = std::fs::read_to_string(dir.join("pids")).unwrap_or_default();
        for line in pids.lines() {
            let Some((name, pid)) = line.split_once(' ') else {
                continue;
            };
            let Ok(pid) = pid.trim().parse::<i32>() else {
                continue;
            };
            let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
            let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
            let cmdline = String::from_utf8_lossy(&cmdline);
            if comm.trim() == name && cmdline.contains(dir.to_string_lossy().as_ref()) {
                // SAFETY: the pid was checked to be our own orphan just above.
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                    libc::kill(pid, libc::SIGKILL);
                }
                cleaned.push(format!("{name} {pid}"));
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    cleaned
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolutions_parse_with_either_separator() {
        assert_eq!(parse_resolution("1280x800").unwrap(), (1280, 800));
        assert_eq!(parse_resolution("1440×900").unwrap(), (1440, 900));
        assert!(parse_resolution("10x10").is_err());
        assert!(parse_resolution("wide").is_err());
    }

    #[test]
    fn names_are_reduced_to_a_safe_label() {
        assert_eq!(sanitize_name("  My Desktop!/../x "), "my-desktop-----x");
        assert_eq!(sanitize_name("ytm-refine"), "ytm-refine");
    }

    #[test]
    fn status_words_match_the_pwa() {
        assert_eq!(Status::NeedsYou.word(), "needs");
        assert_eq!(Status::Running.word(), "running");
    }
}
