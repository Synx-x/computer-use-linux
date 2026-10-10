import './tile.js';
import { windowMock } from './tile.js';
import { liveApi, mockApi } from './api.js';

const MOCK = new URLSearchParams(location.search).has('mock');
const api = MOCK ? mockApi() : liveApi();
const $ = (s, r = document) => r.querySelector(s);

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text != null) e.textContent = text;
  return e;
}
function btn(cls, text, onClick) {
  const b = el('button', cls, text);
  b.type = 'button';
  if (onClick) b.addEventListener('click', onClick);
  return b;
}

// Settings and last view persist in localStorage. Storage can throw, so every access is guarded.
const store = {
  get(k, d) { try { return localStorage.getItem(k) ?? d; } catch { return d; } },
  set(k, v) { try { localStorage.setItem(k, v); } catch { /* private window */ } },
};

const state = {
  view: store.get('ad.view', 'wall'),
  focusId: null,
  mode: 'watch', // watch | drive
  blur: store.get('ad.blur', 'standard'),
  sky: store.get('ad.sky', 'moving'),
  settings: false,
  desktops: [],
};

const BLUR = {
  standard: ['Standard', 'Frosted. 55% bar, 6px tiles'],
  bleed: ['Bleed', 'Bar fades right, lighter tiles'],
  clear: ['Clear', 'No glass. Sky in full'],
};

// ---------- chrome ----------
const root = document.documentElement;
const space = $('.space');

// The sky (sky.js) moves its camera when the page changes. Each change picks a move
// that matches it, and the sky ignores moves while it is still or reduced-motion.
const skyMove = (kind) => window.sky?.transition(kind);
let skyWas = { sky: null, settings: false, page: null };

function applyTheme() {
  root.dataset.blur = state.blur;
  if (state.sky !== skyWas.sky) { window.sky?.setMoving(state.sky !== 'still'); skyWas.sky = state.sky; }
  if (state.settings !== skyWas.settings) { skyMove(state.settings ? 'warp' : 'shift'); skyWas.settings = state.settings; }
  space.classList.toggle('settings-open', state.settings);
  $('.content').toggleAttribute('inert', state.settings);
  $('.content').setAttribute('aria-hidden', String(state.settings));
  $('.overlay').setAttribute('aria-hidden', String(!state.settings));
  $('.overlay').toggleAttribute('inert', !state.settings);
  $('#gear').setAttribute('aria-expanded', String(state.settings));
  $('#brand-host').textContent = `HYPRLAND · ${MOCK ? 'dusk' : location.hostname}`;
  for (const [id, v] of [['v-wall', 'wall'], ['v-focus', 'focus']]) $('#' + id).setAttribute('aria-pressed', String(state.view === v));
  $('#glass-opts').querySelectorAll('.opt').forEach((o) => o.setAttribute('aria-checked', String(o.dataset.k === state.blur)));
  $('#sky-opts').querySelectorAll('button').forEach((o) => o.setAttribute('aria-checked', String(o.dataset.k === state.sky)));
}

function buildSettings() {
  const glass = $('#glass-opts');
  for (const k of Object.keys(BLUR)) {
    const o = btn('opt', null, () => { state.blur = k; store.set('ad.blur', k); applyTheme(); });
    o.setAttribute('role', 'radio');
    o.dataset.k = k;
    const mini = el('span', 'mini');
    mini.dataset.k = k;
    mini.style.cssText = ''; // starfield background comes from .mini in sky.css
    mini.append(el('i', 'mbar'), el('i', 'mt a'), el('i', 'mt b'));
    const row = el('span', 'row');
    const txt = el('span');
    txt.append(el('span', 'l', BLUR[k][0]), el('span', 'n', BLUR[k][1]));
    row.append(txt, el('span', 'radio'));
    o.append(mini, row);
    glass.append(o);
  }
  const sky = $('#sky-opts');
  for (const [k, label] of [['moving', 'Moving'], ['still', 'Still']]) {
    const b = btn('', label, () => { state.sky = k; store.set('ad.sky', k); applyTheme(); });
    b.setAttribute('role', 'radio');
    b.dataset.k = k;
    sky.append(b);
  }
}

// ---------- data ----------
let toastTimer;
function toast(msg, ok = false) {
  const t = $('#toast');
  t.textContent = msg;
  t.className = 'toast' + (ok ? ' ok' : '');
  t.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { t.hidden = true; }, 4000);
}

async function guard(fn) {
  try { await fn(); } catch (e) { toast(e.message); }
}

let loadFailed = false;
async function load() {
  try {
    state.desktops = await api.list();
    loadFailed = false;
  } catch (e) {
    if (!loadFailed) toast(`Daemon unreachable: ${e.message}`);
    loadFailed = true;
  }
  if (state.focusId && !state.desktops.some((d) => d.id === state.focusId)) state.focusId = null;
  if (state.view === 'focus' && !state.focusId && state.desktops.length) state.focusId = state.desktops[0].id;
  render();
}

// ---------- render ----------
let shownKey = '';
const focused = () => state.desktops.find((d) => d.id === state.focusId) || state.desktops[0];
const driving = (d) => state.mode === 'drive' || d?.control === 'user';

function render() {
  const page = state.view + ':' + (state.view === 'focus' ? state.focusId : '');
  if (skyWas.page !== null && page !== skyWas.page) skyMove(state.view === 'wall' ? 'expand' : skyWas.page.startsWith('wall') ? 'zoom' : 'shift');
  skyWas.page = page;
  if (lockedId && (state.view !== 'focus' || state.focusId !== lockedId)) releaseLock('left');
  const live = state.desktops.filter((d) => d.status !== 'idle').length;
  const needs = state.desktops.filter((d) => d.status === 'needs').length;
  $('#live-n').textContent = live;
  $('#needs-n').textContent = needs;
  applyTheme();

  const f = focused();
  const key = state.view + '|' + (state.view === 'focus' ? f?.id : '') + '|' + state.desktops.map((d) => d.id).join(',');
  if (key !== shownKey) {
    shownKey = key;
    teardownVnc();
    state.view === 'wall' ? buildWall() : buildFocus();
  }
  updateTiles();
  if (state.view === 'focus' && f) updateStage(f);
}

function updateTiles() {
  for (const t of document.querySelectorAll('desktop-tile')) {
    const d = state.desktops.find((x) => x.id === t.dataset.id);
    if (d) t.data = d;
  }
}

function makeTile(d, variant, i) {
  const t = document.createElement('desktop-tile');
  t.setAttribute('variant', variant);
  if (MOCK) t.setAttribute('mock', '');
  t.dataset.id = d.id;
  t.style.animationDelay = (variant === 'rail' ? 60 + i * 90 : 80 + i * 110) + 'ms';
  return t;
}

function buildWall() {
  const main = $('#main');
  main.replaceChildren();
  const wall = el('section', 'wall');
  wall.setAttribute('aria-label', 'All desktops');
  if (!state.desktops.length) wall.append(el('div', 'empty', loadFailed ? 'The daemon is not answering.' : 'No desktops yet. Make one with New desktop.'));
  state.desktops.forEach((d, i) => wall.append(makeTile(d, 'wall', i)));
  main.append(wall);
}

let rfb = null;
let rfbHost = null;
function teardownVnc() {
  if (rfb) { try { rfb.disconnect(); } catch { /* already closed */ } }
  rfb = null;
  rfbHost = null;
}

// The divider between the live view and the side panel. Drag it, or focus it and use the
// arrow keys. A double-click puts it back. The width survives reloads.
const RAIL_MIN = 260;
const clampRail = (w) => Math.round(Math.max(RAIL_MIN, Math.min(w, Math.max(RAIL_MIN, innerWidth * 0.6))));
function makeDivider(sec) {
  const d = el('div', 'divider');
  d.setAttribute('role', 'separator');
  d.setAttribute('aria-orientation', 'vertical');
  d.setAttribute('aria-label', 'Resize the side panel');
  d.tabIndex = 0;
  const set = (w) => { w = clampRail(w); sec.style.setProperty('--rail-w', w + 'px'); d.setAttribute('aria-valuenow', String(w)); return w; };
  const save = (w) => store.set('ad.railw', w);
  const now = () => parseFloat(getComputedStyle(sec).getPropertyValue('--rail-w')) || 380;
  d.addEventListener('pointerdown', (e) => {
    e.preventDefault();
    d.setPointerCapture(e.pointerId);
    sec.classList.add('resizing');
    const right = sec.getBoundingClientRect().right;
    const move = (m) => set(right - m.clientX - d.offsetWidth / 2);
    const up = () => {
      d.removeEventListener('pointermove', move);
      sec.classList.remove('resizing');
      save(now());
    };
    d.addEventListener('pointermove', move);
    d.addEventListener('pointerup', up, { once: true });
    d.addEventListener('pointercancel', up, { once: true });
  });
  d.addEventListener('keydown', (e) => {
    const step = e.shiftKey ? 80 : 24;
    if (e.key === 'ArrowLeft') save(set(now() + step));
    else if (e.key === 'ArrowRight') save(set(now() - step));
    else return;
    e.preventDefault();
  });
  d.addEventListener('dblclick', () => save(set(380)));
  return d;
}

// Each log verb belongs to a kind, and the kind sets its chip colour.
const VERB_KIND = { click: 'act', scroll: 'act', drag: 'act', type_text: 'act', press_key: 'act', act: 'act', type: 'act', key: 'act',
  screenshot: 'look', capture: 'look', list_windows: 'look', get_app_state: 'look', zoom: 'look', focused_window: 'look',
  say: 'say', launch: 'life', start: 'life', stop: 'life', takeover: 'warn', handback: 'life', wait: 'warn', needs: 'warn', blocked: 'warn' };
function tidyDetail(d) {
  const s = String(d ?? '').trim();
  if (!s || s === '{}' || s === '[]') return '';
  if (s.startsWith('{')) {
    try { return Object.entries(JSON.parse(s)).map(([k, v]) => `${k.replace(/_/g, ' ')} ${typeof v === 'object' ? JSON.stringify(v) : v}`).join(' · '); } catch { /* not JSON, keep it */ }
  }
  return s;
}
let logKey = '';

function buildFocus() {
  const main = $('#main');
  main.replaceChildren();
  const f = focused();
  const sec = el('section', 'focus');
  sec.setAttribute('aria-label', 'Focused desktop');
  if (!f) { sec.append(el('div', 'empty', 'No desktops yet.')); main.append(sec); return; }

  const stage = el('div', 'stage');
  stage.id = 'stage';

  const head = el('div', 'head');
  // The live view starts below the header, however tall a long task makes it.
  new ResizeObserver(() => stage.style.setProperty('--head-h', head.offsetHeight + 'px')).observe(head);
  const info = el('div', 'info');
  // Three tiers: who and what state, then the task, then the quiet machine details.
  const titleRow = el('div', 'title-row');
  const h1 = el('h1'); h1.id = 'f-name';
  const chip = el('span', 'chip'); chip.id = 'f-chip';
  titleRow.append(h1, chip);
  const task = el('div', 'task'); task.id = 'f-task';
  const meta = el('div', 'meta');
  const sub = el('span', 'sub'); sub.id = 'f-sub';
  const mode = el('span', 'mode'); mode.id = 'f-mode';
  mode.setAttribute('role', 'status');
  meta.append(sub, mode);
  info.append(titleRow, task, meta);
  const seg = el('div', 'seg glass');
  seg.setAttribute('role', 'group');
  seg.setAttribute('aria-label', 'Control');
  const watch = btn('', 'Watch', () => handBack(f.id)); watch.id = 'b-watch';
  const drive = btn('drive', 'Take over', () => takeOver(f.id)); drive.id = 'b-drive';
  seg.append(watch, drive);
  const acts = el('div', 'acts');
  const assign = btn('pill', 'Assign agent', () => assignDialog(f)); assign.id = 'b-assign';
  const shot = btn('pill', 'Screenshot', () => saveShot(f)); shot.id = 'b-shot';
  const stop = btn('pill danger', 'Stop', () => stopDesktop(f)); stop.id = 'b-stop';
  acts.append(assign, shot, stop, seg);
  head.append(info, acts);


  // Take over tools over the live view: the key passthrough lock and fullscreen.
  const tools = el('div', 'lockbar');
  const lk = el('button', 'lk'); lk.type = 'button'; lk.id = 'b-lock';
  lk.addEventListener('click', () => toggleLock(f.id));
  const fs = el('button', 'lk fs'); fs.type = 'button'; fs.id = 'b-fs';
  fs.setAttribute('aria-label', 'Fullscreen live view');
  fs.innerHTML = '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5"/></svg>';
  fs.addEventListener('click', toggleFullscreen);
  tools.append(lk, fs);
  stage.append(head, tools);

  const rail = el('aside', 'rail');
  rail.setAttribute('aria-label', 'Other desktops and agent actions');
  state.desktops.filter((d) => d.id !== f.id).forEach((d, i) => rail.append(makeTile(d, 'rail', i)));
  const log = el('div', 'log'); log.id = 'log';
  log.setAttribute('aria-live', 'polite');
  rail.append(log);
  sec.style.setProperty('--rail-w', clampRail(store.get('ad.railw', 380)) + 'px');
  sec.append(stage, makeDivider(sec), rail);
  main.append(sec);
  surfaceKey = '';
}

let surfaceKey = '';
function updateStage(f) {
  const dr = driving(f);
  const stage = $('#stage');
  stage.classList.toggle('driving', dr);
  $('#f-sub').textContent = `${f.display} · ${f.compositor} · ${f.res}`;
  $('#f-name').textContent = f.name;
  const chip = $('#f-chip');
  chip.className = `chip ${f.status}`;
  chip.replaceChildren(el('span', `dot ${f.status}`), document.createTextNode({ running: 'Running', needs: 'Needs you', idle: 'Idle' }[f.status]));
  $('#f-task').textContent = f.task;
  $('#f-task').title = f.task;
  $('#b-watch').setAttribute('aria-pressed', String(!dr));
  $('#b-drive').setAttribute('aria-pressed', String(dr));
  $('#f-mode').textContent = dr ? 'You are driving · agent paused' : 'Watching · read-only';
  $('#f-mode').classList.toggle('drive', dr);
  const locked = dr && f.locked;
  stage.classList.toggle('locked', locked);
  const lk = $('#b-lock');
  lk.setAttribute('aria-pressed', String(locked));
  lk.innerHTML = locked
    ? '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="5" y="11" width="14" height="10" rx="2"/><path d="M8 11V7a4 4 0 0 1 8 0v4"/></svg><span>Keys locked</span><kbd>Right Ctrl</kbd><span class="hint">to release</span>'
    : '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="5" y="11" width="14" height="10" rx="2"/><path d="M8 11V7a4 4 0 0 1 7.6-1.8"/></svg><span>Lock keys</span>';
  lk.title = locked ? 'Every key, Hyprland binds included, goes to this desktop. Tap Right Ctrl alone to release.' : 'Send every key, Hyprland binds included, to this desktop';
  // Keys only reach noVNC through its canvas, so a fresh lock focuses it, whoever set it.
  if (locked && lockedId !== f.id) requestAnimationFrame(() => $('#stage canvas')?.focus());
  lockedId = locked ? f.id : (lockedId === f.id ? null : lockedId);
  syncKeyboardLock();
  const idle = f.status === 'idle';
  // A real desktop always has a live display. Only a desktop with no agent offers Assign.
  const unassigned = MOCK ? idle : !f.pane;
  $('#b-assign').hidden = !unassigned;

  // The stage shows live VNC in real mode and the text window in mock mode.
  const skey = [f.id, f.status, f.windowTitle, f.lines.join('|')].join('#');
  if (MOCK && skey !== surfaceKey) {
    surfaceKey = skey;
    stage.querySelectorAll('.win, .idle-note').forEach((n) => n.remove());
    if (idle) stage.prepend(el('div', 'idle-note', 'Waiting for an agent'));
    else { const w = windowMock(f.lines, f.windowTitle); stage.prepend(w); }
  }
  if (!MOCK) {
    if (rfbHost && rfbHost !== f.id) teardownVnc();
    connectVnc(f, stage);
    // An empty desktop streams as plain black, so say what it is and what to do.
    const note = stage.querySelector('.empty-note');
    stage.classList.toggle('empty', unassigned);
    if (unassigned && !note) stage.append(el('div', 'empty-note', 'Empty desktop · assign an agent to start'));
    else if (!unassigned) note?.remove();
    if (rfb) rfb.viewOnly = !dr;
  }

  // Action log. Falls back to the single last action when the daemon sends no log.
  const log = $('#log');
  const entries = f.log.length ? f.log : f.last ? [{ t: '', verb: 'last', detail: `${f.last}${f.ago ? ', ' + f.ago + ' ago' : ''}` }] : [];
  // Rebuild only when the log changed, so a reader's scroll position holds between polls.
  const key = f.id + '|' + f.agent + '|' + JSON.stringify(entries);
  if (key === logKey && log.childElementCount) return;
  logKey = key;
  const h = el('div', 'lh');
  h.append(el('h2', '', 'Agent actions'), el('span', 'who', f.agent));
  log.replaceChildren(h);
  if (!entries.length) { log.append(el('div', 'none', 'Nothing yet. Actions show here as the agent works.')); return; }
  const list = el('ol', 'entries');
  for (const e of entries) {
    const kind = VERB_KIND[e.verb] || 'other';
    const r = el('li', `e ${kind}`);
    const top = el('div', 'et');
    top.append(el('span', 'v', e.verb === 'say' ? 'said' : e.verb.replace(/_/g, ' ')), el('span', 't', e.t));
    r.append(top);
    const d = tidyDetail(e.detail);
    if (d) r.append(el('div', 'd', d));
    list.append(r);
  }
  log.append(list);
}

async function connectVnc(f, stage) {
  if (rfb || rfbHost === f.id) return;
  rfbHost = f.id;
  try {
    const { default: RFB } = await import('./vendor/novnc/core/rfb.js');
    if (rfbHost !== f.id) return;
    const host = el('div', 'vnc');
    stage.prepend(host);
    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    rfb = new RFB(host, `${proto}://${location.host}/api/desktops/${encodeURIComponent(f.id)}/vnc`);
    rfb.viewOnly = !driving(f);
    rfb.scaleViewport = true;
    rfb.background = 'transparent';
    rfb.addEventListener('disconnect', () => { host.remove(); rfb = null; rfbHost = null; });
  } catch (e) {
    rfbHost = null;
    toast(`Live view failed: ${e.message}`);
  }
}

// ---------- actions ----------
// ---------- key passthrough lock ----------
// While locked, Hyprland sits in an empty submap, so every key reaches this page and
// through noVNC the desktop. A tap of Right Ctrl alone releases it. Leaving the page,
// losing focus or handing back always releases it, so the host never keeps the lock.
let lockedId = null;
function toggleLock(id) {
  const f = state.desktops.find((d) => d.id === id);
  return guard(async () => {
    if (f?.locked) await api.unlock(id);
    else { await api.lock(id); $('#stage canvas')?.focus(); }
    await load();
  });
}
function releaseLock(reason) {
  if (!lockedId || MOCK) return;
  const id = lockedId;
  lockedId = null;
  autoLocked = false;
  if (reason === 'pagehide') navigator.sendBeacon?.(`/api/desktops/${encodeURIComponent(id)}/unlock?reason=pagehide`);
  else api.unlock(id, reason).then(load).catch(() => {});
}
// Entering or leaving fullscreen can fire a blur without any real loss of focus,
// so a blur releases the lock only when focus is still gone a moment later.
let fsChangedAt = 0;
function onBlur() {
  if (!lockedId) return;
  setTimeout(() => {
    if (Date.now() - fsChangedAt < 1500) return;
    if (!document.hasFocus()) releaseLock('blur');
  }, 400);
}
let rctl = { down: false, other: false };
addEventListener('keydown', (e) => {
  if (e.code === 'ControlRight') { if (!e.repeat) rctl = { down: true, other: false }; }
  else if (rctl.down) rctl.other = true;
}, true);
addEventListener('keyup', (e) => {
  if (e.code !== 'ControlRight') return;
  const tap = rctl.down && !rctl.other;
  rctl = { down: false, other: false };
  if (tap && lockedId) releaseLock('rctrl');
}, true);
addEventListener('blur', onBlur);
document.addEventListener('visibilitychange', () => { if (document.hidden) releaseLock('hidden'); });
addEventListener('pagehide', () => releaseLock('pagehide'));

// Fullscreen is separate from the lock. With both on, Chromium's own shortcuts
// (Ctrl+W, Ctrl+T, Esc) pass through too. Holding Esc for 2 s always exits.
function toggleFullscreen() {
  const stage = $('#stage');
  if (document.fullscreenElement) document.exitFullscreen?.();
  else stage?.requestFullscreen?.().catch((e) => toast(`Fullscreen failed: ${e.message}`));
}
// Fullscreen while driving is computer-use mode: the keys lock to the desktop, so Super
// and Alt combos reach it instead of Hyprland. A lock fullscreen set, fullscreen clears.
let autoLocked = false;
async function fullscreenLock(on) {
  const f = focused();
  if (MOCK || !f || !driving(f)) return;
  try {
    if (on && !f.locked) { await api.lock(f.id); autoLocked = true; await load(); $('#stage canvas')?.focus(); }
    else if (!on && autoLocked && lockedId) { autoLocked = false; releaseLock('fullscreen-exit'); }
  } catch (e) { toast(e.message); }
}
// The desktop may not fill a fullscreen screen, so the pointer can leave it through
// the letterbox. Moves there go to the canvas, which pins them to the desktop's edge,
// so edge panels and hot corners still trigger.
function forwardEdgePointer(e) {
  const canvas = $('#stage canvas');
  if (!canvas || e.target === canvas || !e.isTrusted) return;
  canvas.dispatchEvent(new MouseEvent(e.type, { clientX: e.clientX, clientY: e.clientY, button: e.button, buttons: e.buttons, bubbles: true, cancelable: true }));
}
for (const type of ['mousemove', 'mousedown', 'mouseup']) {
  document.addEventListener(type, (e) => { if (document.fullscreenElement && e.target.closest?.('.vnc')) forwardEdgePointer(e); }, true);
}
function syncKeyboardLock() {
  const kb = navigator.keyboard;
  if (!kb?.lock) return;
  if (document.fullscreenElement && lockedId) kb.lock().catch(() => {});
  else kb.unlock?.();
}
document.addEventListener('fullscreenchange', () => {
  fsChangedAt = Date.now();
  fullscreenLock(Boolean(document.fullscreenElement));
  syncKeyboardLock();
  const on = Boolean(document.fullscreenElement);
  $('#b-fs')?.setAttribute('aria-pressed', String(on));
  $('#b-fs')?.setAttribute('aria-label', on ? 'Exit fullscreen' : 'Fullscreen live view');
});

const takeOver = (id) => guard(async () => { state.mode = 'drive'; await api.takeover(id); await load(); });
const handBack = (id) => guard(async () => { if (lockedId === id) lockedId = null; state.mode = 'watch'; await api.handback(id); await load(); });

function saveShot(f) {
  const done = (blob) => {
    const a = el('a');
    a.href = URL.createObjectURL(blob);
    a.download = `${f.name}.${blob.type.includes('png') ? 'png' : 'webp'}`;
    a.click();
    setTimeout(() => URL.revokeObjectURL(a.href), 2000);
  };
  if (rfb) return rfb.toBlob(done, 'image/png');
  if (MOCK) return toast('Mock mode has no pixels to save');
  fetch(`/api/desktops/${encodeURIComponent(f.id)}/thumb.webp`).then((r) => r.blob()).then(done).catch((e) => toast(e.message));
}

function ask(title, fields, submit) {
  const dlg = $('#dlg');
  const form = $('form', dlg);
  form.replaceChildren(el('h2', '', title));
  for (const fl of fields) {
    const lab = el('label', '', fl.label);
    let inp;
    if (fl.options) {
      inp = el('select');
      fl.options.forEach((o) => inp.append(new Option(o, o)));
    } else if (fl.area) {
      inp = el('textarea');
    } else {
      inp = el('input');
      inp.autocomplete = 'off';
    }
    inp.name = fl.name;
    if (fl.placeholder) inp.placeholder = fl.placeholder;
    if (fl.value) inp.value = fl.value;
    lab.append(inp);
    form.append(lab);
  }
  const row = el('div', 'row');
  const cancel = btn('pill quiet', 'Cancel', () => dlg.close());
  const ok = btn('pill accent', 'Create');
  ok.type = 'submit';
  ok.textContent = title.startsWith('Assign') ? 'Assign' : 'Create';
  row.append(cancel, ok);
  form.append(row);
  form.onsubmit = (e) => {
    e.preventDefault();
    const data = Object.fromEntries(new FormData(form));
    dlg.close();
    guard(async () => { await submit(data); await load(); });
  };
  dlg.showModal();
  form.querySelector('input, textarea, select')?.focus();
}

function newDesktop() {
  ask('New desktop', [
    { name: 'name', label: 'Name', placeholder: 'ytm-refine' },
    { name: 'resolution', label: 'Resolution', options: ['1280x800', '1440x900', '1920x1080'] },
  ], (v) => api.create(v.name.trim(), v.resolution));
}
function assignDialog(f) {
  ask(`Assign agent to ${f.name}`, [{ name: 'prompt', label: 'Task prompt', area: true, placeholder: 'What should the agent do?' }],
    (v) => api.assign(f.id, v.prompt.trim()));
}
function stopDesktop(f) {
  if (confirm(`Stop ${f.name}? This ends its agent too.`)) guard(async () => { state.focusId = null; await api.stop(f.id); await load(); });
}

// ---------- wiring ----------
$('#v-wall').addEventListener('click', () => { state.view = 'wall'; store.set('ad.view', 'wall'); render(); });
$('#v-focus').addEventListener('click', () => { state.view = 'focus'; store.set('ad.view', 'focus'); if (!state.focusId) state.focusId = state.desktops[0]?.id ?? null; render(); });
$('#new').addEventListener('click', newDesktop);
$('#gear').addEventListener('click', () => { state.settings = !state.settings; applyTheme(); });
$('#done').addEventListener('click', () => { state.settings = false; applyTheme(); });
document.addEventListener('keydown', (e) => { if (e.key === 'Escape' && state.settings) { state.settings = false; applyTheme(); } });
document.addEventListener('focus-desktop', (e) => {
  state.view = 'focus';
  state.focusId = e.detail.id;
  state.mode = 'watch';
  store.set('ad.view', 'focus');
  if (e.detail.mode === 'drive') takeOver(e.detail.id);
  else render();
});

new ResizeObserver(() => root.style.setProperty('--top-h', $('.top').offsetHeight + 'px')).observe($('.top'));
// A #focus/<id> link opens that desktop in Focus.
const deep = /^#focus\/(.+)$/.exec(location.hash);
if (deep) { state.view = 'focus'; state.focusId = decodeURIComponent(deep[1]); }
buildSettings();
applyTheme();
api.subscribe(() => load());
load();
setInterval(() => { if (!document.hidden) load(); }, 5000);

// Thumbnails refresh every 1.5 s, and only while the wall or the rail is on screen.
if (!MOCK) {
  setInterval(() => {
    if (document.hidden || state.settings) return;
    document.querySelectorAll('desktop-tile').forEach((t) => t.refreshThumb());
  }, 1500);
}

if ('serviceWorker' in navigator && location.protocol !== 'file:') {
  navigator.serviceWorker.register('sw.js').catch(() => {});
}
