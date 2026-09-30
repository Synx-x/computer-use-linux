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

/// Share of tiles that may still change while a window counts as quiet. It
/// absorbs ambient animation that the mask did not catch.
pub const QUIET_FRACTION: f64 = 0.01;

/// Tiles that change on their own (an animated background, a playing video),
/// in row-major tile order. Diffs skip them.
pub type AmbientMask = Vec<bool>;

/// Compare two frames of the same window. Returns None when nothing visibly
/// changed. A size change counts as a full-frame change.
#[cfg(test)]
pub fn changed_region(before: &DynamicImage, after: &DynamicImage) -> Option<ChangedRegion> {
    changed_region_masked(before, after, None)
}

/// Build an ambient mask from frames captured while no input was sent. A tile
/// that changes between any two consecutive frames is ambient, and so are its
/// neighbours, since animation drifts. Returns None when nothing moved.
pub fn ambient_mask(frames: &[DynamicImage]) -> Option<AmbientMask> {
    let first = frames.first()?;
    let (w, h) = first.dimensions();
    let (cols, rows) = (w.div_ceil(TILE), h.div_ceil(TILE));
    let mut hit = vec![false; (cols * rows) as usize];
    for pair in frames.windows(2) {
        if pair[1].dimensions() != (w, h) {
            return None;
        }
        let (a, b) = (pair[0].to_luma8(), pair[1].to_luma8());
        for row in 0..rows {
            for col in 0..cols {
                if tile_diff(&a, &b, col * TILE, row * TILE, w, h) > TILE_THRESHOLD {
                    hit[(row * cols + col) as usize] = true;
                }
            }
        }
    }
    if !hit.contains(&true) {
        return None;
    }
    let mut mask = hit.clone();
    for row in 0..rows as i64 {
        for col in 0..cols as i64 {
            if !hit[(row * cols as i64 + col) as usize] {
                continue;
            }
            for (dr, dc) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                let (r, c) = (row + dr, col + dc);
                if r >= 0 && c >= 0 && r < rows as i64 && c < cols as i64 {
                    mask[(r * cols as i64 + c) as usize] = true;
                }
            }
        }
    }
    Some(mask)
}

/// Like `changed_region`, but tiles set in `mask` never count as changed.
pub fn changed_region_masked(
    before: &DynamicImage,
    after: &DynamicImage,
    mask: Option<&[bool]>,
) -> Option<ChangedRegion> {
    let (w, h) = after.dimensions();
    if before.dimensions() != (w, h) {
        return Some(ChangedRegion { x: 0, y: 0, width: w, height: h, fraction: 1.0 });
    }
    let cols = w.div_ceil(TILE);
    let mask = mask.filter(|m| m.len() == (cols * h.div_ceil(TILE)) as usize);
    let a = before.to_luma8();
    let b = after.to_luma8();
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    let (mut changed, mut total) = (0u32, 0u32);
    for ty in (0..h).step_by(TILE as usize) {
        for tx in (0..w).step_by(TILE as usize) {
            total += 1;
            if mask.is_some_and(|m| m[((ty / TILE) * cols + tx / TILE) as usize]) {
                continue;
            }
            // An animated page also has slow glows and fades. Comparing tile
            // structure, not brightness, ignores those but keeps text and edges.
            let diff = if mask.is_some() {
                tile_structure_diff(&a, &b, tx, ty, w, h)
            } else {
                tile_diff(&a, &b, tx, ty, w, h)
            };
            if diff > TILE_THRESHOLD {
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

/// Mean absolute difference after removing each tile's mean brightness, so a
/// uniform fade or glow scores near zero.
fn tile_structure_diff(a: &GrayImage, b: &GrayImage, tx: u32, ty: u32, w: u32, h: u32) -> f64 {
    let (x1, y1) = ((tx + TILE).min(w), (ty + TILE).min(h));
    let count = f64::from((x1 - tx) * (y1 - ty));
    if count == 0.0 {
        return 0.0;
    }
    let (mut sa, mut sb) = (0.0, 0.0);
    for y in ty..y1 {
        for x in tx..x1 {
            sa += f64::from(a.get_pixel(x, y)[0]);
            sb += f64::from(b.get_pixel(x, y)[0]);
        }
    }
    let (ma, mb) = (sa / count, sb / count);
    let mut sum = 0.0;
    for y in ty..y1 {
        for x in tx..x1 {
            let da = f64::from(a.get_pixel(x, y)[0]) - ma;
            let db = f64::from(b.get_pixel(x, y)[0]) - mb;
            sum += (da - db).abs();
        }
    }
    sum / count
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

/// Landmark name that caches where a `click_text` label was last clicked.
pub fn text_landmark_name(text: &str) -> String {
    format!("text:{}", normalize(text))
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
    fn ambient_mask_hides_animated_tiles_only() {
        let still = solid(96, 96, 40);
        let mut moved = still.to_rgb8();
        for y in 0..10 {
            for x in 0..10 {
                moved.put_pixel(x, y, image::Rgb([255, 255, 255]));
            }
        }
        let moved = DynamicImage::ImageRgb8(moved);
        let mask = ambient_mask(&[still.clone(), moved.clone()]).unwrap();
        assert_eq!(changed_region_masked(&still, &moved, Some(&mask)), None);
        let mut far = still.to_rgb8();
        for y in 80..96 {
            for x in 80..96 {
                far.put_pixel(x, y, image::Rgb([255, 255, 255]));
            }
        }
        let far = DynamicImage::ImageRgb8(far);
        assert!(changed_region_masked(&still, &far, Some(&mask)).is_some());
        assert_eq!(ambient_mask(&[still.clone(), still.clone()]), None);
        // A uniform fade elsewhere is ignored once the page is known to animate.
        let faded = solid(96, 96, 60);
        let mut masked_fade = mask.clone();
        masked_fade.iter_mut().for_each(|t| *t = false);
        assert_eq!(changed_region_masked(&still, &faded, Some(&masked_fade)), None);
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

// ---- Text targets -------------------------------------------------------

/// A run of words on screen whose text matches a requested label.
#[derive(Debug, Clone, Serialize)]
pub struct TextMatch {
    pub text: String,
    /// Centre of the matched words, window-relative, in capture pixels.
    pub x: u32,
    pub y: u32,
    /// 1.0 for an exact match, lower for a contained or near match.
    pub score: f64,
}

/// Read words with Tesseract inside `region` of `frame`, then return every
/// run of words that matches `target`, best first.
pub fn find_text(frame: &DynamicImage, region: (u32, u32, u32, u32), target: &str) -> Result<Vec<TextMatch>> {
    let (rx, ry, rw, rh) = region;
    // Small UI text reads far better at twice the size.
    let crop = frame
        .crop_imm(rx, ry, rw, rh)
        .resize_exact(rw * 2, rh * 2, FilterType::Triangle)
        .to_luma8();
    let path = std::env::temp_dir().join(format!("cul-ocr-{}-{}.png", std::process::id(), now_secs()));
    crop.save(&path).context("failed to write the OCR crop")?;
    let output = std::process::Command::new("tesseract")
        .arg(&path)
        .args(["-", "--psm", "11", "tsv"])
        .env("OMP_THREAD_LIMIT", "1")
        .output();
    let _ = fs::remove_file(&path);
    let output = output.context("failed to run tesseract (install the tesseract package)")?;
    if !output.status.success() {
        anyhow::bail!("tesseract exited with {}", output.status);
    }

    // Group recognised words into lines, keyed by block, paragraph and line.
    let mut lines: Vec<((String, String, String), Vec<(String, u32, u32, u32, u32)>)> = Vec::new();
    for row in String::from_utf8_lossy(&output.stdout).lines().skip(1) {
        let cols: Vec<&str> = row.split('\t').collect();
        if cols.len() < 12 || cols[0] != "5" || cols[11].trim().is_empty() {
            continue;
        }
        let conf: f64 = cols[10].parse().unwrap_or(-1.0);
        if conf < 30.0 {
            continue;
        }
        let key = (cols[2].to_string(), cols[3].to_string(), cols[4].to_string());
        let nums: Vec<u32> = cols[6..10].iter().map(|v| v.parse().unwrap_or(0)).collect();
        let word = (cols[11].trim().to_string(), nums[0], nums[1], nums[2], nums[3]);
        match lines.iter_mut().find(|(k, _)| *k == key) {
            Some((_, words)) => words.push(word),
            None => lines.push((key, vec![word])),
        }
    }

    let want = normalize(target);
    let want_len = want.split(' ').count().max(1);
    let mut found: Vec<TextMatch> = Vec::new();
    for (_, words) in &lines {
        for start in 0..words.len() {
            for len in 1..=(want_len + 1).min(words.len() - start) {
                let run = &words[start..start + len];
                let text = run.iter().map(|w| w.0.as_str()).collect::<Vec<_>>().join(" ");
                let got = normalize(&text);
                let score = if got == want {
                    1.0
                } else if len == want_len && similarity(&got, &want) >= 0.85 {
                    similarity(&got, &want)
                } else {
                    continue;
                };
                let left = run.iter().map(|w| w.1).min().unwrap_or(0);
                let top = run.iter().map(|w| w.2).min().unwrap_or(0);
                let right = run.iter().map(|w| w.1 + w.3).max().unwrap_or(0);
                let bottom = run.iter().map(|w| w.2 + w.4).max().unwrap_or(0);
                found.push(TextMatch {
                    text,
                    x: rx + (left + right) / 4,
                    y: ry + (top + bottom) / 4,
                    score,
                });
            }
        }
    }
    found.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    // One screen spot per match: drop runs that land on an already-kept point.
    let mut kept: Vec<TextMatch> = Vec::new();
    for candidate in found {
        if !kept.iter().any(|k| k.x.abs_diff(candidate.x) < 12 && k.y.abs_diff(candidate.y) < 8) {
            kept.push(candidate);
        }
    }
    Ok(kept)
}

fn normalize(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_alphanumeric() || c == '.' { c.to_ascii_lowercase() } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Levenshtein similarity from 0 to 1.
fn similarity(a: &str, b: &str) -> f64 {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        prev = cur;
    }
    1.0 - prev[b.len()] as f64 / a.len().max(b.len()) as f64
}

#[cfg(test)]
mod text_tests {
    use super::*;

    #[test]
    fn normalize_ignores_case_and_punctuation() {
        assert_eq!(normalize("Gemini 3.1 Flash-Lite!"), "gemini 3.1 flash lite");
    }

    #[test]
    fn similarity_tolerates_one_ocr_slip() {
        assert!(similarity("gemini 3.1 flash lite", "gemini 3.1 flash 1ite") >= 0.85);
        assert!(similarity("usage", "billing") < 0.5);
    }
}
