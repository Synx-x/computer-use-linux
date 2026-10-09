# Lane pwa

The `pwa` lane built Phase 3 of `agent-desktops-plan.md`: the whole `web/` tree. It touches nothing under `src/`, `Cargo.toml` or `tests/`. Branch: `lane/pwa`.

## What shipped

- `web/index.html`, `app.css`, `sky.css`: the artboard's markup, tokens, frosted glass, settings screen, transitions and sky, ported by copying the artboard's CSS and SVG verbatim. Glass level switches through `html[data-blur]`.
- `web/tile.js`: the `<desktop-tile>` web component, used for wall tiles (`variant="wall"`) and rail tiles (`variant="rail"`).
- `web/app.js`: view state, focus view, Take over and Hand back, the New desktop and Assign agent dialogs, Stop desktop, and thumbnails every 1.5 s. It keeps view, glass level and sky motion in `localStorage`.
- `web/api.js`: one interface with a live backend (the plan's API table) and a mock backend. `?mock` selects the mock.
- `web/mock.js`: the four sample desktops from the artboard.
- `web/manifest.webmanifest`, `web/sw.js`, `web/icons/`: install support. The service worker caches the shell only and never touches `/api/`.
- `web/vendor/novnc/`: noVNC 1.7.0 (`core/` and `vendor/pako`), copied from the npm package `@novnc/novnc`. It loads on first focus of a live desktop.
- `docs/design/design-ledger.md`, `docs/agent-desktops-pwa.md`, `docs/shots/`.

## Decisions

1. **Assumed API shapes.** `GET /api/desktops` may return an array or `{desktops: []}`. `api.js` `normalize()` accepts `res` or `resolution` (string or `{width,height}`), `needs` or `needs_you`, `last` or `last_action`, and `window_title`. It also reads an optional `log` array of `{t, verb, detail}` and an optional `control` of `user` or `agent`. If `core` names fields differently, change `normalize()` only.
2. **Action log fallback.** The API table has no log endpoint. Without a `log` field, the rail shows the single `last` action. Core can add `log` to the list payload and the UI will show it.
3. **Status events.** `/api/events` is treated as a nudge: any message triggers a refetch of `/api/desktops`. A 5 s poll backs it up.
4. **Take over.** The UI calls `POST .../takeover`, sets noVNC `viewOnly=false` and shows the amber banner. Hand back reverses both. "Pause agent" currently maps to take over, because the API has no separate pause.
5. **Disabled controls.** "Open in VNC viewer" shows a toast. The daemon gives no VNC viewer link yet.
6. **Idle desktops** get an "Assign agent" button in place of Pause agent. The artboard had no assign flow, so it uses the same dialog style as New desktop.
7. **Settings overlay** is `position: absolute` under `.space > .overlay`. The sky stylesheet sets `.space > *` to `position: relative`, which silently stretched the page until I raised the specificity.
8. **Fonts** load from Google Fonts, as in the artboard. The shell stays usable offline with system fallbacks.

## Verification

All checks ran on this machine on 2026-10-09.

| Check | Result |
| --- | --- |
| Mock wall, focus, settings, take over at 1440×960 | Matches the artboard. Screenshots in `docs/shots/` |
| Wall, focus, settings at 390×844 | No horizontal scroll, layout wraps as the artboard's flex rules intend |
| Live mode against a fake daemon on port 7431 | Two tiles rendered, thumbnails loaded (640 px image visible), `needs_you` mapped to Needs you, focus attempted a WebSocket to `/api/desktops/d1/vnc` |
| noVNC import in Chromium | `import('/vendor/novnc/core/rfb.js')` returns a class |
| Service worker | Activates, caches 13 shell files, manifest lists 3 icons |
| `impeccable detect web/` | Two findings, both inherited from the artboard: the page wrapper has no inset, and the sky has radial halos. Kept on purpose |
| Syntax | `node --check` passes on all JS files as modules |

Not verified, because `core` has not landed: a real wayvnc stream through the daemon proxy and a real thumbnail from grim. Also not verified: the install prompt from Chromium over HTTP. Installability normally needs `localhost`, which 127.0.0.1 satisfies, but I did not click Install.

## For core

- Serve `web/` at `/` and `web/vendor/` as-is. All asset paths are relative, so the PWA works at any mount.
- The static server on 7430 was `python3 -m http.server`. Stop it with `pkill -f "http.server 7430"` if it lingers.
- The WebSocket proxy must speak the RFB protocol over binary frames. noVNC passes no subprotocol.
