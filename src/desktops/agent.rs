//! The agent on a desktop: a Claude Code session in a herdr pane.
//!
//! Each desktop's agent gets its own tab in the herdr workspace
//! `agent-desktops`. The agent runs with `--strict-mcp-config` and one MCP
//! server, this binary in nested mode. A stale or host-only build in the
//! user's own MCP config can then never become its input path.

use super::desktop::{Desktop, LogEntry, Status};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

pub const WORKSPACE_LABEL: &str = "agent-desktops";
const TRUST_TIMEOUT: Duration = Duration::from_secs(45);
const TRANSCRIPT_TAIL_BYTES: u64 = 256 * 1024;
const LOG_ENTRIES: usize = 12;

#[derive(Debug, Clone)]
pub struct Agent {
    pub pane_id: String,
    pub tab_id: String,
    pub prompt: String,
    pub cwd: PathBuf,
    pub started: SystemTime,
}

async fn herdr(args: &[&str]) -> Result<Value> {
    let output = tokio::process::Command::new("herdr")
        .args(args)
        .output()
        .await
        .context("run herdr")?;
    // Some commands, `pane run` among them, print nothing on success.
    if output.status.success() && output.stdout.iter().all(u8::is_ascii_whitespace) {
        return Ok(Value::Null);
    }
    // A failed command prints its JSON error on stderr.
    if !output.status.success() {
        let message = serde_json::from_slice::<Value>(&output.stderr)
            .ok()
            .and_then(|reply| reply["error"]["message"].as_str().map(str::to_string))
            .unwrap_or_else(|| String::from_utf8_lossy(&output.stderr).trim().to_string());
        bail!("herdr {}: {message}", args.join(" "));
    }
    let reply: Value = serde_json::from_slice(&output.stdout).with_context(|| {
        format!(
            "herdr {} gave no JSON: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )
    })?;
    if let Some(error) = reply.get("error") {
        bail!(
            "herdr {}: {}",
            args.join(" "),
            error["message"].as_str().unwrap_or("error")
        );
    }
    Ok(reply["result"].clone())
}

/// Text herdr shows for a pane, or empty.
async fn pane_text(pane: &str) -> String {
    let output = tokio::process::Command::new("herdr")
        .args(["pane", "read", pane, "--source", "visible", "--lines", "40"])
        .output()
        .await;
    match output {
        Ok(output) => {
            let text = String::from_utf8_lossy(&output.stdout).into_owned();
            // `pane read` answers in JSON on some versions and plain text on
            // others. Take the text field when there is one.
            serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|reply| find_string(&reply["result"], "text"))
                .unwrap_or(text)
        }
        Err(_) => String::new(),
    }
}

fn find_string(value: &Value, key: &str) -> Option<String> {
    match value {
        Value::Object(map) => map
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| map.values().find_map(|child| find_string(child, key))),
        Value::Array(items) => items.iter().find_map(|child| find_string(child, key)),
        _ => None,
    }
}

/// Find the `agent-desktops` workspace or create it. Returns its id and,
/// when it was just created, its unused root pane and tab.
async fn ensure_workspace(cwd: &Path) -> Result<(String, Option<(String, String)>)> {
    let list = herdr(&["workspace", "list"]).await?;
    if let Some(found) = list["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|workspace| workspace["label"] == WORKSPACE_LABEL)
    {
        let id = found["workspace_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        return Ok((id, None));
    }
    let cwd = cwd.to_string_lossy();
    let created = herdr(&[
        "workspace",
        "create",
        "--label",
        WORKSPACE_LABEL,
        "--cwd",
        &cwd,
        "--no-focus",
    ])
    .await?;
    let id = created["workspace"]["workspace_id"]
        .as_str()
        .ok_or_else(|| anyhow!("herdr workspace create returned no id"))?
        .to_string();
    let root = created["root_pane"]["pane_id"].as_str().map(str::to_string);
    let tab = created["tab"]["tab_id"].as_str().map(str::to_string);
    Ok((id, root.zip(tab)))
}

/// Quote a string for a POSIX shell.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// The guidance every desktop agent gets on top of its task.
fn system_prompt(desktop: &Desktop) -> String {
    format!(
        "You run on your own private Wayland desktop, {display}, a headless sway at {w}x{h}. \
         The computer-use-linux MCP tools act on that desktop only: screenshot it, then click, \
         type and press keys. Start apps with `swaymsg exec <command>`. The user can watch the \
         desktop and may take over. If you are interrupted, wait for the word continue.",
        display = desktop.display,
        w = desktop.width,
        h = desktop.height,
    )
}

/// The MCP config that gives the agent this binary as its only server,
/// pinned to the desktop's display.
fn write_mcp_config(desktop: &Desktop) -> Result<PathBuf> {
    let exe = std::env::current_exe().context("find the daemon executable")?;
    let env: serde_json::Map<String, Value> = desktop
        .nested_env()
        .into_iter()
        .map(|(key, value)| (key, Value::String(value)))
        .collect();
    let config = json!({
        "mcpServers": {
            "computer-use-linux": {
                "command": exe,
                "args": ["mcp"],
                "env": env,
            }
        }
    });
    let path = desktop.dir.join("mcp.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&config)?).context("write mcp.json")?;
    Ok(path)
}

/// The shell command the pane runs. `CUL_DESKTOPS_AGENT` replaces the
/// `claude` program, which tests use to run a stand-in.
fn agent_command(desktop: &Desktop, prompt: &str, mcp_config: &Path, cwd: &Path) -> String {
    let program = std::env::var("CUL_DESKTOPS_AGENT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            format!(
                "claude --permission-mode bypassPermissions --strict-mcp-config --mcp-config {} \
                 --append-system-prompt {}",
                shell_quote(&mcp_config.to_string_lossy()),
                shell_quote(&system_prompt(desktop)),
            )
        });
    let mut command = format!("cd {} && env", shell_quote(&cwd.to_string_lossy()));
    for key in crate::nested_input::HOST_ONLY_ENV_KEYS {
        command.push_str(&format!(" -u {key}"));
    }
    for (key, value) in desktop.nested_env() {
        command.push_str(&format!(" {key}={}", shell_quote(&value)));
    }
    if let Some(sandbox) = sandbox_prefix(desktop, cwd) {
        command.push(' ');
        command.push_str(&sandbox);
    }
    command.push_str(&format!(" {program} {}", shell_quote(prompt)));
    command
}

/// The bubblewrap prefix that confines the agent, or None when the sandbox is
/// off (`CUL_DESKTOPS_SANDBOX=off`) or `bwrap` is missing.
///
/// The agent runs with bypass permissions, so its shell is the real risk, not its
/// MCP tools. The sandbox mounts the filesystem read-only except the agent's own
/// folder, `~/.claude` and a scratch cache. It replaces `/dev`, `/tmp` and the
/// runtime directory, so the host Wayland and Hyprland sockets, the session bus,
/// the ydotool socket and `/dev/uinput` are gone. Only this desktop's Wayland and
/// sway sockets come back. The herdr socket is hidden too, because it can run
/// commands in any pane.
fn sandbox_prefix(desktop: &Desktop, cwd: &Path) -> Option<String> {
    if std::env::var("CUL_DESKTOPS_SANDBOX").is_ok_and(|value| value.trim() == "off") {
        return None;
    }
    let bwrap = which("bwrap")?;
    let home = std::env::var("HOME").ok()?;
    let runtime = std::env::var("XDG_RUNTIME_DIR").ok()?;
    let wayland = format!("{runtime}/{}", desktop.display);
    let q = |path: &str| shell_quote(path);
    let mut args = vec![
        "--ro-bind / /".to_string(),
        "--dev /dev".to_string(),
        "--proc /proc".to_string(),
        "--tmpfs /tmp".to_string(),
        format!("--tmpfs {}", q(&runtime)),
        format!("--bind {} {}", q(&wayland), q(&wayland)),
        format!("--bind {d} {d}", d = q(&desktop.dir.to_string_lossy())),
        format!("--bind {d} {d}", d = q(&cwd.to_string_lossy())),
        format!("--tmpfs {}", q(&format!("{home}/.cache"))),
        format!("--tmpfs {}", q(&format!("{home}/.config/herdr"))),
        "--unshare-pid".to_string(),
        "--die-with-parent".to_string(),
    ];
    for path in [format!("{home}/.claude"), format!("{home}/.claude.json")] {
        if Path::new(&path).exists() {
            args.push(format!("--bind {p} {p}", p = q(&path)));
        }
    }
    Some(format!(
        "{} {} --",
        q(&bwrap.to_string_lossy()),
        args.join(" ")
    ))
}

fn which(program: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .map(|dir| Path::new(dir).join(program))
        .find(|path| path.is_file())
}

/// Answer Claude's folder-trust prompt, then wait for the session banner.
async fn accept_trust(pane: &str) -> bool {
    let deadline = Instant::now() + TRUST_TIMEOUT;
    while Instant::now() < deadline {
        let text = pane_text(pane).await;
        if text.contains("bypass permissions on") {
            return true;
        }
        if text.contains("Yes, I trust this folder") {
            if !text.contains("❯ Yes, I trust this folder") {
                let _ = herdr(&["pane", "send-keys", pane, "down"]).await;
            }
            let _ = herdr(&["pane", "send-keys", pane, "enter"]).await;
        }
        tokio::time::sleep(Duration::from_millis(800)).await;
    }
    false
}

/// Start an agent on `desktop` with `prompt`.
pub async fn assign(desktop: &Desktop, prompt: &str) -> Result<Agent> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/tmp".into());
    let cwd = home.join(".local/share/agent-desktops").join(&desktop.name);
    std::fs::create_dir_all(&cwd).context("create agent working directory")?;
    let (workspace, fresh_root) = ensure_workspace(&cwd).await?;
    let cwd_text = cwd.to_string_lossy().into_owned();
    let (pane_id, tab_id) = match fresh_root {
        Some((pane, tab)) => {
            let _ = herdr(&["tab", "rename", &tab, &desktop.name]).await;
            (pane, tab)
        }
        None => {
            let created = herdr(&[
                "tab",
                "create",
                "--workspace",
                &workspace,
                "--label",
                &desktop.name,
                "--cwd",
                &cwd_text,
                "--no-focus",
            ])
            .await?;
            let pane = created["root_pane"]["pane_id"].as_str();
            let tab = created["tab"]["tab_id"].as_str();
            match pane.zip(tab) {
                Some((pane, tab)) => (pane.to_string(), tab.to_string()),
                None => bail!("herdr tab create returned no pane"),
            }
        }
    };
    let mcp_config = write_mcp_config(desktop)?;
    let command = agent_command(desktop, prompt, &mcp_config, &cwd);
    herdr(&["pane", "run", &pane_id, &command]).await?;
    let agent = Agent {
        pane_id,
        tab_id,
        prompt: prompt.to_string(),
        cwd,
        started: SystemTime::now(),
    };
    if std::env::var_os("CUL_DESKTOPS_AGENT").is_none() && !accept_trust(&agent.pane_id).await {
        // The pane stays open so the user can see what went wrong.
        bail!("the agent in pane {} did not start", agent.pane_id);
    }
    Ok(agent)
}

/// The agent's status. herdr's own `agent_status` wins when it knows.
/// Otherwise the pane decides: a question or permission prompt on screen
/// means the agent needs the user, and Claude's terminal title starts with
/// a spinner while it works and with ✳ when it waits.
pub async fn status(agent: &Agent) -> Status {
    let Ok(reply) = herdr(&["pane", "get", &agent.pane_id]).await else {
        return Status::Idle;
    };
    match find_string(&reply, "agent_status").as_deref() {
        Some("working") => return Status::Running,
        Some("blocked") => return Status::NeedsYou,
        Some("idle") => return Status::Idle,
        _ => {}
    }
    let title = find_string(&reply, "terminal_title").unwrap_or_default();
    status_from_screen(&title, &pane_text(&agent.pane_id).await)
}

/// Text Claude Code shows only while it waits on the user.
const NEEDS_YOU_MARKERS: &[&str] = &[
    "Enter to select",
    "Do you want to proceed?",
    "Do you want to make this edit",
    "Do you want to create",
    "Yes, I trust this folder",
];

pub fn status_from_screen(title: &str, screen: &str) -> Status {
    if NEEDS_YOU_MARKERS
        .iter()
        .any(|marker| screen.contains(marker))
    {
        return Status::NeedsYou;
    }
    match title.trim_start().chars().next() {
        Some('✳') | None => Status::Idle,
        // Braille dots and quarter circles are Claude's working spinners.
        Some(glyph) if ('\u{2800}'..='\u{28ff}').contains(&glyph) => Status::Running,
        Some('◐' | '◓' | '◑' | '◒' | '✢' | '✶' | '✻' | '✽' | '·') => {
            Status::Running
        }
        Some(_) => Status::Idle,
    }
}

/// Interrupt the agent so the user can drive.
pub async fn interrupt(agent: &Agent) -> Result<()> {
    herdr(&["pane", "send-keys", &agent.pane_id, "escape"]).await?;
    Ok(())
}

/// Tell the agent to carry on after a hand back.
pub async fn resume(agent: &Agent) -> Result<()> {
    herdr(&["pane", "send-text", &agent.pane_id, "continue"]).await?;
    herdr(&["pane", "send-keys", &agent.pane_id, "enter"]).await?;
    Ok(())
}

/// Close the agent's tab, which ends its process.
pub async fn close(agent: &Agent) {
    if herdr(&["tab", "close", &agent.tab_id]).await.is_err() {
        let _ = herdr(&["pane", "close", &agent.pane_id]).await;
    }
}

/// Claude names a project directory after the cwd with every character
/// that is not ASCII alphanumeric replaced by `-`.
fn project_slug(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect()
}

fn newest_transcript(cwd: &Path, since: SystemTime) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let projects = home.join(".claude/projects");
    let mut candidates = vec![cwd.to_path_buf()];
    if let Ok(real) = cwd.canonicalize() {
        candidates.push(real);
    }
    candidates
        .iter()
        .filter_map(|path| std::fs::read_dir(projects.join(project_slug(path))).ok())
        .flatten()
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            (modified >= since).then_some((modified, entry.path()))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

/// Seconds since the epoch for an RFC 3339 UTC time such as
/// `2026-10-09T17:42:08.123Z`.
fn parse_utc(value: &str) -> Option<SystemTime> {
    let (date, time) = value.split_once('T')?;
    let mut date = date.split('-').map(|part| part.parse::<i64>().ok());
    let (year, month, day) = (date.next()??, date.next()??, date.next()??);
    let time = time.trim_end_matches('Z');
    let mut clock = time.split(':');
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    let second: f64 = clock.next()?.parse().ok()?;
    // Days from the civil date, after Howard Hinnant's algorithm.
    let shifted = if month <= 2 { year - 1 } else { year };
    let era = shifted.div_euclid(400);
    let year_of_era = shifted - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    let seconds = days * 86_400 + hour * 3600 + minute * 60;
    let total = seconds as f64 + second;
    (total >= 0.0).then(|| SystemTime::UNIX_EPOCH + Duration::from_secs_f64(total))
}

fn shorten(value: &str, limit: usize) -> String {
    let flat = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= limit {
        return flat;
    }
    let cut: String = flat.chars().take(limit.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// A one-line description of a tool call's input.
fn describe_input(input: &Value) -> String {
    let Some(map) = input.as_object() else {
        return String::new();
    };
    // An act batch reads best as its steps: type "gg", key Enter.
    if let Some(steps) = map.get("steps").and_then(Value::as_array) {
        let parts: Vec<String> = steps
            .iter()
            .filter_map(Value::as_object)
            .filter_map(|step| step.iter().next())
            .map(|(verb, args)| match describe_input(args).as_str() {
                "" => verb.clone(),
                detail => format!("{verb} {detail}"),
            })
            .collect();
        if !parts.is_empty() {
            return shorten(&parts.join(", "), 80);
        }
    }
    for key in [
        "text",
        "key",
        "command",
        "prompt",
        "description",
        "file_path",
        "url",
    ] {
        if let Some(value) = map.get(key).and_then(Value::as_str) {
            return shorten(value, 80);
        }
    }
    if let (Some(x), Some(y)) = (map.get("x"), map.get("y")) {
        return format!("at {x},{y}");
    }
    shorten(&Value::Object(map.clone()).to_string(), 80)
}

/// Turn transcript lines into log entries, newest first.
pub fn log_from_transcript(lines: &str) -> Vec<LogEntry> {
    let mut entries = Vec::new();
    for line in lines.lines() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if record["type"] != "assistant" {
            continue;
        }
        let time = record["timestamp"]
            .as_str()
            .and_then(parse_utc)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        for block in record["message"]["content"]
            .as_array()
            .into_iter()
            .flatten()
        {
            match block["type"].as_str() {
                Some("tool_use") => {
                    let name = block["name"].as_str().unwrap_or("tool");
                    let verb = name.rsplit("__").next().unwrap_or(name).to_string();
                    entries.push(LogEntry {
                        time,
                        verb,
                        detail: describe_input(&block["input"]),
                    });
                }
                Some("text") => {
                    let text = block["text"].as_str().unwrap_or_default();
                    if !text.trim().is_empty() {
                        entries.push(LogEntry {
                            time,
                            verb: "say".to_string(),
                            detail: shorten(text, 100),
                        });
                    }
                }
                _ => {}
            }
        }
    }
    entries.reverse();
    entries.truncate(LOG_ENTRIES);
    entries
}

/// The newest actions from the agent's transcript.
pub fn action_log(agent: &Agent) -> Vec<LogEntry> {
    use std::io::{Read, Seek, SeekFrom};
    let Some(path) = newest_transcript(&agent.cwd, agent.started) else {
        return Vec::new();
    };
    let Ok(mut file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let length = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let start = length.saturating_sub(TRANSCRIPT_TAIL_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    // Skip the partial first line when reading from the middle of the file.
    let text = if start > 0 {
        text.split_once('\n')
            .map(|(_, rest)| rest)
            .unwrap_or_default()
    } else {
        &text
    };
    log_from_transcript(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_match_claude_project_directories() {
        assert_eq!(
            project_slug(Path::new("/home/pc/.local/share/agent-desktops/a1")),
            "-home-pc--local-share-agent-desktops-a1"
        );
    }

    #[test]
    fn utc_timestamps_parse_to_epoch_seconds() {
        let time = parse_utc("2026-10-09T17:42:08.500Z").unwrap();
        let seconds = time
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        assert_eq!(seconds, 1_791_567_728.5);
        assert!(parse_utc("not a time").is_none());
    }

    #[test]
    fn transcript_tool_calls_become_log_entries_newest_first() {
        let lines = [
            json!({"type": "user", "message": {"content": "go"}}),
            json!({"type": "assistant", "timestamp": "2026-10-09T17:00:00Z", "message": {"content": [
                {"type": "tool_use", "name": "mcp__computer-use-linux__click", "input": {"x": 4, "y": 5}}
            ]}}),
            json!({"type": "assistant", "timestamp": "2026-10-09T17:00:01Z", "message": {"content": [
                {"type": "tool_use", "name": "mcp__computer-use-linux__type_text", "input": {"text": "gg"}},
                {"type": "text", "text": "Done."}
            ]}}),
        ]
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
        let log = log_from_transcript(&lines);
        let verbs: Vec<&str> = log.iter().map(|entry| entry.verb.as_str()).collect();
        assert_eq!(verbs, ["say", "type_text", "click"]);
        assert_eq!(log[1].detail, "gg");
        assert_eq!(log[2].detail, "at 4,5");
    }

    #[test]
    fn act_batches_read_as_their_steps() {
        let input = json!({"steps": [{"type": {"text": "gg"}}, {"key": {"key": "Enter"}}, {"wait": {"ms": 5}}]});
        assert_eq!(
            describe_input(&input),
            r#"type gg, key Enter, wait {"ms":5}"#
        );
    }

    #[test]
    fn the_screen_decides_status_when_herdr_does_not_know() {
        assert_eq!(status_from_screen("✳ Foot echo", ""), Status::Idle);
        assert_eq!(status_from_screen("◑ Foot echo", ""), Status::Running);
        assert_eq!(status_from_screen("⠋ Working", ""), Status::Running);
        assert_eq!(
            status_from_screen("✳ Waiting", "Enter to select · ↑/↓ to navigate"),
            Status::NeedsYou
        );
    }

    #[test]
    fn prompts_are_quoted_for_the_shell() {
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }
}
