//! Keybinds for an agent desktop's sway, taken from the user's Hyprland.
//!
//! With the key lock on, Super and Alt combos reach the desktop, so the desktop
//! needs binds for them. Each desktop reads the live Hyprland binds and writes the
//! ones that have a sway equivalent. Fullscreen, focus, move, resize, workspaces
//! and floating translate directly. An exec bind runs on the desktop. Its command
//! goes into a script file, so no shell quoting passes through the sway config.
//!
//! Some commands reach the host, not the desktop, so the translation skips them.
//! The uwsm launcher is one: it starts apps in the host session, so its
//! `uwsm-app --` prefix is dropped and the command runs directly. The Caelestia
//! CLI, `hyprctl`, `hyprshade`, session controls and process killers such as
//! `pkill` are skipped, since those would act on the host.
//!
//! `CUL_DESKTOP_BINDS=default` uses a small built-in set, and `off` writes none.

use serde_json::Value;
use std::fmt::Write as _;
use std::path::Path;

/// Commands that act on the host session, not the desktop.
const HOST_ONLY: &[&str] = &[
    "hyprctl",
    "hyprshade",
    "hyprlock",
    "caelestia",
    "uwsm ",
    "systemctl",
    "loginctl",
    "dpms",
    "wlogout",
    "notify-send",
    "pkill",
    "killall",
];

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
    let scripts = dir.join("binds");
    let mut out = String::from("floating_modifier Mod4 normal\n");
    let mut seen = std::collections::HashSet::new();
    let mut count = 0;
    for bind in &binds {
        if bind["submap"]
            .as_str()
            .is_some_and(|submap| !submap.is_empty())
            || bind["mouse"] == true
        {
            continue;
        }
        let Some(keys) = sway_keys(
            bind["modmask"].as_u64().unwrap_or(0),
            bind["key"].as_str().unwrap_or(""),
        ) else {
            continue;
        };
        if !seen.insert(keys.clone()) {
            continue;
        }
        let arg = bind["arg"].as_str().unwrap_or("").trim();
        let command = match bind["dispatcher"].as_str().unwrap_or("") {
            "exec" => match exec_script(&scripts, count, arg) {
                Some(path) => format!("exec sh {}", path.display()),
                None => continue,
            },
            dispatcher => match translate(dispatcher, arg) {
                Some(command) => command,
                None => continue,
            },
        };
        let _ = writeln!(out, "bindsym {keys} {command}");
        count += 1;
    }
    Some(out)
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
fn exec_script(dir: &Path, index: usize, command: &str) -> Option<std::path::PathBuf> {
    let command = command
        .strip_prefix("uwsm-app -- ")
        .unwrap_or(command)
        .replace("; uwsm-app -- ", "; ");
    if command.is_empty() || HOST_ONLY.iter().any(|word| command.contains(word)) {
        return None;
    }
    std::fs::create_dir_all(dir).ok()?;
    let path = dir.join(format!("{index}.sh"));
    std::fs::write(&path, format!("#!/bin/sh\n{command}\n")).ok()?;
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
    fn host_only_execs_are_skipped_and_uwsm_is_dropped() {
        let dir = std::env::temp_dir().join(format!("cul-binds-test-{}", std::process::id()));
        assert!(exec_script(&dir, 0, "hyprctl dispatch exit").is_none());
        assert!(exec_script(&dir, 1, "caelestia shell drawers toggle dashboard").is_none());
        let path = exec_script(&dir, 2, "uwsm-app -- kitty").unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "#!/bin/sh\nkitty\n");
        let _ = std::fs::remove_dir_all(dir);
    }
}
