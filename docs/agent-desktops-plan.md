# Agent desktops plan

Agent desktops give each AI agent its own hidden Wayland desktop on this machine. Each agent desktop can be watched from a web app, and the user can take over any agent's desktop there. The agent desktops design comes from the Agent Desktops Viewer canvas. The agent desktops approach was proven by a spike on 2026-10-09. A headless sway took keystrokes and a click while the host cursor and focus stayed still.

## Decisions

| Topic | Choice |
| --- | --- |
| Platform | Linux only |
| Backend | Rust, a `desktops` subcommand in this fork |
| Frontend | A PWA in plain HTML and JS, with no build step |
| Access | 127.0.0.1 only. Tailnet access and auth come later |
| First scope | Create a desktop, assign an agent, view it on the wall and in focus, take over, stop |

## Phase 1: input on a nested display

The MCP server uses ydotool today. Ydotool writes to `/dev/uinput`, so its events reach the host seat, not a nested compositor.

1. Add `src/nested_input.rs`. It holds one `zwlr_virtual_pointer_v1` and one `zwp_virtual_keyboard_v1` open for the life of the process. A held device is required: a headless seat drops one-shot devices (spike finding).
2. Choose the backend at startup. When `CUL_NESTED=1` is set, every click, scroll, key and type goes through `nested_input` on `WAYLAND_DISPLAY`. When it is unset, behaviour stays as it is.
3. Load the keyboard keymap from xkbcommon when it is available, or from a bundled US keymap.
4. Test: an integration test starts a headless sway, opens zenity, clicks a button and types into foot, then asserts the results. Skip the test when sway is missing.

Done when every input tool works against a headless sway and the host cursor position stays the same.

## Phase 2: the daemon

The daemon runs as `computer-use-linux desktops serve --port 7420`. It binds 127.0.0.1 only.

- **Desktop manager:** starts `sway` with `WLR_BACKENDS=headless`, `WLR_LIBINPUT_NO_DEVICES=1`, its own `SWAYSOCK` and a config that sets the resolution. It records the new `wayland-N` socket. On stop or exit it kills the whole process group.
- **View stream:** starts one `wayvnc --websocket` per desktop on a port from 5901 upward, bound to 127.0.0.1. The daemon proxies the WebSocket, so the browser only ever talks to port 7420.
- **Thumbnails:** runs `grim` on the desktop every 1.5 s, but only while a user is viewing the wall. The result is a WebP at a fixed width.
- **Agents:** "Assign agent" creates a herdr pane in workspace `agent-desktops`. The pane runs `WAYLAND_DISPLAY=wayland-N CUL_NESTED=1 claude --permission-mode bypassPermissions`, then answers the trust prompt. Status comes from `herdr agent get` and maps to Running, Needs you or Idle. The action log reads the agent's transcript tail.
- **Take over:** interrupts the agent by sending `escape` through herdr. It then switches the user's VNC session from view-only to input. **Hand back** reverts both and sends "continue" to the agent.

API:

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/api/desktops` | List desktops with status, task and last action |
| POST | `/api/desktops` | Create a desktop. Body: name, resolution |
| DELETE | `/api/desktops/:id` | Stop a desktop and its agent |
| POST | `/api/desktops/:id/agent` | Assign an agent. Body: prompt |
| POST | `/api/desktops/:id/takeover` and `/handback` | Change control |
| GET | `/api/desktops/:id/thumb.webp` | Latest thumbnail |
| WS | `/api/desktops/:id/vnc` | Proxied wayvnc stream |
| GET | `/api/events` | Server-sent status events |
| GET | `/` | The PWA |

New crates: `axum`, `tower-http` (static files), `tokio-tungstenite`. Add tokio's `net` and `fs` features.

Done when `curl` can create a desktop, assign an agent, read its status and stop it, and no sway or wayvnc process remains afterwards.

## Phase 3: the PWA

The PWA lives in `web/` and is embedded into the binary with `include_dir`.

- Port the canvas markup to plain HTML. Wall tiles and side tiles become one web component, `<desktop-tile>`. The page keeps the canvas's tokens, the frosted glass, the settings screen and the transitions.
- The wall shows thumbnails. Focus opens a live noVNC view of one desktop, view-only until take-over. noVNC is vendored from the npm package `@novnc/novnc` into `web/vendor/`.
- Add a manifest, icons and a service worker that caches the shell only. Live data always comes from the daemon.
- Settings persist in `localStorage`.

Done when the PWA installs from Chromium and drives the whole Phase 2 flow without the terminal.

## Phase 4: the space background

The sky from `~/dev/space-bg` replaces the CSS sky when that project's `PORT.md` lands.

## Phase 5: hardening

- A systemd user unit, `agent-desktops.service`.
- A limit on desktops, four by default.
- Cleanup of orphaned sway and wayvnc processes at daemon start.
- Docs in `docs/agent-desktops.md`.

## Verification

Every phase ends with a live check on this machine, and each check records the host cursor before and after. Screenshots of the PWA go in `docs/shots/`.
