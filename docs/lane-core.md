# Lane core: nested input and the desktops daemon

The host here runs Hyprland, sway 1.12 and wayvnc 0.10.2. The core lane delivers Phase 1, Phase 2 and the Rust half of Phase 5 of [the agent desktops plan](agent-desktops-plan.md) on it. The host passes live checks for all three phases. The host cursor was recorded throughout. The host copy of the work sits on branch `lane/core`. The host sees no edits to `web/` from this lane.

## Shipped

| Phase | What | Where |
| --- | --- | --- |
| 1 | `CUL_NESTED=1` routes the five input tools to the nested compositor | `src/nested_input.rs`, `src/server.rs` |
| 1 | Nested isolation for window tools, screenshots and AT-SPI | `src/windowing/`, `src/screenshot.rs`, `src/atspi_tree.rs` |
| 2 | `computer-use-linux desktops serve` with the full API table | `src/desktops/` |
| 5 | Desktop limit, orphan cleanup at start, `desktops install-unit` | `src/desktops/` |
| tests | Live tests for Phase 1 and Phase 2 | `tests/` |

The five input tools are click, scroll, drag, press_key and type_text. The desktop limit is `--max`, default 4. The concept doc for Phase 1 is [nested-input.md](nested-input.md).

## Running it

```sh
cargo build
target/debug/computer-use-linux desktops serve --port 7420      # --max N, --web-dir DIR
target/debug/computer-use-linux desktops install-unit          # writes agent-desktops.service, does not enable it
```

The `web/` directory reaches the browser in three ways. Debug builds serve `web/` from the checkout. The `web/` copy in release builds is embedded when `web/index.html` existed at build time. The `web/` embed depends on a `cul_web` cfg from `build.rs`. The `web/` fallback is a one-line placeholder page, served at the root path when `web/` is missing. The `--web-dir` flag overrides both ways of finding `web/`.

## API

The routes match the plan's table. Each desktop row uses the field names that `normalize()` in the pwa lane's `web/api.js` reads. The PWA needs no mapping:

```json
{
  "id": "d1", "name": "final-check", "display": "wayland-2", "compositor": "sway headless",
  "res": "1280×800", "resolution": "1280x800", "width": 1280, "height": 800,
  "agent": "Claude · final-check", "task": "<the prompt>", "status": "running",
  "last": "click at 640,400", "ago": "4s", "windowTitle": "foot", "lines": [],
  "log": [{"t": "20:31:09", "verb": "click", "detail": "at 640,400"}],
  "control": "agent", "pane": "w1C:p1",
  "vnc": "/api/desktops/d1/vnc", "thumb": "/api/desktops/d1/thumb.webp",
  "vnc_port": 5901, "created": "20:30:41"
}
```

- The `status` field is `running`, `needs` or `idle`. The `control` field is `agent` or `user`.
- The list route returns a bare array. The single-desktop route returns one row.
- Create answers 201. It answers 409 at the limit and 400 for a bad resolution.
- Assign answers 409 when the desktop already has an agent. It answers 502 when herdr or the agent fails.
- The event route sends unnamed SSE messages shaped `{"type":"desktops","desktops":[...]}`. One comes on connect and one on every change. A plain `EventSource.onmessage` sees them all.
- The thumbnail is a 480 px wide lossless WebP. The VNC route speaks RFB inside binary WebSocket frames. It accepts the `binary` subprotocol that noVNC asks for.

## Decisions

### The daemon frames the WebSocket, not wayvnc

The plan said `wayvnc --websocket`, but wayvnc 0.10.2 with neatvnc 1.0.2 segfaults on any WebSocket handshake, with a null call inside libneatvnc. Wayvnc crashed on a tungstenite handshake at 20:15:45 on 2026-10-09. Wayvnc crashed again on a Python `websockets` handshake at 20:16:16. The daemon now runs wayvnc on plain TCP at 127.0.0.1. It carries the RFB bytes from wayvnc inside WebSocket frames, the way websockify does. That change dropped the `tokio-tungstenite` crate. The `ws` feature of axum covers the viewer side of wayvnc.

### View-only is enforced in the proxy

The proxy enforces view-only by filtering each RFB message. The filter in `src/desktops/rfb.rs` tracks message boundaries from the viewer. While the agent drives, it drops key, pointer, clipboard, xvp, resize and QEMU key messages. It forwards the other messages. Take over flips one flag, so an open viewer connection gains input messages without a reconnect. A message type the filter does not know closes the connection. A message it cannot frame could leak input if forwarded. The `viewOnly` flag in noVNC is a convenience only, and the server does not trust it.

### Each agent gets only this binary as its MCP server

The agent runs Claude with `--permission-mode bypassPermissions`, `--strict-mcp-config` and `--mcp-config <desktop>/mcp.json`. The agent's MCP file names the daemon's own executable with `mcp` and the nested environment. The user's global MCP entry points at `~/.local/bin`, a build without nested mode. That MCP build would drive the host with ydotool. The strict flag keeps that MCP entry out. The appended system prompt tells the agent which display it owns. It also tells the agent to wait for "continue" after an interrupt.

### Status falls back to the screen

Status comes from the pane record first and the pane's screen second. On this host the herdr agent list was empty for every pane, this lane's own session included. Asking herdr for a pane's agent answered `agent_not_found`. The pane itself reports `agent_status: unknown`. The daemon therefore reads the pane record and uses `agent_status` when it is `working`, `blocked` or `idle`. Otherwise it reads the pane's screen. Visible prompt text in the pane such as "Enter to select" or "Do you want to proceed?" means `needs`. The pane's terminal title starts with a spinner glyph while Claude works and with `✳` when it waits. The live run's pane went running, then idle, on that signal.

### One runtime root per port

Desktops live under `$XDG_RUNTIME_DIR/agent-desktops-<port>/<id>/`, one root per port. Desktops from two daemons never share a root, because every daemon first binds its port. Orphan cleanup of desktops runs after the bind. It kills a pid only when the process is still a sway or wayvnc whose command line names that desktop's directory. The first design shared one root across all desktops. With it, a test daemon could have killed a live daemon's desktops.

### Children are isolated and owned

The daemon isolates and owns every child it starts. The daemon starts sway and wayvnc with a cleared environment. It passes through only HOME, USER, PATH, the runtime dir, the session bus and the locale. The daemon runs each child in its own process group with `PR_SET_PDEATHSIG`. On stop, the daemon kills each whole group, so every app an agent opened goes too. The daemon runs on the current-thread runtime. The thread that forks the children therefore lives as long as the daemon process. The daemon starts sway in `$HOME`, so the apps it launches do not inherit the daemon's working directory.

### Shutdown does not wait on viewers

On SIGTERM or Ctrl-C, the daemon stops every desktop and closes every agent tab first. Open connections then get three seconds. An earlier daemon used axum's graceful shutdown alone. With one SSE stream attached, that daemon never exited and kept its desktops alive. The daemon test now holds an event stream open across SIGTERM. It requires an exit within 10 s.

### Nested mode cannot reach the host

Phase 1 found two leaks beyond input. Both are closed.

- The `hyprctl` tool finds the host Hyprland through `$XDG_RUNTIME_DIR/hypr` even with `HYPRLAND_INSTANCE_SIGNATURE` unset. The nested `list_windows` returned host windows. Nested mode now allows the i3 backend alone, which runs `swaymsg`. It refuses every other backend for focus, move and resize.
- The host and every nested desktop share one AT-SPI bus. A nested agent could have pressed a host app's button by element index. The two AT-SPI write paths now refuse any app whose process lacks this session's `WAYLAND_DISPLAY` and `SWAYSOCK`. The snapshot and both app lists apply the same filter.

### Smaller choices

- Each agent gets its own tab in the herdr workspace `agent-desktops`, created on first use. Its working directory is `~/.local/share/agent-desktops/<name>`. Stop closes the tab, which ends the agent.
- The plan's `tower-http` crate was not added. The module `src/desktops/web.rs` serves the checkout, the embedded copy and the placeholder through one path. It sets the right MIME type for `.webmanifest` files and modules.
- On error, herdr prints JSON on stderr and exits 1. A successful `pane run` prints nothing. The helper handles both.
- The `CUL_DESKTOPS_AGENT` variable replaces the Claude program. The daemon test uses it to run a stand-in agent.

## Verification

All checks ran on 2026-10-09 against the live host. Checks that read the host cursor used `hyprctl cursorpos`. Checks ran while the user was using the mouse, so readings taken minutes apart can differ. Checks treat the samples taken during the input itself as the decisive records.

### Phase 1

- The `nested_input` test passes. It drives the real MCP server against a headless sway.
- Clicks land in the expected foot cells: `0;1;1` at the corner and the centre cell at 640,400. Right click, wheel and drag report correctly.
- The text `hello Wörld ☺ $#@!` reaches `cat`, and Ctrl+D ends it. Text and Enter reach a zenity entry.
- The `list_windows` tool returns the nested foot through sway. A key sent by window id lands.
- A 20 Hz sampler ran across one full test run. The host cursor read 2696,278 in all 126 samples taken during the test. The kernel input device list held 41 devices before and after.

### Phase 2

- Using curl, I created a desktop, assigned a real Claude agent, and read its status and log. I then took over, handed back and stopped it.
- After the stop, no sway, wayvnc, agent claude or nested MCP server remained. Herdr reported the pane gone, and the runtime directory was removed.
- In the final live run, the agent ran `swaymsg exec foot` and took a screenshot. It clicked at 640,400 at 20:31:09, typed `echo clicked and typed` and pressed Enter.
- The desktop's own thumbnail shows that command and its output. A 10 Hz sampler read the host cursor at 2262,1139 from three seconds before the click to three seconds after. The device count held at 42 for the whole run.
- Through the proxy, a click sent while view-only did not land. After take over, the same click on a new connection landed in the centre cell of foot, 107×26. Wayvnc stayed up.
- The SSE stream carried every step, from start and launch through each tool call to idle. It also carried the take over and hand back.
- SIGTERM to the daemon stopped every desktop. No sway or wayvnc remained.
- The `desktops_daemon` test passes with `CUL_TEST_HERDR=1` and a stand-in agent. Its steps are listed below.

The daemon test covers these steps:

1. Create, the limit, list, the thumbnail, the page and the 404 for unknown API paths.
2. The view-only drop, take over and hand back.
3. Assign, then a second assign refused.
4. The agent's herdr pane closed on delete, and no process left after delete.
5. A prompt exit with an event stream open.

### Phase 5

The `install-unit` command writes a unit that `systemd-analyze --user verify` accepts. The unit is written, not enabled.

### Suites

The library suite passes all 357 tests. The new ones cover the RFB filter, keymaps, the transcript reader, status words, serve options and the unit text.

## Open items

- Clippy with `-D warnings` fails on two findings that predate this lane. They sit at `src/act.rs:234` and `src/server.rs:3461`.
- The fmt check also reports drift in files this lane did not write. New files are formatted. Existing files were left alone to keep the diff reviewable.
- The Phase 5 operator doc, `docs/agent-desktops.md`, is not written. It needs both lanes. This file and `nested-input.md` hold the core half.
- Enabling the systemd unit is left to the user.
- In nested mode, `focused_element_summary` can still read which host element has focus. It is read-only and feeds landing notes only. Gating it would need the same pid check.
- A herdr build with working agent detection would make `needs` exact. The screen fallback catches Claude's own prompts only.
- The `lines` field is always empty. The daemon has no text extraction of the screen yet.
- Tailnet access and auth remain out of scope, as the plan says.
