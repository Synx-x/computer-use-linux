# Nested input

## Purpose

Nested input sends clicks, scrolls, drags, key chords and typed text to a nested Wayland compositor, such as a headless sway. It exists because ydotool writes to `/dev/uinput`, so its events reach the host seat and never the nested compositor. The module is `src/nested_input.rs`. It is Phase 1 of [the agent desktops plan](agent-desktops-plan.md).

Nested mode is off by default. Set `CUL_NESTED=1` to turn it on. With the variable unset, every input tool behaves as before.

## Owned concepts

- **Nested mode.** The process runs with `CUL_NESTED=1`. Every pointer, key and capture call targets `WAYLAND_DISPLAY` and nothing else. `nested_input::enabled()` is the single check.
- **Held devices.** One `zwlr_virtual_pointer_v1` and one `zwp_virtual_keyboard_v1`, opened on first use and kept for the life of the process. A headless seat drops a device that appears for one event and goes away, so one-shot devices do not work.
- **Base keymap.** A US keymap used for chords. `xkbcli compile-keymap --layout us` builds it when `xkbcli` is on PATH. Otherwise the bundled `src/nested_us_keymap.xkb` is used.
- **Typing keymap.** A throwaway keymap built per `type_text` call. Keycode `9 + i` maps to the i-th distinct keysym in the text. This is the wtype technique. It types any character that has a keysym, whatever the layout.
- **Layout bounds.** The bounding box of all `wl_output` modes, divided by each output's scale. Absolute pointer motion maps onto it.

## Owned data

State lives in memory only.

| Item | Where | Lifetime |
| --- | --- | --- |
| Pointer and keyboard objects, Wayland connection, event queue | `static DEVICES: Mutex<Option<NestedInput>>` | Process life, or until a call fails |
| Output geometry | `State.outputs`, filled at connect | Same as the devices |
| Active keymap (`Base` or `Typing`) and depressed modifier mask | `NestedInput` | Same as the devices |
| Base keymap text | `NestedInput.base_keymap`, read once at connect | Same as the devices |

Nothing is written to disk.

## Owned decisions

- **A failed call drops the devices.** `with_devices` clears the slot on any error, so the next call reconnects.
- **Nested mode never falls back to the host.** `capture_screenshot_raw` returns the grim result or an error. It skips the portal chain and any forced backend. `hydrate_session_bus_env` skips the host-graft steps and calls `isolate_environment` instead.
- **`isolate_environment` strips host keys.** It removes `HYPRLAND_INSTANCE_SIGNATURE`, `DISPLAY`, `XAUTHORITY`, `I3SOCK` and `NIRI_SOCKET`. It sets `XDG_CURRENT_DESKTOP=sway` when `SWAYSOCK` is set, and `XDG_SESSION_TYPE=wayland`.
- **Window tools see only the nested sway.** In nested mode the window registry tries the i3 backend alone, which runs `swaymsg` when `SWAYSOCK` is set. Every other backend is refused for list, focus, move and resize. Stripping `HYPRLAND_INSTANCE_SIGNATURE` is not enough on its own: `hyprctl` finds the host instance through `$XDG_RUNTIME_DIR/hypr` without it (seen 2026-10-09).
- **Sway windows are keyed by container id.** A sway view carries a pid, so its `window_id` is the sway container id and focus uses `[con_id=N]`. Wayland-native views have no X11 id, so i3's `[id=0x…]` selector cannot reach them.
- **AT-SPI acts only on nested apps.** The host and every nested desktop share one AT-SPI bus. `owns_pid` reads `/proc/<pid>/environ` and passes an app only when its `WAYLAND_DISPLAY`, and `SWAYSOCK` when set, match this process. `perform_action_inner` and `set_element_value` refuse any other app. The snapshot, `list_accessible_apps` and the process list in `list_apps` apply the same filter, so host apps never show.
- **Chords stay on the base keymap.** Modifier state is sent with explicit `modifiers` events in that keymap. The typing keymap is replaced by the base keymap after every `type_text` call, even on error.
- **Only three mouse buttons.** `button_code` accepts left, right and middle. Any other name makes the click tool return an error.

## Interfaces

Public functions in `nested_input`:

| Function | Effect |
| --- | --- |
| `enabled()` | True when `CUL_NESTED` is `1` |
| `layout_size()` | Width and height of the layout bounds |
| `click(x, y, button, count)` | Move, then press and release `count` times. Returns the clamped landing point |
| `scroll(point, direction, notches)` | Optional move, then discrete wheel notches |
| `drag(start, end)` | Press at start, move in 12 steps, release at end |
| `key_chord(modifiers, key)` | Evdev codes on the US keymap |
| `type_text(text)` | Typing keymap per run of up to 240 distinct keysyms |

`src/server.rs` calls these from the `click`, `scroll`, `drag`, `press_key` and `type_text` tools, each behind `if enabled()`. Blocking Wayland work runs through `run_nested_input`, which uses `spawn_blocking`. Results come back through `nested_action_result` with the message "on the nested display".

Test helpers are in `tests/common/mod.rs`:

- `HeadlessSway::start(width, height)` starts sway in its own process group with `WLR_BACKENDS=headless`, a short socket path under `XDG_RUNTIME_DIR`, and a config that sets the output size. It waits up to 10 s for the `ready` file that sway writes with `$WAYLAND_DISPLAY`.
- `exec`, `tree` and `wait_focused` drive and inspect sway through `swaymsg`.
- `mcp()` starts the MCP server with `WAYLAND_DISPLAY`, `SWAYSOCK` and `CUL_NESTED=1`. `Mcp::call` returns a tool's structured content.
- `missing(&[programs])` names the first absent program. A test that finds one skips, and passes.
- `host_cursor()` reads `hyprctl cursorpos`, so a test can assert the host cursor did not move.
- `Drop` sends SIGTERM to the sway process group, so apps it started go with it.

`tests/fixtures/nested/mouse_report.py` runs inside foot. It enables SGR mouse reporting and writes the reports to the path in `argv[1]`. Mode `press` stops at the first press or wheel event. Mode `drag` stops at the first release and keeps every report before it. It writes the file through a `.tmp` rename, so a reader never sees a partial file.

## Dependencies

- A compositor with `wl_seat`, `zwlr_virtual_pointer_manager_v1` and `zwp_virtual_keyboard_manager_v1`. Connecting fails with a named error when one is missing.
- `WAYLAND_DISPLAY` naming that compositor. An empty value is an error.
- `grim` for screenshots in nested mode.
- Crates: `wayland-client`, `wayland-protocols-wlr`, `wayland-protocols-misc`, `xkeysym`, `libc` (for `memfd_create`).
- Optional: `xkbcli` for the base keymap.
- Tests need `sway`, `swaymsg`, and the apps a test opens, such as `foot` and `zenity`.

## Invariants

- The devices are never created per call.
- In nested mode no input or capture path reaches the host session.
- The keymap fd stays open until the roundtrip after the keymap request, then closes.
- After `type_text` returns, the active keymap is the base keymap.
- Pointer positions are clamped to the layout, so a click never lands outside the output.
- Wayland key codes are evdev codes. The typing keymap offsets by 8 as XKB does, so slot `i` is sent as code `i + 1`.

## Failure boundaries

A failure drops the held devices and returns an error through the tool. The MCP server keeps running and the next call reconnects. Nothing is retried inside a call.

A keymap change is applied by clients on their own event loop. The module sleeps 25 ms after each upload to cover that. A slow client can still miss it. This is a timing assumption, not a guarantee.

A character with no keysym fails the whole `type_text` call before any key is sent.

## Verification status

Verified live on 2026-10-09 on Hyprland with sway 1.12:

- `tests/nested_input.rs` passes. It drives the real MCP server over stdio. It clicks the top-left cell and the centre cell of foot, right-clicks, scrolls, drags, types `hello Wörld ☺ $#@!` and presses Ctrl+D into `cat`, and types into a zenity entry. It then lists windows through sway and sends a key to one window by id.
- During that run a 20 Hz `hyprctl cursorpos` sampler read the same host point for all 126 samples, and `/proc/bus/input/devices` kept 41 devices. Nested mode creates no input device.
- A live `list_apps` with zenity open on the nested desktop returned zenity alone. The same call in host mode listed the host apps.

## See also

- [Agent desktops plan](agent-desktops-plan.md), Phase 1 and Phase 2
- `src/nested_input.rs`
- `tests/common/mod.rs`
- `tests/fixtures/nested/mouse_report.py`
