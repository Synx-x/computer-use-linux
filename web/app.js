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
  standard: ['Standard', 'Frosted. 40% bar, 6px tiles'],
  bleed: ['Bleed', 'Bar fades right, lighter tiles'],
  clear: ['Clear', 'Barely there. Sky in full'],
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
  const info = el('div');
  const sub = el('div', 'sub'); sub.id = 'f-sub';
  const h1 = el('h1'); h1.id = 'f-name';
  const stt = el('div', 'state'); stt.id = 'f-state';
  info.append(sub, h1, stt);
  const seg = el('div', 'seg glass');
  seg.setAttribute('role', 'group');
  seg.setAttribute('aria-label', 'Control');
  const watch = btn('', 'Watch', () => handBack(f.id)); watch.id = 'b-watch';
  const drive = btn('drive', 'Take over', () => takeOver(f.id)); drive.id = 'b-drive';
  seg.append(watch, drive);
  head.append(info, seg);

  const banner = el('div', 'banner');
  banner.id = 'banner';
  banner.setAttribute('role', 'status');
  banner.append(el('span', '', "You're driving. The agent is paused."), btn('', 'Hand back', () => handBack(f.id)));

  const foot = el('div', 'foot');
  const row = el('div', 'row');
  const shot = btn('pill', 'Save screenshot', () => saveShot(f)); shot.id = 'b-shot';
  const assign = btn('pill', 'Assign agent', () => assignDialog(f)); assign.id = 'b-assign';
  const stop = btn('pill danger', 'Stop desktop', () => stopDesktop(f)); stop.id = 'b-stop';
  row.append(assign, shot, stop);
  const mode = el('span', 'mode'); mode.id = 'f-mode';
  foot.append(row, mode);
  stage.append(head, banner, foot);

  const rail = el('aside', 'rail');
  rail.setAttribute('aria-label', 'Other desktops and agent actions');
  state.desktops.filter((d) => d.id !== f.id).forEach((d, i) => rail.append(makeTile(d, 'rail', i)));
  const log = el('div', 'log'); log.id = 'log';
  rail.append(log);
  sec.append(stage, rail);
  main.append(sec);
  surfaceKey = '';
}

let surfaceKey = '';
function updateStage(f) {
  const dr = driving(f);
  const stage = $('#stage');
  stage.classList.toggle('driving', dr);
  $('#banner').hidden = !dr;
  $('#f-sub').textContent = `${f.display} · ${f.compositor} · ${f.res}`;
  $('#f-name').textContent = f.name;
  const stt = $('#f-state');
  stt.replaceChildren();
  const color = f.status === 'needs' ? 'var(--amber)' : f.status === 'idle' ? 'var(--mute)' : 'var(--ink-3)';
  stt.style.color = color;
  stt.append(el('span', `dot ${f.status}`), document.createTextNode({ running: 'Running', needs: 'Needs you', idle: 'Idle' }[f.status] + ' '), el('span', 'mu', `· ${f.task}`));
  $('#b-watch').setAttribute('aria-pressed', String(!dr));
  $('#b-drive').setAttribute('aria-pressed', String(dr));
  $('#f-mode').textContent = dr ? 'YOU ARE DRIVING · agent paused' : 'WATCHING · read-only';
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
    if (rfb) rfb.viewOnly = !dr;
  }

  // Action log. Falls back to the single last action when the daemon sends no log.
  const log = $('#log');
  log.replaceChildren(el('h2', '', `AGENT ACTIONS · ${f.agent}`));
  const entries = f.log.length ? f.log : f.last ? [{ t: '', verb: 'last', detail: `${f.last}${f.ago ? ', ' + f.ago + ' ago' : ''}` }] : [];
  if (!entries.length) log.append(el('div', 'none', 'Nothing yet.'));
  for (const e of entries) {
    const r = el('div', 'e');
    r.append(el('span', 't', e.t), el('span', 'v', e.verb), el('span', 'd', e.detail));
    log.append(r);
  }
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
const takeOver = (id) => guard(async () => { state.mode = 'drive'; await api.takeover(id); await load(); });
const handBack = (id) => guard(async () => { state.mode = 'watch'; await api.handback(id); await load(); });

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
