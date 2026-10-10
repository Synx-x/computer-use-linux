// One interface, two backends. `live` talks to the daemon, `mock` keeps everything in memory.
// The daemon contract is the API table in docs/agent-desktops-plan.md.

import { sampleDesktops } from './mock.js';

const WORD = { running: 'running', needs: 'needs', needs_you: 'needs', 'needs-you': 'needs', idle: 'idle' };

/** Accept the daemon's field names loosely and return the shape the UI renders. */
export function normalize(r) {
  const status = WORD[String(r.status || 'idle').toLowerCase()] || 'idle';
  const res = r.res || r.resolution || '';
  return {
    id: String(r.id),
    name: r.name || r.id,
    display: r.display || r.wayland_display || '',
    compositor: r.compositor || 'sway headless',
    res: typeof res === 'object' ? `${res.width}×${res.height}` : String(res).replace('x', '×'),
    agent: r.agent || 'Unassigned',
    task: r.task || (status === 'idle' ? 'Ready for the next agent' : ''),
    status,
    last: r.last || r.last_action || '',
    ago: r.ago || '',
    windowTitle: r.windowTitle || r.window_title || '',
    lines: r.lines || [],
    log: r.log || [],
    control: r.control || null,
    pane: r.pane || null,
  };
}

async function call(method, path, body) {
  const res = await fetch(path, {
    method,
    headers: body ? { 'content-type': 'application/json' } : undefined,
    body: body ? JSON.stringify(body) : undefined,
  });
  if (!res.ok) throw new Error(`${method} ${path}: ${res.status} ${(await res.text()).slice(0, 120)}`);
  return res.status === 204 ? null : res.json().catch(() => null);
}

export function liveApi() {
  return {
    mock: false,
    async list() {
      const j = await call('GET', '/api/desktops');
      return (Array.isArray(j) ? j : j?.desktops || []).map(normalize);
    },
    create: (name, resolution) => call('POST', '/api/desktops', { name, resolution }),
    stop: (id) => call('DELETE', `/api/desktops/${encodeURIComponent(id)}`),
    assign: (id, prompt) => call('POST', `/api/desktops/${encodeURIComponent(id)}/agent`, { prompt }),
    takeover: (id) => call('POST', `/api/desktops/${encodeURIComponent(id)}/takeover`),
    handback: (id) => call('POST', `/api/desktops/${encodeURIComponent(id)}/handback`),
    /** Server-sent status events. `onEvent` fires on any message, the caller refetches. */
    subscribe(onEvent) {
      const es = new EventSource('/api/events');
      es.onmessage = onEvent;
      es.onerror = () => {};
      return () => es.close();
    },
  };
}

export function mockApi() {
  let rows = sampleDesktops();
  let n = 6;
  const listeners = new Set();
  const tick = () => listeners.forEach((f) => f());
  const find = (id) => rows.find((r) => r.id === id);
  return {
    mock: true,
    async list() { return rows.map((r) => normalize(structuredClone(r))); },
    async create(name, resolution) {
      const id = `new-${n}`;
      rows.push({ id, name: name || `desktop-${n}`, display: `wayland-${n}`, compositor: 'sway headless',
        res: (resolution || '1280x800').replace('x', '×'), agent: 'Unassigned', task: 'Ready for the next agent',
        status: 'idle', last: 'started', ago: '0s', windowTitle: '', lines: [], log: [] });
      n += 1;
      tick();
    },
    async stop(id) { rows = rows.filter((r) => r.id !== id); tick(); },
    async assign(id, prompt) {
      const r = find(id);
      if (!r) return;
      Object.assign(r, { status: 'running', agent: 'Claude · mock', task: prompt.slice(0, 60) || 'Working', last: 'launch', ago: '0s',
        windowTitle: 'foot — mock', lines: ['$ claude', 'Working on it…'],
        log: [{ t: new Date().toTimeString().slice(0, 8), verb: 'launch', detail: prompt.slice(0, 80) }] });
      tick();
    },
    async takeover(id) { const r = find(id); if (r) r.control = 'user'; tick(); },
    async handback(id) { const r = find(id); if (r) r.control = 'agent'; tick(); },
    subscribe(onEvent) { listeners.add(onEvent); return () => listeners.delete(onEvent); },
  };
}
