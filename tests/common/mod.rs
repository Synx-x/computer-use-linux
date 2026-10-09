//! Shared helpers for the live nested-desktop tests.
//! Each test runs its own headless sway in a fresh runtime directory and stops it on drop.
//! A test skips, and passes, when a required program is missing.

#![allow(dead_code)]

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

pub fn have(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

/// Return the first missing program, so a test can print why it skipped.
pub fn missing(programs: &[&str]) -> Option<String> {
    programs
        .iter()
        .find(|program| !have(program))
        .map(|program| (*program).to_string())
}

/// The environment a nested process gets: the basic variables and nothing
/// that names the host session.
pub fn clean_env() -> Vec<(String, String)> {
    [
        "HOME",
        "USER",
        "PATH",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
        "LANG",
    ]
    .iter()
    .filter_map(|key| {
        std::env::var(key)
            .ok()
            .map(|value| (key.to_string(), value))
    })
    .collect()
}

pub fn wait_for<T>(timeout: Duration, mut probe: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = probe() {
            return Some(value);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub fn read_file(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .filter(|text| !text.is_empty())
}

/// The host cursor as Hyprland reports it, when the host is Hyprland.
pub fn host_cursor() -> Option<String> {
    let output = Command::new("hyprctl").arg("cursorpos").output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub struct HeadlessSway {
    child: Child,
    pub dir: PathBuf,
    pub socket: PathBuf,
    pub display: String,
}

impl HeadlessSway {
    pub fn start(width: u32, height: u32) -> HeadlessSway {
        let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.subsec_nanos())
            .unwrap_or(0);
        // Unix socket paths cap at 108 bytes, so keep the directory short.
        let dir = PathBuf::from(runtime).join(format!("cul-it-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create test runtime dir");
        let socket = dir.join("sway.sock");
        let ready = dir.join("ready");
        let config = format!(
            "output HEADLESS-1 resolution {width}x{height} position 0 0\n\
             swaybg_command -\n\
             xwayland disable\n\
             default_border none\n\
             exec sh -c 'printf %s \"$WAYLAND_DISPLAY\" > {ready}'\n",
            ready = ready.display()
        );
        std::fs::write(dir.join("config"), config).expect("write sway config");
        let log = std::fs::File::create(dir.join("sway.log")).expect("create sway log");
        let child = Command::new("sway")
            .arg("-c")
            .arg(dir.join("config"))
            .env_clear()
            .envs(clean_env())
            .env("WLR_BACKENDS", "headless")
            .env("WLR_LIBINPUT_NO_DEVICES", "1")
            .env("SWAYSOCK", &socket)
            .stdin(Stdio::null())
            .stdout(log.try_clone().expect("clone log"))
            .stderr(log)
            .process_group(0)
            .spawn()
            .expect("start sway");
        let display = wait_for(Duration::from_secs(10), || read_file(&ready))
            .expect("headless sway did not become ready");
        HeadlessSway {
            child,
            dir,
            socket,
            display,
        }
    }

    /// Run a shell command inside the nested session.
    pub fn exec(&self, command: &str) {
        let status = Command::new("swaymsg")
            .arg("-s")
            .arg(&self.socket)
            .arg("exec")
            .arg(command)
            .stdout(Stdio::null())
            .status()
            .expect("run swaymsg exec");
        assert!(status.success(), "swaymsg exec failed for {command}");
    }

    pub fn tree(&self) -> Value {
        let output = Command::new("swaymsg")
            .arg("-s")
            .arg(&self.socket)
            .args(["-t", "get_tree", "-r"])
            .output()
            .expect("run swaymsg get_tree");
        serde_json::from_slice(&output.stdout).unwrap_or(Value::Null)
    }

    /// Wait until a view with `app_id` has keyboard focus.
    pub fn wait_focused(&self, app_id: &str) -> bool {
        fn focused(node: &Value, app_id: &str) -> bool {
            if node["app_id"] == app_id && node["focused"] == true {
                return true;
            }
            ["nodes", "floating_nodes"].iter().any(|key| {
                node[key]
                    .as_array()
                    .is_some_and(|children| children.iter().any(|child| focused(child, app_id)))
            })
        }
        wait_for(Duration::from_secs(15), || {
            focused(&self.tree(), app_id).then_some(())
        })
        .is_some()
    }

    pub fn mcp(&self) -> Mcp {
        Mcp::start(&[
            ("WAYLAND_DISPLAY", self.display.as_str()),
            ("SWAYSOCK", self.socket.to_str().expect("utf-8 socket path")),
            ("CUL_NESTED", "1"),
        ])
    }
}

impl Drop for HeadlessSway {
    fn drop(&mut self) {
        // sway started its own process group. Kill the whole group so the
        // apps it ran go with it.
        let pgid = self.child.id() as i32;
        unsafe {
            libc::kill(-pgid, libc::SIGTERM);
        }
        let _ = wait_for(Duration::from_secs(3), || {
            self.child.try_wait().ok().flatten()
        });
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A minimal MCP client over the server's stdio.
pub struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Mcp {
    pub fn start(env: &[(&str, &str)]) -> Mcp {
        let mut child = Command::new(env!("CARGO_BIN_EXE_computer-use-linux"))
            .arg("mcp")
            .env_clear()
            .envs(clean_env())
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start MCP server");
        let stdin = child.stdin.take().expect("MCP stdin");
        let stdout = BufReader::new(child.stdout.take().expect("MCP stdout"));
        let mut mcp = Mcp {
            child,
            stdin,
            stdout,
            next_id: 0,
        };
        mcp.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "nested-test", "version": "0"}
            }),
        );
        mcp.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        mcp
    }

    fn send(&mut self, message: Value) {
        writeln!(self.stdin, "{message}").expect("write MCP request");
        self.stdin.flush().expect("flush MCP request");
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            let read = self.stdout.read_line(&mut line).expect("read MCP reply");
            assert!(read > 0, "MCP server exited during {method}");
            let reply: Value = serde_json::from_str(&line).expect("MCP reply is JSON");
            if reply["id"] == id {
                return reply;
            }
        }
    }

    /// Call a tool and return its structured content.
    pub fn call(&mut self, tool: &str, arguments: Value) -> Value {
        let reply = self.request("tools/call", json!({"name": tool, "arguments": arguments}));
        let result = &reply["result"];
        if !result["structuredContent"].is_null() {
            return result["structuredContent"].clone();
        }
        result.clone()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
