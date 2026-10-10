# Agent desktops

Agent desktops give each AI agent its own hidden Wayland desktop on this machine. Each agent clicks and types on its own desktop while the host cursor and focus stay with the user. The user watches every desktop from a web app, and the user can take over any desktop and hand it back.

## Parts

Agent desktops have three parts:

| Part | Where | Job |
| --- | --- | --- |
| Daemon | `computer-use-linux desktops serve`, `src/desktops/` | Starts desktops, runs agents, serves the API, the VNC proxy and the web app |
| Nested input | `CUL_NESTED=1`, `src/nested_input.rs` | Sends the MCP server's clicks and keys to one nested desktop, never to the host |
| Web app | `web/` | Wall and Focus views, settings, the space sky |

The daemon is the only process the user starts. The daemon starts everything else.

## Run it

```sh
cd ~/dev/computer-use-linux
cargo build
target/debug/computer-use-linux desktops serve --port 7420
```

Then open `http://127.0.0.1:7420` in Chromium. Chromium offers to install the page as an app.

Serve options:

| Option | Default | Effect |
| --- | --- | --- |
| `--port N` | 7420 | The port. The daemon binds 127.0.0.1 only |
| `--max N` | 4 | The most desktops at once |
| `--web-dir DIR` | `web/` in the checkout | Serves the web app from another folder |

The daemon needs `sway`, `wayvnc`, `grim`, `herdr` and `claude` on `PATH`.

## Use it

1. **New desktop** starts a headless sway with its own `wayland-N` socket.
2. **Assign agent** asks for a task prompt. The daemon then opens a herdr tab in the `agent-desktops` workspace and starts Claude there in bypass mode.
3. **Wall** shows every desktop as a live thumbnail with its status: Running, Needs you or Idle.
4. **Focus** shows one desktop as a live VNC stream, view-only, with the agent's recent actions beside it.
5. **Take over** interrupts the agent and passes the user's mouse and keys through the stream. **Hand back** blocks them again and tells the agent to continue.
6. **Stop desktop** ends the agent, the compositor and every app on that desktop.

## Key passthrough lock

The lock sends every key to the desktop during take over, Hyprland binds included.

- **Lock keys.** The padlock sits over the live view while the user drives. The padlock turns amber when locked and names the release key.
- **Release.** A tap of Right Ctrl alone releases the lock. Right Ctrl held with another key still passes through.
- **How.** The lock moves Hyprland into the `agentdesk-lock` submap. That submap holds one bind, on F24, a key no keyboard sends. Hyprland 0.56 refuses an unregistered submap, so the daemon registers it at runtime before each lock.
- **Window look.** Each desktop takes the host's gaps, border width and border colours from `hyprctl getoption` when it starts. Rounding needs SwayFX: with SwayFX as the installed sway, the desktop also takes the host's corner radius. Plain sway has no rounding.
- **Keybinds.** Each desktop's sway gets your Hyprland binds, read live from `hyprctl binds -j` when the desktop starts. Super+A toggles fullscreen there, as it does on the host. Fullscreen, focus, move, resize, workspaces and floating translate directly. An exec bind runs on the desktop, without its `uwsm-app --` prefix. The translation skips binds that would act on the host: the Caelestia CLI, `hyprctl`, `hyprshade`, session controls, `notify-send`, `pkill` and `killall`. `CUL_DESKTOP_BINDS=default` uses a small built-in set, and `off` writes none.
- **Fullscreen.** The fullscreen button is separate from the lock. Fullscreen while driving turns the lock on, and leaving fullscreen turns it off again. With both on, the page also calls `navigator.keyboard.lock()`, so Chromium's own shortcuts pass through. Holding Esc for 2 s always leaves fullscreen.
- **Safety.** The lock releases on hand back, desktop stop, window blur, a hidden page, a closed page, a lost event stream or VNC stream, and daemon SIGTERM. A daemon start also resets a lock left by a crash.

A live check on 2026-10-10 sent Super+5 and Ctrl+L while locked. Both reached the desktop: a nested sway bind fired and foot printed `^L`. The host stayed on its workspace. After a Right Ctrl tap, Super+5 switched the host workspace again. `docs/shots/lock/log.txt` holds the record.

Settings hold two theme options: Glass, for how much sky shows through the panels, and Sky, Moving or Still. The browser keeps both in `localStorage`.

## How the parts stay apart from the host

- **Input.** Each agent gets an MCP config that names only this binary with `CUL_NESTED=1`. Claude runs with `--strict-mcp-config`, so the host build that drives the real seat through ydotool never loads.
- **Windows and accessibility.** Nested mode allows only the sway window backend. Its AT-SPI tools refuse apps whose process lacks the desktop's `WAYLAND_DISPLAY` and `SWAYSOCK`.
- **View-only.** The daemon's VNC proxy reads every RFB message from the browser. While the agent drives, it drops key, pointer and clipboard messages. The browser's own view-only flag is a convenience only.
- **Processes.** Each sway and wayvnc runs in its own process group with a cleared environment. Stop kills the whole group.
- **Shell.** Each agent runs inside a bubblewrap sandbox. The filesystem is read-only, except the agent's own folder, `~/.claude` and a scratch `~/.cache`. The sandbox replaces `/dev`, `/tmp` and the runtime directory. That hides the host Wayland and Hyprland sockets, the session bus, `/dev/uinput` and the ydotool socket. The herdr socket is hidden too, because it can run commands in any pane. Only the desktop's own Wayland and sway sockets come back. `CUL_DESKTOPS_SANDBOX=off` turns the sandbox off.

A live agent checked the sandbox on 2026-10-10. Its own screenshot and `foot` worked. `hyprctl` could not reach Hyprland, a write to the repository failed with "Read-only file system", and `/dev/uinput` was missing.

[nested-input.md](nested-input.md) has the input design. [lane-core.md](lane-core.md) has every daemon decision and its evidence.

## API

All routes live on the daemon's port.

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/api/desktops` | List desktops |
| POST | `/api/desktops` | Create a desktop. Body: `name`, `resolution` |
| DELETE | `/api/desktops/:id` | Stop a desktop and its agent |
| POST | `/api/desktops/:id/agent` | Assign an agent. Body: `prompt` |
| POST | `/api/desktops/:id/takeover`, `/handback` | Change control |
| POST | `/api/desktops/:id/lock`, `/unlock` | Key passthrough lock, during take over only |
| GET | `/api/desktops/:id/thumb.webp` | Latest thumbnail |
| WS | `/api/desktops/:id/vnc` | Proxied VNC stream |
| GET | `/api/events` | Server-sent status events |

## Operate it

- **Logs.** The daemon writes to stderr. Under systemd, read them with `journalctl --user -u agent-desktops`.
- **Runtime files.** Each desktop lives in `$XDG_RUNTIME_DIR/agent-desktops-<port>/<id>/`. Agent working folders live in `~/.local/share/agent-desktops/<name>`.
- **Orphans.** On start, the daemon kills any sway or wayvnc left in its runtime folder by an earlier run.
- **Stop.** Ctrl-C or SIGTERM stops every desktop and closes every agent tab.
- **Install.** `desktops install` writes `~/.config/systemd/user/agent-desktops.service`, enables it and starts it. It also writes the **Agent Desktops** app launcher to `~/.local/share/applications/` and prints the MCP command. The unit runs the binary that wrote it, so run `install` from the build you want to keep. `desktops uninstall` removes the unit and the launcher.
- **Unit only.** `desktops install-unit` writes the unit and does not enable it.

## The orchestration MCP server

`computer-use-linux desktops mcp` is an MCP server over stdio for an agent that hands out work. It calls the daemon's local API, so the daemon must be running. `--port` or `CUL_DESKTOPS_PORT` picks the daemon.

| Tool | Use |
| --- | --- |
| `start_task` | Create a desktop and start a sandboxed agent with a prompt |
| `list_desktops` | Every desktop with its status, task and last action |
| `wait_for_desktop` | Wait until an agent goes idle or needs a person |
| `desktop_status` | One desktop's status and recent actions |
| `desktop_screenshot` | A small WebP screenshot of one desktop |
| `create_desktop`, `assign_task` | Create an empty desktop, then give it work |
| `stop_desktop` | End the agent, its apps and the desktop |

Register it with Claude Code: `claude mcp add --scope user agent-desktops -- computer-use-linux desktops mcp`.

## Known limits

- Linux with sway only. Windows support would need its own backend.
- wayvnc 0.10.2 crashes on WebSocket handshakes. The daemon therefore talks plain TCP to wayvnc and does the WebSocket framing itself.
- In agent-browser, `keyboard type` does not reach the stream, because noVNC listens for real key events. Its `press` command works.

## Sky credits

The sky imagery comes from ESO/S. Brunier and from NASA, ESA, CSA and STScI, under CC BY 4.0. `web/sky/CREDITS.md` lists each image and its source.
