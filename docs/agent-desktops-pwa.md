# Agent desktops PWA

The PWA is the browser front end for agent desktops. It lives in `web/` and is plain HTML, CSS and JS with no build step. It is the `pwa` lane's output of Phase 3 in [`agent-desktops-plan.md`](agent-desktops-plan.md). Its look is a port of the canvas artboard `docs/agent-desktops-design.dc.html`.

Status as of 2026-10-09: the `web/` tree is complete. `index.html`, `app.js`, `api.js`, `mock.js`, `tile.js`, the CSS, the manifest, the service worker, the icons and the vendored noVNC 1.7.0 all exist. `web/sky.html.part` was folded into `index.html`. See `lane-pwa.md` for the verification record.

## Purpose

The PWA puts every agent desktop on a wall. The user opens one in a focus view, takes over, and hands control back. The PWA talks only to the daemon API in the plan. It never reaches sway, wayvnc or herdr directly.

## Owned concepts

- **Wall**: the grid of desktop tiles. Each tile is `<desktop-tile variant="wall">`.
- **Rail**: the strip of side tiles in the focus view. Same element, `variant="rail"`.
- **Status**: one of `running`, `needs`, `idle`. Labelled "Running", "Needs you", "Idle" in `web/tile.js` (`STATUS`).
- **Mock mode**: the `mock` attribute on a tile. The tile draws a text window from sample lines instead of a thumbnail. The plan says `?mock` serves sample desktops from `web/mock.js`.
- **Glass level**: `html[data-blur]`, set to `standard`, `bleed` or `clear`. It swaps the frosted-glass tokens in `web/app.css`.
- **Sky**: the turning space background from the artboard, in `web/sky.css` and the `#sky` block in `web/index.html`. Phase 4 of the plan replaces it with `~/dev/space-bg`.

## Owned data

The PWA owns view state only. Settings (glass level, sky motion) persist in `localStorage` per the plan. Desktop records belong to the daemon.

A tile reads these fields from a record: `id`, `name`, `task`, `agent`, `last`, `ago`, `status`, `display`, `res`, `windowTitle`, `lines`.

## Owned decisions

- Tile markup and the `focus-desktop` event contract.
- Design tokens in `web/app.css`: `--bg #0B0A0F`, `--void #030208`, `--lime #7FFF00`, `--amber #F2B544`, `--red #FF6B80`, `--ease cubic-bezier(.22, 1, .36, 1)`.
- Type tokens: `--display` is Bricolage Grotesque and `--mono` is JetBrains Mono.
- Amber marks a tile that needs the user. Lime marks focus rings and links.
- Motion is CSS transitions copied from the artboard. No GSAP, no Three.js.

## Interfaces

`<desktop-tile>` in `web/tile.js` is a light-DOM custom element. Light DOM is deliberate, so page rules such as `.wall:has(desktop-tile:hover)` reach it.

| Member | Behaviour |
| --- | --- |
| `tile.data = record` | Sets `data-status`. Rebuilds only when a visible field changed, using a JSON signature. |
| `tile.refreshThumb()` | Loads `/api/desktops/:id/thumb.webp?t=<now>` into a hidden `Image`. On load it swaps the image in and hides the text fallback. No-op for idle tiles. |
| `variant` attribute | `wall` has Take over and Focus buttons plus the agent line. `rail` has a Focus label only. |
| `mock` attribute | Skips the thumbnail and uses the text window. |
| `focus-desktop` event | Bubbles. `detail` is `{ id, mode }`. `mode` is `watch` (Focus) or `drive` (Take over). |
| `windowMock(lines, title)` | Exported helper that builds the text window. |

Daemon endpoints the PWA consumes: `GET /api/desktops`, `GET /api/events`, `GET /api/desktops/:id/thumb.webp`, `WS /api/desktops/:id/vnc`, and `POST /api/desktops/:id/takeover` and `/handback`. The plan's API table defines them.

## Dependencies

- The `core` lane's daemon on 127.0.0.1:7420. The `pwa` lane serves its own static files on 7430.
- noVNC, vendored into `web/vendor/novnc/`, for the focus view.
- Fonts: Bricolage Grotesque, JetBrains Mono, and IBM Plex Sans for body text (set in `.space`).
- `agent-browser` for the 1440x960 and 390x844 screenshot checks.

## Invariants

- No build step. Everything under `web/` is served as written.
- The `pwa` lane edits `web/` only. The `core` lane must not edit it.
- Live data comes from the daemon. The service worker caches the shell only.
- Under `prefers-reduced-motion: reduce`, tile, stage, overlay and panel transitions and animations are off.
- While the settings screen is open, `.space.settings-open .content` fades and blurs the main view over 650 ms.
- At 560 px wide and below, the `.stage` and `.banner` offsets change. Wall rows grow to a 340 px minimum.

## Failure boundaries

- Missing thumbnail: the text mock stays up. A failed `Image` load changes nothing.
- A tile with status `idle` reads "Waiting for an agent" and never requests a thumbnail.
- Daemon down: the wall has no data source. `?mock` is the offline path.
- A bad record breaks only its own tile, since each tile builds itself.

## Design ledger

`docs/design/design-ledger.md` records which design tools the lane consulted. Direction came from the artboard, so most roster entries are N/A. `impeccable` is installed, and `impeccable detect` should run against `web/` during validation. `taste-skill`, `taste-library` and `excalidraw-diagram` were not found. The ledger says the artboard makes them unnecessary.

## Troubleshooting

| Symptom | Fix |
| --- | --- |
| Tile never gets a screenshot | Check `GET /api/desktops/:id/thumb.webp` returns 200. The daemon makes thumbnails only while a client views the wall. |
| Hover dimming does nothing | Keep the tile in light DOM. A shadow root blocks `.wall:has(desktop-tile:hover)`. |
| Glass looks wrong | Check `html[data-blur]` is `standard`, `bleed` or `clear`. |

## See also

- `docs/agent-desktops-plan.md`
- `docs/agent-desktops-design.dc.html`
- `docs/design/design-ledger.md`
