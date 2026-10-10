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

Settings hold two theme options: Glass, for how much sky shows through the panels, and Sky, Moving or Still. The browser keeps both in `localStorage`.

## How the parts stay apart from the host

- **Input.** Each agent gets an MCP config that names only this binary with `CUL_NESTED=1`. Claude runs with `--strict-mcp-config`, so the host build that drives the real seat through ydotool never loads.
- **Windows and accessibility.** Nested mode allows only the sway window backend. Its AT-SPI tools refuse apps whose process lacks the desktop's `WAYLAND_DISPLAY` and `SWAYSOCK`.
- **View-only.** The daemon's VNC proxy reads every RFB message from the browser. While the agent drives, it drops key, pointer and clipboard messages. The browser's own view-only flag is a convenience only.
- **Processes.** Each sway and wayvnc runs in its own process group with a cleared environment. Stop kills the whole group.

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
| GET | `/api/desktops/:id/thumb.webp` | Latest thumbnail |
| WS | `/api/desktops/:id/vnc` | Proxied VNC stream |
| GET | `/api/events` | Server-sent status events |

## Operate it

- **Logs.** The daemon writes to stderr. Under systemd, read them with `journalctl --user -u agent-desktops`.
- **Runtime files.** Each desktop lives in `$XDG_RUNTIME_DIR/agent-desktops-<port>/<id>/`. Agent working folders live in `~/.local/share/agent-desktops/<name>`.
- **Orphans.** On start, the daemon kills any sway or wayvnc left in its runtime folder by an earlier run.
- **Stop.** Ctrl-C or SIGTERM stops every desktop and closes every agent tab.
- **Service.** `desktops install-unit` writes `~/.config/systemd/user/agent-desktops.service` and does not enable it. The unit runs the binary that wrote it, so write it from the build you want to keep. Enable it with `systemctl --user daemon-reload && systemctl --user enable --now agent-desktops.service`.

## Known limits

- Linux with sway only. Windows support would need its own backend.
- wayvnc 0.10.2 crashes on WebSocket handshakes. The daemon therefore talks plain TCP to wayvnc and does the WebSocket framing itself.
- In agent-browser, `keyboard type` does not reach the stream, because noVNC listens for real key events. Its `press` command works.

## Sky credits

The sky imagery comes from ESO/S. Brunier and from NASA, ESA, CSA and STScI, under CC BY 4.0. `web/sky/CREDITS.md` lists each image and its source.
