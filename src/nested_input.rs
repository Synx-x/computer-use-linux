//! Input for a nested Wayland compositor, such as a headless sway.
//!
//! ydotool writes to `/dev/uinput`, so its events reach the host seat and
//! never the nested compositor. With `CUL_NESTED=1` set, every pointer and
//! keyboard tool goes through this module instead. It talks to the compositor
//! on `WAYLAND_DISPLAY` through `zwlr_virtual_pointer_v1` and
//! `zwp_virtual_keyboard_v1`.
//!
//! The process holds one pointer and one keyboard open for its whole life.
//! A headless seat drops a device that appears for one event and goes away,
//! so a one-shot device per call does not work.
//!
//! Chords use a US keymap, compiled by xkbcommon when `xkbcli` is present and
//! taken from the bundled copy otherwise. Text uses a second keymap built per
//! call, one keycode per distinct character, the way wtype does it. That
//! types any character with a keysym, whatever the layout. The US keymap is
//! restored after every call.

use anyhow::{anyhow, bail, Context, Result};
use std::io::Write;
use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_output, wl_pointer, wl_registry, wl_seat};
use wayland_client::{delegate_noop, Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};
use xkeysym::Keysym;

pub(crate) const NESTED_ENV: &str = "CUL_NESTED";

const BUNDLED_US_KEYMAP: &str = include_str!("nested_us_keymap.xkb");
const KEYMAP_FORMAT_XKB_V1: u32 = 1;
const KEY_PRESSED: u32 = 1;
const KEY_RELEASED: u32 = 0;
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
/// Keycodes 9..=255 are free in a generated keymap. Leave a little room.
const TYPING_KEYMAP_SLOTS: usize = 240;
const WHEEL_STEP: f64 = 15.0;
/// Environment keys that point at the host session. A nested process must
/// never reach the host compositor or X server through them.
pub(crate) const HOST_ONLY_ENV_KEYS: &[&str] = &[
    "HYPRLAND_INSTANCE_SIGNATURE",
    "DISPLAY",
    "XAUTHORITY",
    "I3SOCK",
    "NIRI_SOCKET",
];

/// True when `CUL_NESTED=1`. Every input tool then targets the nested
/// compositor on `WAYLAND_DISPLAY` and nothing else.
pub fn enabled() -> bool {
    std::env::var(NESTED_ENV).is_ok_and(|value| value.trim() == "1")
}

/// Strip the host session keys from this process. Run once at startup in
/// nested mode, before any backend probe reads them.
pub(crate) fn isolate_environment() {
    for key in HOST_ONLY_ENV_KEYS {
        std::env::remove_var(key);
    }
    if std::env::var_os("SWAYSOCK").is_some() {
        std::env::set_var("XDG_CURRENT_DESKTOP", "sway");
    }
    std::env::set_var("XDG_SESSION_TYPE", "wayland");
}

/// True when process `pid` runs on this nested session: its
/// `WAYLAND_DISPLAY`, and `SWAYSOCK` when this process has one, match ours.
/// AT-SPI shares one bus across the host and every nested desktop, so this
/// check is what keeps an accessibility action off host apps.
pub(crate) fn owns_pid(pid: u32) -> bool {
    let Ok(environ) = std::fs::read(format!("/proc/{pid}/environ")) else {
        return false;
    };
    let lookup = |key: &str| {
        let prefix = format!("{key}=");
        environ
            .split(|byte| *byte == 0)
            .find_map(|entry| entry.strip_prefix(prefix.as_bytes()))
            .map(|value| String::from_utf8_lossy(value).into_owned())
    };
    let ours = |key: &str| std::env::var(key).ok().filter(|value| !value.is_empty());
    let Some(display) = ours("WAYLAND_DISPLAY") else {
        return false;
    };
    if lookup("WAYLAND_DISPLAY").as_deref() != Some(display.as_str()) {
        return false;
    }
    match ours("SWAYSOCK") {
        Some(socket) => lookup("SWAYSOCK").as_deref() == Some(socket.as_str()),
        None => true,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

pub fn button_code(name: Option<&str>) -> Option<u32> {
    match name.unwrap_or("left").to_ascii_lowercase().as_str() {
        "left" => Some(BTN_LEFT),
        "right" => Some(BTN_RIGHT),
        "middle" => Some(BTN_MIDDLE),
        _ => None,
    }
}

static DEVICES: Mutex<Option<NestedInput>> = Mutex::new(None);

/// Run `f` against the held devices, opening them on first use. A failed
/// call drops the devices so the next call reconnects.
fn with_devices<T>(f: impl FnOnce(&mut NestedInput) -> Result<T>) -> Result<T> {
    let mut guard = DEVICES
        .lock()
        .map_err(|_| anyhow!("nested input lock poisoned"))?;
    if guard.is_none() {
        *guard = Some(NestedInput::connect()?);
    }
    let devices = guard.as_mut().expect("devices were just opened");
    let result = f(devices);
    if result.is_err() {
        *guard = None;
    }
    result
}

pub fn layout_size() -> Result<(u32, u32)> {
    with_devices(|devices| Ok(devices.extent()))
}

pub fn click(x: i32, y: i32, button: u32, count: u32) -> Result<(i32, i32)> {
    with_devices(|devices| {
        let landing = devices.move_to(x, y)?;
        sleep_ms(20);
        for index in 0..count.max(1) {
            if index > 0 {
                sleep_ms(60);
            }
            devices.button(button, true)?;
            sleep_ms(30);
            devices.button(button, false)?;
        }
        Ok(landing)
    })
}

pub fn scroll(point: Option<(i32, i32)>, direction: ScrollDirection, notches: i32) -> Result<()> {
    with_devices(|devices| {
        if let Some((x, y)) = point {
            devices.move_to(x, y)?;
            sleep_ms(20);
        }
        devices.wheel(direction, notches.max(1))
    })
}

pub fn drag(start: (i32, i32), end: (i32, i32)) -> Result<()> {
    with_devices(|devices| {
        devices.move_to(start.0, start.1)?;
        sleep_ms(30);
        devices.button(BTN_LEFT, true)?;
        let steps = 12;
        for step in 1..=steps {
            sleep_ms(16);
            let x = start.0 + (end.0 - start.0) * step / steps;
            let y = start.1 + (end.1 - start.1) * step / steps;
            devices.move_to(x, y)?;
        }
        sleep_ms(30);
        devices.button(BTN_LEFT, false)
    })
}

/// Press `modifiers` in order, tap `key`, then release the modifiers in
/// reverse. Codes are evdev codes on the US keymap.
pub fn key_chord(modifiers: &[u16], key: u16) -> Result<()> {
    with_devices(|devices| {
        devices.ensure_base_keymap()?;
        for modifier in modifiers {
            devices.key(u32::from(*modifier), true)?;
            sleep_ms(10);
        }
        devices.key(u32::from(key), true)?;
        sleep_ms(20);
        devices.key(u32::from(key), false)?;
        for modifier in modifiers.iter().rev() {
            sleep_ms(10);
            devices.key(u32::from(*modifier), false)?;
        }
        Ok(())
    })
}

pub fn type_text(text: &str) -> Result<()> {
    let keysyms = text
        .chars()
        .map(keysym_for_char)
        .collect::<Result<Vec<_>>>()?;
    with_devices(|devices| {
        let result = devices.type_keysyms(&keysyms);
        let restored = devices.ensure_base_keymap();
        result.and(restored)
    })
}

fn keysym_for_char(ch: char) -> Result<u32> {
    let keysym = match ch {
        '\n' | '\r' => Keysym::Return,
        '\t' => Keysym::Tab,
        '\u{8}' => Keysym::BackSpace,
        '\u{1b}' => Keysym::Escape,
        other => Keysym::from_char(other),
    };
    if keysym == Keysym::NoSymbol {
        bail!(
            "character U+{:04X} has no keysym and cannot be typed",
            ch as u32
        );
    }
    Ok(keysym.raw())
}

/// One keymap that maps keycode 9 + i to `keysyms[i]`. Numeric keysyms
/// avoid name lookups. xkbcommon reads any value of 10 or more as a raw
/// keysym, and every printable keysym is at least 0x20.
pub(crate) fn typing_keymap(keysyms: &[u32]) -> String {
    let mut keymap = String::from("xkb_keymap {\nxkb_keycodes \"cul\" {\n");
    keymap.push_str(&format!(
        "minimum = 8;\nmaximum = {};\n",
        8 + keysyms.len().max(1)
    ));
    for index in 0..keysyms.len() {
        keymap.push_str(&format!("<K{index}> = {};\n", index + 9));
    }
    keymap.push_str("};\nxkb_types \"cul\" { include \"complete\" };\n");
    keymap.push_str("xkb_compatibility \"cul\" { include \"complete\" };\n");
    keymap.push_str("xkb_symbols \"cul\" {\n");
    for (index, keysym) in keysyms.iter().enumerate() {
        keymap.push_str(&format!("key <K{index}> {{[ 0x{keysym:08x} ]}};\n"));
    }
    keymap.push_str("};\n};\n");
    keymap
}

fn base_keymap() -> String {
    let compiled = std::process::Command::new("xkbcli")
        .args(["compile-keymap", "--layout", "us", "--output-format", "1"])
        .stderr(std::process::Stdio::null())
        .output();
    match compiled {
        Ok(output) if output.status.success() && output.stdout.starts_with(b"xkb_keymap") => {
            String::from_utf8_lossy(&output.stdout).into_owned()
        }
        _ => BUNDLED_US_KEYMAP.to_string(),
    }
}

/// The real-modifier bit an evdev modifier key sets in the US keymap.
fn modifier_mask(code: u32) -> u32 {
    match code {
        42 | 54 => 1 << 0,   // Shift
        29 | 97 => 1 << 2,   // Control
        56 | 100 => 1 << 3,  // Mod1 (Alt)
        125 | 126 => 1 << 6, // Mod4 (Super)
        _ => 0,
    }
}

fn sleep_ms(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

#[derive(Default)]
struct OutputGeometry {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    scale: i32,
}

#[derive(Default)]
struct State {
    outputs: Vec<OutputGeometry>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActiveKeymap {
    Base,
    Typing,
}

struct NestedInput {
    connection: Connection,
    queue: EventQueue<State>,
    state: State,
    pointer: ZwlrVirtualPointerV1,
    keyboard: ZwpVirtualKeyboardV1,
    epoch: Instant,
    active_keymap: ActiveKeymap,
    base_keymap: String,
    depressed_modifiers: u32,
}

impl NestedInput {
    fn connect() -> Result<Self> {
        let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
        if display.trim().is_empty() {
            bail!("CUL_NESTED=1 needs WAYLAND_DISPLAY to name the nested compositor");
        }
        let connection = Connection::connect_to_env()
            .with_context(|| format!("failed to connect to Wayland display {display}"))?;
        let (globals, mut queue) = registry_queue_init::<State>(&connection)
            .context("failed to read the Wayland registry")?;
        let qh = queue.handle();
        let seat: wl_seat::WlSeat = globals
            .bind(&qh, 1..=7, ())
            .context("nested compositor has no wl_seat")?;
        let pointer_manager: ZwlrVirtualPointerManagerV1 = globals
            .bind(&qh, 1..=2, ())
            .context("nested compositor lacks zwlr_virtual_pointer_manager_v1")?;
        let keyboard_manager: ZwpVirtualKeyboardManagerV1 = globals
            .bind(&qh, 1..=1, ())
            .context("nested compositor lacks zwp_virtual_keyboard_manager_v1")?;
        let mut state = State::default();
        for global in globals.contents().clone_list() {
            if global.interface == "wl_output" {
                let index = state.outputs.len();
                state.outputs.push(OutputGeometry {
                    scale: 1,
                    ..OutputGeometry::default()
                });
                let _: wl_output::WlOutput =
                    globals
                        .registry()
                        .bind(global.name, global.version.min(4), &qh, index);
            }
        }
        let pointer = pointer_manager.create_virtual_pointer(Some(&seat), &qh, ());
        let keyboard = keyboard_manager.create_virtual_keyboard(&seat, &qh, ());
        queue
            .roundtrip(&mut state)
            .context("Wayland roundtrip failed")?;
        let mut devices = Self {
            connection,
            queue,
            state,
            pointer,
            keyboard,
            epoch: Instant::now(),
            active_keymap: ActiveKeymap::Typing,
            base_keymap: base_keymap(),
            depressed_modifiers: 0,
        };
        devices.ensure_base_keymap()?;
        Ok(devices)
    }

    fn time(&self) -> u32 {
        self.epoch.elapsed().as_millis() as u32
    }

    fn flush(&mut self) -> Result<()> {
        self.connection
            .flush()
            .context("failed to flush nested Wayland input")?;
        Ok(())
    }

    /// The layout bounding box. Absolute motion maps onto it.
    fn bounds(&self) -> (i32, i32, i32, i32) {
        let mut min_x = i32::MAX;
        let mut min_y = i32::MAX;
        let mut max_x = i32::MIN;
        let mut max_y = i32::MIN;
        for output in &self.state.outputs {
            if output.width <= 0 || output.height <= 0 {
                continue;
            }
            let scale = output.scale.max(1);
            min_x = min_x.min(output.x);
            min_y = min_y.min(output.y);
            max_x = max_x.max(output.x + output.width / scale);
            max_y = max_y.max(output.y + output.height / scale);
        }
        if min_x > max_x || min_y > max_y {
            return (0, 0, 1920, 1080);
        }
        (min_x, min_y, max_x - min_x, max_y - min_y)
    }

    fn extent(&self) -> (u32, u32) {
        let (_, _, width, height) = self.bounds();
        (width.max(1) as u32, height.max(1) as u32)
    }

    /// Move to layout point (x, y), clamped to the layout. Returns the point
    /// that was emitted.
    fn move_to(&mut self, x: i32, y: i32) -> Result<(i32, i32)> {
        let (origin_x, origin_y, width, height) = self.bounds();
        let local_x = (x - origin_x).clamp(0, width.max(1) - 1);
        let local_y = (y - origin_y).clamp(0, height.max(1) - 1);
        let time = self.time();
        self.pointer.motion_absolute(
            time,
            local_x as u32,
            local_y as u32,
            width.max(1) as u32,
            height.max(1) as u32,
        );
        self.pointer.frame();
        self.flush()?;
        Ok((local_x + origin_x, local_y + origin_y))
    }

    fn button(&mut self, button: u32, pressed: bool) -> Result<()> {
        let state = if pressed {
            wl_pointer::ButtonState::Pressed
        } else {
            wl_pointer::ButtonState::Released
        };
        let time = self.time();
        self.pointer.button(time, button, state);
        self.pointer.frame();
        self.flush()
    }

    fn wheel(&mut self, direction: ScrollDirection, notches: i32) -> Result<()> {
        let (axis, sign) = match direction {
            ScrollDirection::Down => (wl_pointer::Axis::VerticalScroll, 1),
            ScrollDirection::Up => (wl_pointer::Axis::VerticalScroll, -1),
            ScrollDirection::Right => (wl_pointer::Axis::HorizontalScroll, 1),
            ScrollDirection::Left => (wl_pointer::Axis::HorizontalScroll, -1),
        };
        for _ in 0..notches {
            let time = self.time();
            self.pointer.axis_source(wl_pointer::AxisSource::Wheel);
            self.pointer
                .axis_discrete(time, axis, WHEEL_STEP * f64::from(sign), sign);
            self.pointer.frame();
            self.flush()?;
            sleep_ms(12);
        }
        Ok(())
    }

    fn key(&mut self, code: u32, pressed: bool) -> Result<()> {
        let time = self.time();
        let state = if pressed { KEY_PRESSED } else { KEY_RELEASED };
        self.keyboard.key(time, code, state);
        let mask = modifier_mask(code);
        if mask != 0 && self.active_keymap == ActiveKeymap::Base {
            if pressed {
                self.depressed_modifiers |= mask;
            } else {
                self.depressed_modifiers &= !mask;
            }
            self.keyboard.modifiers(self.depressed_modifiers, 0, 0, 0);
        }
        self.flush()
    }

    fn upload_keymap(&mut self, keymap: &str) -> Result<()> {
        let fd = keymap_fd(keymap)?;
        self.keyboard
            .keymap(KEYMAP_FORMAT_XKB_V1, fd.as_fd(), keymap.len() as u32 + 1);
        self.keyboard.modifiers(0, 0, 0, 0);
        self.depressed_modifiers = 0;
        // The compositor receives the fd at flush. Keep it open until the
        // roundtrip confirms the keymap request was read.
        self.flush()?;
        self.queue
            .roundtrip(&mut self.state)
            .context("Wayland roundtrip after a keymap change failed")?;
        drop(fd);
        // Clients apply the new keymap on their own event loop.
        sleep_ms(25);
        Ok(())
    }

    fn ensure_base_keymap(&mut self) -> Result<()> {
        if self.active_keymap == ActiveKeymap::Base {
            return Ok(());
        }
        let keymap = self.base_keymap.clone();
        self.upload_keymap(&keymap)?;
        self.active_keymap = ActiveKeymap::Base;
        Ok(())
    }

    fn type_keysyms(&mut self, keysyms: &[u32]) -> Result<()> {
        let mut start = 0;
        while start < keysyms.len() {
            // Take the longest run whose distinct keysyms fit in one keymap.
            let mut distinct: Vec<u32> = Vec::new();
            let mut end = start;
            while end < keysyms.len() {
                if !distinct.contains(&keysyms[end]) {
                    if distinct.len() == TYPING_KEYMAP_SLOTS {
                        break;
                    }
                    distinct.push(keysyms[end]);
                }
                end += 1;
            }
            self.upload_keymap(&typing_keymap(&distinct))?;
            self.active_keymap = ActiveKeymap::Typing;
            for keysym in &keysyms[start..end] {
                let index = distinct
                    .iter()
                    .position(|candidate| candidate == keysym)
                    .expect("every keysym in the run has a slot");
                // Wayland key codes are evdev codes, which XKB offsets by 8.
                let code = index as u32 + 1;
                self.key(code, true)?;
                sleep_ms(4);
                self.key(code, false)?;
                sleep_ms(4);
            }
            start = end;
        }
        Ok(())
    }
}

fn keymap_fd(keymap: &str) -> Result<OwnedFd> {
    // SAFETY: the name is a valid NUL-terminated string and the flags are
    // plain constants. A negative return is checked before the fd is used.
    let raw = unsafe { libc::memfd_create(c"cul-keymap".as_ptr(), libc::MFD_CLOEXEC) };
    if raw < 0 {
        return Err(std::io::Error::last_os_error()).context("memfd_create failed");
    }
    // SAFETY: memfd_create returned a fresh fd that nothing else owns.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut file = std::fs::File::from(fd.try_clone().context("failed to clone keymap fd")?);
    file.write_all(keymap.as_bytes())
        .and_then(|()| file.write_all(&[0]))
        .context("failed to write the keymap")?;
    Ok(fd)
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_output::WlOutput, usize> for State {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(output) = state.outputs.get_mut(*index) else {
            return;
        };
        match event {
            wl_output::Event::Geometry { x, y, .. } => {
                output.x = x;
                output.y = y;
            }
            wl_output::Event::Mode {
                flags: WEnum::Value(flags),
                width,
                height,
                ..
            } if flags.contains(wl_output::Mode::Current) => {
                output.width = width;
                output.height = height;
            }
            wl_output::Event::Scale { factor } => output.scale = factor,
            _ => {}
        }
    }
}

delegate_noop!(State: ignore wl_seat::WlSeat);
delegate_noop!(State: ZwlrVirtualPointerManagerV1);
delegate_noop!(State: ZwlrVirtualPointerV1);
delegate_noop!(State: ZwpVirtualKeyboardManagerV1);
delegate_noop!(State: ZwpVirtualKeyboardV1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_keymap_maps_each_keysym_to_its_own_keycode() {
        let keymap = typing_keymap(&[0x41, 0x0100263a]);
        assert!(keymap.contains("<K0> = 9;"));
        assert!(keymap.contains("<K1> = 10;"));
        assert!(keymap.contains("maximum = 10;"));
        assert!(keymap.contains("key <K1> {[ 0x0100263a ]};"));
    }

    #[test]
    fn control_characters_map_to_their_function_keysyms() {
        assert_eq!(keysym_for_char('\n').unwrap(), Keysym::Return.raw());
        assert_eq!(keysym_for_char('\t').unwrap(), Keysym::Tab.raw());
        assert_eq!(keysym_for_char('a').unwrap(), 0x61);
        assert_eq!(keysym_for_char('☺').unwrap(), 0x0100263a);
    }

    #[test]
    fn modifier_masks_match_the_us_keymap_real_modifiers() {
        assert_eq!(modifier_mask(42), 1);
        assert_eq!(modifier_mask(29), 4);
        assert_eq!(modifier_mask(56), 8);
        assert_eq!(modifier_mask(125), 64);
        assert_eq!(modifier_mask(30), 0);
    }

    #[test]
    fn bundled_keymap_is_a_complete_xkb_keymap() {
        assert!(BUNDLED_US_KEYMAP.starts_with("xkb_keymap"));
        assert!(BUNDLED_US_KEYMAP.contains("xkb_symbols"));
    }

    #[test]
    fn unknown_buttons_are_refused() {
        assert_eq!(button_code(None), Some(BTN_LEFT));
        assert_eq!(button_code(Some("side")), None);
    }
}
