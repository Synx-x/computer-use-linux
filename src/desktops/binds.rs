//! Keybinds for an agent desktop's sway, taken from the user's Hyprland.
//!
//! With the key lock on, Super and Alt combos reach the desktop, so the desktop
//! needs binds for them. Each desktop reads the live Hyprland binds and writes the
//! ones that have a sway equivalent. Fullscreen, focus, move, resize, workspaces
//! and floating translate directly. An exec bind runs on the desktop. Its command
//! goes into a script file, so no shell quoting passes through the sway config.
//!
//! Every bind runs with the desktop's `bin/` first on PATH. That directory holds
//! shims for the host tools binds and the user's scripts call. `hyprctl` turns
//! dispatches and queries into sway IPC. `caelestia shell` reaches the desktop's
//! own Caelestia shell by pid, never the host's. `pkill` and `killall` only signal
//! processes started in this desktop. `uwsm-app` runs the app directly, and
//! `hyprshade` does nothing. Hyprland dispatchers with no direct sway command go
//! through the same `hyprctl` shim, and submaps become sway modes. Only session
//! power commands, such as `systemctl` and `loginctl`, are still skipped, since
//! those would act on the host.
//!
//! `CUL_DESKTOP_BINDS=default` uses a small built-in set, and `off` writes none.

use serde_json::Value;
use std::fmt::Write as _;
use std::path::Path;

/// Commands that act on the host session and have no desktop meaning.
const HOST_ONLY: &[&str] = &["uwsm ", "systemctl", "loginctl", "wlogout"];

/// The shims written to each desktop's `bin/`, as (file name, contents).
const SHIMS: &[(&str, &str)] = &[
    ("hyprctl", include_str!("shims/hyprctl.py")),
    ("caelestia", include_str!("shims/caelestia.sh")),
    ("pkill", include_str!("shims/pkill.sh")),
    ("killall", include_str!("shims/pkill.sh")),
    ("uwsm-app", include_str!("shims/uwsm-app.sh")),
    ("hyprshade", include_str!("shims/nop.sh")),
    ("hyprlock", "#!/bin/sh\nexec caelestia shell lock lock\n"),
];

/// Write the shims to `dir/bin`, or None when the directory cannot be written.
fn write_shims(dir: &Path) -> Option<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).ok()?;
    let host = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").unwrap_or_default();
    for (name, body) in SHIMS {
        let path = bin.join(name);
        let body = if host.is_empty() {
            body.to_string()
        } else {
            body.replace("__HOST_SIGNATURE__", &host)
        };
        std::fs::write(&path, body).ok()?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).ok()?;
    }
    Some(bin)
}

/// Quote a string for sh.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The sway config lines that give the desktop the host's window look: gaps,
/// border width and colours, and rounding when the sway is SwayFX. Falls back to
/// a padded, bordered default when Hyprland cannot answer.
pub fn sway_look() -> String {
    let get = |option: &str| -> Option<Value> {
        let output = std::process::Command::new("hyprctl")
            .args(["getoption", option, "-j"])
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| serde_json::from_slice(&output.stdout).ok())
            .flatten()
    };
    let first_int = |value: &Option<Value>, fallback: i64| -> i64 {
        value
            .as_ref()
            .and_then(|v| {
                v["int"].as_i64().or_else(|| {
                    v["custom"]
                        .as_str()?
                        .split_whitespace()
                        .next()?
                        .parse()
                        .ok()
                })
            })
            .unwrap_or(fallback)
    };
    let colour = |value: &Option<Value>, fallback: &str| -> String {
        value
            .as_ref()
            .and_then(|v| v["custom"].as_str())
            .and_then(|text| text.split_whitespace().next())
            .and_then(argb_to_hex)
            .unwrap_or_else(|| fallback.to_string())
    };
    let gaps_in = first_int(&get("general:gaps_in"), 4).max(0);
    let gaps_out = first_int(&get("general:gaps_out"), 8).max(0);
    let border = first_int(&get("general:border_size"), 2).max(1);
    let rounding = first_int(&get("decoration:rounding"), 8).max(0);
    let active = colour(&get("general:col.active_border"), "#B2FF66");
    let inactive = colour(&get("general:col.inactive_border"), "#3A3846");
    let mut out = format!(
        "default_border pixel {border}\n\
         default_floating_border pixel {border}\n\
         gaps inner {}\n\
         gaps outer {gaps_out}\n\
         smart_borders off\n\
         client.focused {active} {active} #ffffff {active} {active}\n\
         client.focused_inactive {inactive} {inactive} #cccccc {inactive} {inactive}\n\
         client.unfocused {inactive} {inactive} #cccccc {inactive} {inactive}\n",
        gaps_in * 2
    );
    if is_swayfx() {
        let _ = writeln!(out, "corner_radius {rounding}");
    }
    out
}

/// SwayFX adds rounding and other effects; plain sway rejects those commands.
fn is_swayfx() -> bool {
    std::process::Command::new("sway")
        .arg("--version")
        .output()
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .to_lowercase()
                .contains("swayfx")
        })
        .unwrap_or(false)
}

/// Hyprland colours are AARRGGBB hex, sometimes with a `0x` or `rgba(` wrapper.
fn argb_to_hex(text: &str) -> Option<String> {
    let hex: String = text
        .trim_start_matches("rgba(")
        .trim_start_matches("0x")
        .trim_end_matches(')')
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect();
    match hex.len() {
        8 => Some(format!("#{}", &hex[2..])),
        6 => Some(format!("#{hex}")),
        _ => None,
    }
}

/// The sway config lines for this desktop's binds. Exec scripts go in `dir`.
pub fn sway_binds(dir: &Path) -> String {
    let mode = std::env::var("CUL_DESKTOP_BINDS").unwrap_or_default();
    match mode.trim() {
        "off" => String::new(),
        "default" => default_binds(),
        _ => hyprland_binds(dir).unwrap_or_else(default_binds),
    }
}

/// A small set that matches common Hyprland defaults, for hosts without Hyprland.
fn default_binds() -> String {
    let mut out = String::from("floating_modifier Mod4 normal\n");
    for (keys, command) in [
        ("Mod4+a", "fullscreen toggle"),
        ("Mod4+Shift+a", "fullscreen toggle"),
        ("Mod4+q", "kill"),
        ("Mod4+Return", "exec foot"),
        ("Mod4+f", "floating toggle"),
        ("Mod4+Left", "focus left"),
        ("Mod4+Right", "focus right"),
        ("Mod4+Up", "focus up"),
        ("Mod4+Down", "focus down"),
    ] {
        let _ = writeln!(out, "bindsym {keys} {command}");
    }
    for n in 1..=9 {
        let _ = writeln!(out, "bindsym Mod4+{n} workspace number {n}");
        let _ = writeln!(
            out,
            "bindsym Mod4+Shift+{n} move container to workspace number {n}"
        );
    }
    out
}

/// Translate the live Hyprland binds, or None when Hyprland cannot answer.
fn hyprland_binds(dir: &Path) -> Option<String> {
    let output = std::process::Command::new("hyprctl")
        .args(["binds", "-j"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let binds: Vec<Value> = serde_json::from_slice(&output.stdout).ok()?;
    let bin = write_shims(dir)?;
    Some(translate_binds(&binds, &dir.join("binds"), &bin))
}

/// The sway config for a list of Hyprland binds. Exec scripts go in `scripts`.
fn translate_binds(binds: &[Value], scripts: &Path, bin: &Path) -> String {
    let mut modes: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    let mut entered: std::collections::BTreeMap<String, String> = Default::default();
    let mut seen = std::collections::HashSet::new();
    let mut count = 0;
    for bind in binds {
        if bind["mouse"] == true {
            continue;
        }
        let Some(keys) = sway_keys(
            bind["modmask"].as_u64().unwrap_or(0),
            bind["key"].as_str().unwrap_or(""),
        ) else {
            continue;
        };
        let submap = bind["submap"].as_str().unwrap_or("").to_string();
        if !seen.insert((submap.clone(), keys.clone())) {
            continue;
        }
        let dispatcher = bind["dispatcher"].as_str().unwrap_or("");
        let arg = bind["arg"].as_str().unwrap_or("").trim();
        // Remember which key enters a submap, so its mode always has a way out.
        let enters = if dispatcher == "submap" {
            Some(arg.to_string())
        } else {
            arg.split("submap ").nth(1).map(|rest| {
                rest.split(|c: char| c == ';' || c == '"' || c.is_whitespace())
                    .next()
                    .unwrap_or("")
                    .to_string()
            })
        };
        if let Some(name) = enters.filter(|n| !n.is_empty() && n != "reset") {
            entered.entry(name).or_insert_with(|| keys.clone());
        }
        let command = if dispatcher == "exec" {
            match exec_script(scripts, bin, count, arg) {
                Some(path) => format!("exec sh {}", path.display()),
                None => continue,
            }
        } else if dispatcher == "submap" {
            if arg == "reset" || arg.is_empty() {
                "mode default".to_string()
            } else {
                format!("mode \"{arg}\"")
            }
        } else if let Some(command) = translate(dispatcher, arg) {
            command
        } else {
            // No direct sway command: the hyprctl shim maps it, or accepts it as a no-op.
            let line = format!("hyprctl dispatch {dispatcher} {}", shell_quote(arg));
            match exec_script(scripts, bin, count, &line) {
                Some(path) => format!("exec sh {}", path.display()),
                None => continue,
            }
        };
        let mut flags = String::new();
        if bind["locked"] == true {
            flags.push_str("--locked ");
        }
        if bind["release"] == true {
            flags.push_str("--release ");
        }
        if bind["repeat"] != true {
            flags.push_str("--no-repeat ");
        }
        modes
            .entry(submap)
            .or_default()
            .push(format!("bindsym {flags}{keys} {command}"));
        count += 1;
    }
    let mut out = String::from("floating_modifier Mod4 normal\n");
    for line in modes.remove("").unwrap_or_default() {
        let _ = writeln!(out, "{line}");
    }
    for (name, key) in &entered {
        let lines = modes.entry(name.clone()).or_default();
        if !lines.iter().any(|l| l.ends_with("mode default")) {
            lines.push(format!("bindsym {key} mode default"));
        }
    }
    for (name, lines) in modes {
        let _ = writeln!(out, "mode \"{name}\" {{");
        for line in lines {
            let _ = writeln!(out, "    {line}");
        }
        let _ = writeln!(out, "}}");
    }
    out
}

/// A sway command for a Hyprland dispatcher, or None when sway has no match.
fn translate(dispatcher: &str, arg: &str) -> Option<String> {
    let direction = |a: &str| match a {
        "l" | "left" => Some("left"),
        "r" | "right" => Some("right"),
        "u" | "up" | "t" => Some("up"),
        "d" | "down" | "b" => Some("down"),
        _ => None,
    };
    Some(match dispatcher {
        "fullscreen" => "fullscreen toggle".to_string(),
        "killactive" => "kill".to_string(),
        "togglefloating" => "floating toggle".to_string(),
        "movefocus" => format!("focus {}", direction(arg)?),
        "movewindow" => format!("move {}", direction(arg)?),
        "workspace" => workspace_target(arg)?,
        "movetoworkspace" | "movetoworkspacesilent" => {
            format!("move container to {}", workspace_target(arg)?)
        }
        "resizeactive" => {
            let mut parts = arg.split_whitespace().filter_map(|p| p.parse::<i32>().ok());
            let (dx, dy) = (parts.next()?, parts.next().unwrap_or(0));
            let pick = |d: i32, axis: &str| {
                format!(
                    "resize {} {axis} {}px",
                    if d >= 0 { "grow" } else { "shrink" },
                    d.abs()
                )
            };
            if dx != 0 {
                pick(dx, "width")
            } else {
                pick(dy, "height")
            }
        }
        _ => return None,
    })
}

fn workspace_target(arg: &str) -> Option<String> {
    match arg {
        "e+1" | "r+1" | "+1" | "m+1" => Some("workspace next".to_string()),
        "e-1" | "r-1" | "-1" | "m-1" => Some("workspace prev".to_string()),
        n if n.parse::<u32>().is_ok() => Some(format!("workspace number {n}")),
        _ => None,
    }
}

/// Write an exec bind's command to a script, or None when it would reach the host.
/// The script puts the desktop's shims first on PATH.
fn exec_script(dir: &Path, bin: &Path, index: usize, command: &str) -> Option<std::path::PathBuf> {
    let command = command
        .strip_prefix("uwsm-app -- ")
        .unwrap_or(command)
        .replace("; uwsm-app -- ", "; ");
    if command.is_empty() || HOST_ONLY.iter().any(|word| command.contains(word)) {
        return None;
    }
    std::fs::create_dir_all(dir).ok()?;
    let path = dir.join(format!("{index}.sh"));
    let path_line = format!(
        "export PATH={}:\"$PATH\"",
        shell_quote(&bin.to_string_lossy())
    );
    std::fs::write(&path, format!("#!/bin/sh\n{path_line}\n{command}\n")).ok()?;
    Some(path)
}

/// Sway's `Mods+key` for a Hyprland modmask and key, or None for keys sway names differently.
fn sway_keys(modmask: u64, key: &str) -> Option<String> {
    if key.is_empty() || key.starts_with("code:") || key.starts_with("mouse") {
        return None;
    }
    let key = match key {
        "SPACE" | "space" => "space".to_string(),
        "RETURN" | "Return" | "return" => "Return".to_string(),
        "TAB" | "Tab" | "tab" => "Tab".to_string(),
        "ESCAPE" | "Escape" | "escape" => "Escape".to_string(),
        "left" | "Left" => "Left".to_string(),
        "right" | "Right" => "Right".to_string(),
        "up" | "Up" => "Up".to_string(),
        "down" | "Down" => "Down".to_string(),
        single if single.chars().count() == 1 => single.to_lowercase(),
        other => other.to_string(),
    };
    let mut parts = Vec::new();
    for (bit, name) in [(64, "Mod4"), (8, "Mod1"), (4, "Ctrl"), (1, "Shift")] {
        if modmask & bit != 0 {
            parts.push(name);
        }
    }
    parts.push(&key);
    Some(parts.join("+"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hyprland_colours_become_sway_hex() {
        assert_eq!(argb_to_hex("ffeeb7d1").as_deref(), Some("#eeb7d1"));
        assert_eq!(argb_to_hex("0xff5b5356").as_deref(), Some("#5b5356"));
        assert_eq!(argb_to_hex("rgba(33ccffee)").as_deref(), Some("#ccffee"));
        assert_eq!(argb_to_hex("oops"), None);
    }

    #[test]
    fn super_a_becomes_fullscreen() {
        assert_eq!(sway_keys(64, "A").as_deref(), Some("Mod4+a"));
        assert_eq!(sway_keys(65, "A").as_deref(), Some("Mod4+Shift+a"));
        assert_eq!(
            translate("fullscreen", "0").as_deref(),
            Some("fullscreen toggle")
        );
    }

    #[test]
    fn directions_workspaces_and_resize_translate() {
        assert_eq!(translate("movefocus", "l").as_deref(), Some("focus left"));
        assert_eq!(
            translate("workspace", "3").as_deref(),
            Some("workspace number 3")
        );
        assert_eq!(
            translate("movetoworkspace", "e+1").as_deref(),
            Some("move container to workspace next")
        );
        assert_eq!(
            translate("resizeactive", "-30 0").as_deref(),
            Some("resize shrink width 30px")
        );
        assert_eq!(translate("pin", ""), None);
    }

    #[test]
    fn execs_run_with_shims_and_only_session_power_is_skipped() {
        let dir = std::env::temp_dir().join(format!("cul-binds-test-{}", std::process::id()));
        let bin = dir.join("bin");
        assert!(exec_script(&dir, &bin, 0, "systemctl suspend").is_none());
        assert!(exec_script(&dir, &bin, 1, "loginctl lock-session").is_none());
        let path = exec_script(&dir, &bin, 2, "caelestia shell drawers toggle launcher").unwrap();
        let script = std::fs::read_to_string(path).unwrap();
        assert!(script.contains(&format!("export PATH='{}':", bin.display())));
        assert!(script.ends_with("caelestia shell drawers toggle launcher\n"));
        let path = exec_script(&dir, &bin, 3, "uwsm-app -- kitty").unwrap();
        assert!(std::fs::read_to_string(path)
            .unwrap()
            .ends_with("\nkitty\n"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn every_bind_maps_and_submaps_become_modes() {
        let dir = std::env::temp_dir().join(format!("cul-binds-map-{}", std::process::id()));
        let bind = |modmask: u64, key: &str, dispatcher: &str, arg: &str, submap: &str| {
            serde_json::json!({"modmask": modmask, "key": key, "dispatcher": dispatcher,
                "arg": arg, "submap": submap, "mouse": false, "locked": false,
                "release": false, "repeat": key == "Up"})
        };
        let binds = vec![
            bind(
                8,
                "SPACE",
                "exec",
                "caelestia shell drawers toggle launcher",
                "",
            ),
            bind(64, "Z", "togglespecialworkspace", "magic", ""),
            bind(64, "X", "pin", "", ""),
            bind(
                8,
                "6",
                "exec",
                "notify-send on; hyprctl dispatch submap passthrough",
                "",
            ),
            bind(0, "F24", "submap", "reset", "agentdesk-lock"),
            bind(64, "Up", "movefocus", "u", ""),
        ];
        let config = translate_binds(&binds, &dir.join("binds"), &dir.join("bin"));
        assert!(config.contains("bindsym --no-repeat Mod1+space exec sh "));
        assert!(config.contains("bindsym --no-repeat Mod4+z exec sh "));
        assert!(config.contains("bindsym --no-repeat Mod4+x exec sh "));
        assert!(config.contains("bindsym Mod4+Up focus up"));
        assert!(config.contains("mode \"passthrough\" {\n    bindsym Mod1+6 mode default\n}"));
        assert!(config
            .contains("mode \"agentdesk-lock\" {\n    bindsym --no-repeat F24 mode default\n}"));
        let shim = std::fs::read_to_string(dir.join("binds/1.sh")).unwrap();
        assert!(shim.ends_with("hyprctl dispatch togglespecialworkspace 'magic'\n"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
