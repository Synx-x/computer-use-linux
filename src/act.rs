//! Helpers for the `act` tool: frame diffing and validated landmarks.
//!
//! `act` runs a batch of input steps against one window and reports what
//! changed, so an agent can act and verify in a single round trip. Landmarks
//! remember click targets across runs. Each one stores a small perceptual hash
//! of the pixels around it and is checked against the live frame before use,
//! so a moved window, a zoom change or a toggled panel fails the check instead
//! of producing a misclick.

use anyhow::{Context, Result};
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, GrayImage};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Side of a diff tile in pixels.
const TILE: u32 = 24;
/// Mean absolute luma difference above which a tile counts as changed.
const TILE_THRESHOLD: f64 = 4.0;
/// Padding added around the changed area before cropping it for the agent.
pub const DELTA_PADDING: u32 = 16;

/// Area of a frame that changed between two captures.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ChangedRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// Share of tiles that changed, from 0 to 1.
    pub fraction: f64,
}

/// Compare two frames of the same window. Returns None when nothing visibly
/// changed. A size change counts as a full-frame change.
pub fn changed_region(before: &DynamicImage, after: &DynamicImage) -> Option<ChangedRegion> {
    let (w, h) = after.dimensions();
    if before.dimensions() != (w, h) {
        return Some(ChangedRegion { x: 0, y: 0, width: w, height: h, fraction: 1.0 });
    }
    let a = before.to_luma8();
    let b = after.to_luma8();
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    let (mut changed, mut total) = (0u32, 0u32);
    for ty in (0..h).step_by(TILE as usize) {
        for tx in (0..w).step_by(TILE as usize) {
            total += 1;
            if tile_diff(&a, &b, tx, ty, w, h) > TILE_THRESHOLD {
                changed += 1;
                x0 = x0.min(tx);
                y0 = y0.min(ty);
                x1 = x1.max((tx + TILE).min(w));
                y1 = y1.max((ty + TILE).min(h));
            }
        }
    }
    (changed > 0).then(|| ChangedRegion {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
        fraction: f64::from(changed) / f64::from(total.max(1)),
    })
}

fn tile_diff(a: &GrayImage, b: &GrayImage, tx: u32, ty: u32, w: u32, h: u32) -> f64 {
    let (mut sum, mut count) = (0u64, 0u64);
    for y in ty..(ty + TILE).min(h) {
        for x in tx..(tx + TILE).min(w) {
            let da = i32::from(a.get_pixel(x, y)[0]);
            let db = i32::from(b.get_pixel(x, y)[0]);
            sum += da.abs_diff(db) as u64;
            count += 1;
        }
    }
    if count == 0 {
        0.0
    } else {
        sum as f64 / count as f64
    }
}

/// Grow a region by `DELTA_PADDING`, clamped to the frame.
pub fn padded(region: ChangedRegion, frame_w: u32, frame_h: u32) -> (u32, u32, u32, u32) {
    let x = region.x.saturating_sub(DELTA_PADDING);
    let y = region.y.saturating_sub(DELTA_PADDING);
    let right = (region.x + region.width + DELTA_PADDING).min(frame_w);
    let bottom = (region.y + region.height + DELTA_PADDING).min(frame_h);
    (x, y, right - x, bottom - y)
}

// ---- Landmarks ------------------------------------------------------------

/// Side of the square sampled around a landmark point.
const LANDMARK_SAMPLE: u32 = 48;
/// Side of the downscaled grid the hash is built from (16x16 = 256 bits).
const HASH_GRID: u32 = 16;
/// Maximum differing bits (out of 256) for a landmark to still match.
pub const HASH_TOLERANCE: u32 = 40;
/// Weight below which a landmark is refused until it is saved again.
pub const MIN_WEIGHT: f64 = 0.3;

/// Average hash of the square around (x, y), as a 64-char hex string.
pub fn landmark_hash(frame: &DynamicImage, x: u32, y: u32) -> Option<String> {
    let (w, h) = frame.dimensions();
    if x >= w || y >= h {
        return None;
    }
    let half = LANDMARK_SAMPLE / 2;
    let left = x.saturating_sub(half).min(w.saturating_sub(LANDMARK_SAMPLE.min(w)));
    let top = y.saturating_sub(half).min(h.saturating_sub(LANDMARK_SAMPLE.min(h)));
    let side_w = LANDMARK_SAMPLE.min(w - left);
    let side_h = LANDMARK_SAMPLE.min(h - top);
    let grid = frame
        .crop_imm(left, top, side_w, side_h)
        .resize_exact(HASH_GRID, HASH_GRID, FilterType::Triangle)
        .to_luma8();
    let mean = grid.pixels().map(|p| u32::from(p[0])).sum::<u32>() / (HASH_GRID * HASH_GRID);
    let mut bits = Vec::with_capacity((HASH_GRID * HASH_GRID / 8) as usize);
    let mut byte = 0u8;
    for (i, p) in grid.pixels().enumerate() {
        byte = (byte << 1) | u8::from(u32::from(p[0]) > mean);
        if i % 8 == 7 {
            bits.push(byte);
            byte = 0;
        }
    }
    Some(bits.iter().map(|b| format!("{b:02x}")).collect())
}

/// Number of differing bits between two hashes, or None if they don't parse.
pub fn hash_distance(a: &str, b: &str) -> Option<u32> {
    if a.len() != b.len() || a.len() % 2 != 0 {
        return None;
    }
    let mut distance = 0;
    for i in (0..a.len()).step_by(2) {
        let x = u8::from_str_radix(&a[i..i + 2], 16).ok()?;
        let y = u8::from_str_radix(&b[i..i + 2], 16).ok()?;
        distance += (x ^ y).count_ones();
    }
    Some(distance)
}

/// A remembered click target inside one app's window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Landmark {
    pub app: String,
    pub name: String,
    /// Window-relative point, in capture pixels.
    pub x: u32,
    pub y: u32,
    /// Window size when the landmark was saved. A resize invalidates it.
    pub window_width: u32,
    pub window_height: u32,
    pub hash: String,
    /// Confidence from 0 to 1: raised by passing checks, halved by failures.
    pub weight: f64,
    pub hits: u32,
    pub misses: u32,
    pub last_seen: u64,
}

/// Outcome of checking a landmark against the live frame.
#[derive(Debug, Clone, Serialize)]
pub struct LandmarkCheck {
    pub name: String,
    pub passed: bool,
    pub distance: Option<u32>,
    pub weight: f64,
    pub reason: String,
}

fn store_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))?;
    Some(base.join("computer-use-linux").join("landmarks.json"))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

fn key(app: &str, name: &str) -> String {
    format!("{app}::{name}")
}

pub fn load_landmarks() -> HashMap<String, Landmark> {
    store_path()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_landmarks(landmarks: &HashMap<String, Landmark>) -> Result<()> {
    let path = store_path().context("no HOME or XDG_STATE_HOME for the landmark store")?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).context("failed to create the landmark store directory")?;
    }
    let text = serde_json::to_string_pretty(landmarks).context("failed to serialize landmarks")?;
    fs::write(&path, text).with_context(|| format!("failed to write {}", path.display()))
}

/// Save or replace a landmark from the current frame.
pub fn save_landmark(app: &str, name: &str, frame: &DynamicImage, x: u32, y: u32) -> Result<Landmark> {
    let hash = landmark_hash(frame, x, y).context("landmark point lies outside the window")?;
    let (window_width, window_height) = frame.dimensions();
    let landmark = Landmark {
        app: app.to_string(),
        name: name.to_string(),
        x,
        y,
        window_width,
        window_height,
        hash,
        weight: 1.0,
        hits: 0,
        misses: 0,
        last_seen: now_secs(),
    };
    let mut landmarks = load_landmarks();
    landmarks.insert(key(app, name), landmark.clone());
    save_landmarks(&landmarks)?;
    Ok(landmark)
}

/// Check a landmark against the live frame and record the outcome. Returns
/// the landmark when the check passes, so the caller can click it.
pub fn check_landmark(app: &str, name: &str, frame: &DynamicImage) -> (Option<Landmark>, LandmarkCheck) {
    let mut landmarks = load_landmarks();
    let Some(landmark) = landmarks.get_mut(&key(app, name)) else {
        let check = LandmarkCheck {
            name: name.to_string(),
            passed: false,
            distance: None,
            weight: 0.0,
            reason: format!("no landmark named {name:?} for this app; save it first"),
        };
        return (None, check);
    };

    let (w, h) = frame.dimensions();
    let (passed, distance, reason) = if landmark.weight < MIN_WEIGHT {
        (false, None, "weight too low after repeated failures; save it again".to_string())
    } else if (w, h) != (landmark.window_width, landmark.window_height) {
        (
            false,
            None,
            format!(
                "window is {w}x{h}, landmark was saved at {}x{}",
                landmark.window_width, landmark.window_height
            ),
        )
    } else {
        let live = landmark_hash(frame, landmark.x, landmark.y);
        let distance = live.as_deref().and_then(|live| hash_distance(live, &landmark.hash));
        match distance {
            Some(d) if d <= HASH_TOLERANCE => (true, Some(d), "pixels match".to_string()),
            Some(d) => (false, Some(d), format!("pixels differ: {d} of 256 bits")),
            None => (false, None, "could not hash the live frame".to_string()),
        }
    };

    if passed {
        landmark.hits += 1;
        landmark.weight = (landmark.weight + 0.1).min(1.0);
        landmark.last_seen = now_secs();
    } else {
        landmark.misses += 1;
        landmark.weight *= 0.5;
    }
    let check = LandmarkCheck {
        name: name.to_string(),
        passed,
        distance,
        weight: landmark.weight,
        reason,
    };
    let result = passed.then(|| landmark.clone());
    let _ = save_landmarks(&landmarks);
    (result, check)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    fn solid(w: u32, h: u32, value: u8) -> DynamicImage {
        DynamicImage::ImageRgb8(RgbImage::from_pixel(w, h, Rgb([value, value, value])))
    }

    #[test]
    fn identical_frames_have_no_change() {
        assert_eq!(changed_region(&solid(100, 80, 40), &solid(100, 80, 40)), None);
    }

    #[test]
    fn changed_block_is_located() {
        let before = solid(120, 96, 0);
        let mut after = before.to_rgb8();
        for y in 30..50 {
            for x in 50..70 {
                after.put_pixel(x, y, Rgb([255, 255, 255]));
            }
        }
        let region = changed_region(&before, &DynamicImage::ImageRgb8(after)).unwrap();
        assert!(region.x <= 50 && region.x + region.width >= 70);
        assert!(region.y <= 30 && region.y + region.height >= 50);
        assert!(region.fraction > 0.0 && region.fraction < 1.0);
    }

    #[test]
    fn resize_counts_as_full_change() {
        let region = changed_region(&solid(100, 80, 0), &solid(90, 80, 0)).unwrap();
        assert_eq!((region.width, region.height, region.fraction), (90, 80, 1.0));
    }

    #[test]
    fn hash_matches_itself_and_detects_difference() {
        let mut img = solid(200, 200, 30).to_rgb8();
        for y in 90..110 {
            for x in 90..130 {
                img.put_pixel(x, y, Rgb([250, 250, 250]));
            }
        }
        let frame = DynamicImage::ImageRgb8(img);
        let a = landmark_hash(&frame, 100, 100).unwrap();
        assert_eq!(hash_distance(&a, &a), Some(0));
        let b = landmark_hash(&solid(200, 200, 30), 100, 100).unwrap();
        assert!(hash_distance(&a, &b).unwrap() > HASH_TOLERANCE);
    }
}
