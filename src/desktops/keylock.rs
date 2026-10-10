//! The key passthrough lock.
//!
//! Hyprland handles its own binds before any app sees the key, so Super combos
//! never reach the browser. Locking moves Hyprland into a submap with no usable binds,
//! so every key reaches the browser and, through the live view, the desktop.
//! Hyprland 0.56 refuses to enter a submap that was never registered, and it only
//! registers one that holds a bind. So the submap is registered at runtime with a
//! single bind on F24, a key no keyboard sends. `hyprctl reload` drops runtime
//! keywords, so registration runs again before every lock.
//!
//! A stuck submap leaves the host without its binds, so every exit path resets it.

use anyhow::{bail, Context, Result};

/// The submap name. Its only bind is on F24, which no keyboard sends.
pub const SUBMAP: &str = "agentdesk-lock";

/// Register the submap if Hyprland lacks it, then enter it.
pub async fn enter() -> Result<()> {
    if !registered().await {
        let batch = format!(
            "keyword submap {SUBMAP} ; keyword bind ,F24,submap,reset ; keyword submap reset"
        );
        let output = tokio::process::Command::new("hyprctl")
            .args(["--batch", &batch])
            .output()
            .await
            .context("run hyprctl")?;
        if !output.status.success() {
            bail!("hyprctl could not register the {SUBMAP} submap");
        }
    }
    dispatch(&["dispatch", "submap", SUBMAP]).await
}

/// Whether Hyprland already holds a bind in our submap.
async fn registered() -> bool {
    let Ok(output) = tokio::process::Command::new("hyprctl")
        .args(["binds", "-j"])
        .output()
        .await
    else {
        return false;
    };
    serde_json::from_slice::<Vec<serde_json::Value>>(&output.stdout)
        .map(|binds| binds.iter().any(|bind| bind["submap"] == SUBMAP))
        .unwrap_or(false)
}

/// Leave any submap and return to the normal binds.
pub async fn reset() -> Result<()> {
    dispatch(&["dispatch", "submap", "reset"]).await
}

/// The submap Hyprland is in now, or None when hyprctl cannot answer.
pub async fn current() -> Option<String> {
    let output = tokio::process::Command::new("hyprctl")
        .arg("submap")
        .output()
        .await
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Reset only when Hyprland is still in our submap, so the user's own
/// submaps (game mode) are left alone.
pub async fn reset_if_ours() -> Result<bool> {
    if current().await.as_deref() == Some(SUBMAP) {
        reset().await?;
        return Ok(true);
    }
    Ok(false)
}

async fn dispatch(args: &[&str]) -> Result<()> {
    let output = tokio::process::Command::new("hyprctl")
        .args(args)
        .output()
        .await
        .context("run hyprctl")?;
    let text = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() || text.trim() != "ok" {
        bail!("hyprctl {} failed: {}", args.join(" "), text.trim());
    }
    Ok(())
}
