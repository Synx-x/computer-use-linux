/* SkyLogic has the same shape as the artboard's DCLogic component. PORT.md copies its
   methods into that component. render() below stands in for the artboard's {{bindings}}
   and <sc-for>, and is the only part that touches the DOM. */
class SkyLogic {
  constructor() {
    this.state = { sky: 'moving', skySlots: SkyLogic.skyEmpty(), skyCam: SkyLogic.skyCam0(), skyCamMs: 0, skyCamEase: 'linear', skyCamFade: false, skyFlash: null };
  }
  static skyEmpty() { return Array.from({ length: 12 }, () => ({ on: false, tok: 0, gen: 0, kind: '', outer: '', inner: '' })); }
  static skyCam0() { const z = { r: 0, x: 0, y: 0, s: 1 }; return { far: { ...z }, mid: { ...z }, near: { ...z } }; }

  /* Centre of each constellation in chart coordinates (pole at 1700,1700). */
  skyCentres() { return this._skyC || (this._skyC = [[1303,2728],[1260,2124],[1762,2252],[755,2638],[1532,2922],[2261,927],[2531,1448],[1907,1223],[2867,988],[1941,597],[2415,1919],[2203,2639],[1444,2670],[470,2439],[1289,1186],[1078,757],[2384,975],[2950,1237],[712,1731],[1590,1521],[1121,1663],[1933,1539],[1962,2046],[1359,887],[883,2309],[752,1234],[1942,1211],[2042,2609]]); }

  componentDidMount(seed) { this.skyStart(seed); }
  componentWillUnmount() { this.skyStop(); }

  /* ---- timers and the seeded random source ---- */
  skyStart(seed) {
    let a = seed || 20261009;
    const rand = () => { a |= 0; a = a + 0x6D2B79F5 | 0; let t = Math.imul(a ^ a >>> 15, 1 | a); t = t + Math.imul(t ^ t >>> 7, 61 | t) ^ t; return ((t ^ t >>> 14) >>> 0) / 4294967296; };
    const mq = window.matchMedia('(prefers-reduced-motion: reduce)');
    this.sk = { rand, mq, timers: new Set(), slots: SkyLogic.skyEmpty(), cam: SkyLogic.skyCam0(), live: new Map(), token: 0, busy: false, run: 0, last: performance.now(), flashGen: 0, stats: { meteor: 0, shower: 0, sat: 0, flare: 0, comet: 0, maxGroups: 0 }, log: [] };
    this.sk.onMq = () => this.skySetMoving(this.state.sky !== 'still');
    mq.addEventListener('change', this.sk.onMq);
    this.skyLater(() => this.skyTick(), this.skyR(800, 2500));
  }
  skyStop() {
    if (!this.sk) return;
    this.sk.timers.forEach(clearTimeout);
    this.sk.mq.removeEventListener('change', this.sk.onMq);
    this.sk = null;
  }
  skyLater(fn, ms) { const t = setTimeout(() => { if (this.sk) { this.sk.timers.delete(t); fn(); } }, ms); this.sk.timers.add(t); }
  skyR(a, b) { return a + (b - a) * this.sk.rand(); }
  skyPick(list) { return list[Math.floor(this.sk.rand() * list.length)]; }
  skyLive() { return this.state.sky !== 'still' && !this.sk.mq.matches; }
  skyClock() { const n = performance.now(); if (this.skyLive()) this.sk.run += n - this.sk.last; this.sk.last = n; return this.sk.run; }

  /* "Sky: Moving / Still". Still pauses the CSS and clears any event in flight. */
  skySetMoving(on) {
    this.skyClock();
    this.setState({ sky: on ? 'moving' : 'still' });
    if (!on || this.sk.mq.matches) { this.sk.slots = this.sk.slots.map((x) => ({ ...x, on: false, tok: 0 })); this.sk.live.clear(); this.setState({ skySlots: this.sk.slots.slice() }); }
  }

  /* ---- random events: at most two groups on screen, a shower counts as one ---- */
  skyTick() {
    const groups = this.sk.live.size, comet = [...this.sk.live.values()].includes('comet');
    if (this.skyLive() && groups < 2) {
      const kinds = [['meteor', 70], ['shower', 6], ['sat', 10], ['flare', 12], ['comet', 2]].filter((k) => k[0] !== 'comet' || (!comet && groups === 0));
      let roll = this.sk.rand() * kinds.reduce((s, k) => s + k[1], 0), kind = kinds[0][0];
      for (const k of kinds) { if ((roll -= k[1]) <= 0) { kind = k[0]; break; } }
      this.skyAdd(kind);
    }
    this.skyLater(() => this.skyTick(), this.sk.live.size >= 2 ? 1200 : this.skyR(1600, 6200));
  }
  skyMeteor(x, y, ang, dist, len, dur, delay, pk) {
    const tint = this.skyPick(['233,255,208', '233,255,208', '217,204,255', '207,227,255']);
    return { kind: 'meteor', ms: dur + delay, outer: `left:${x.toFixed(0)}px;top:${y.toFixed(0)}px;transform:rotate(${ang.toFixed(1)}deg);--len:${len.toFixed(0)}px;--tint:rgba(${tint},${pk.toFixed(2)})`, inner: `width:${dist.toFixed(0)}px;animation:mv@ ${dur.toFixed(0)}ms cubic-bezier(.3,.1,.5,1) ${delay.toFixed(0)}ms both` };
  }
  skyParts(kind) {
    const W = window.innerWidth, H = window.innerHeight, R = (a, b) => this.skyR(a, b);
    const heading = (lo, hi) => { const a = R(lo, hi); return this.sk.rand() < 0.5 ? 180 - a : a; };
    if (kind === 'meteor') return [this.skyMeteor(R(-0.02, 1.02) * W, R(-0.04, 0.62) * H, heading(12, 78), R(0.25, 0.75) * Math.min(W, 1400), R(70, 240), R(420, 1400), 0, R(0.55, 1))];
    if (kind === 'shower') {
      const a = heading(18, 70), cx = R(0.1, 0.9) * W, cy = R(0, 0.45) * H, n = Math.round(R(6, 10)), out = [];
      for (let i = 0; i < n; i++) out.push(this.skyMeteor(cx + R(-0.3, 0.3) * W, cy + R(-0.2, 0.2) * H, a + R(-5, 5), R(0.2, 0.5) * Math.min(W, 1200), R(60, 190), R(380, 900), R(0, 2600), R(0.45, 0.95)));
      return out;
    }
    if (kind === 'sat' || kind === 'comet') {
      const ltr = this.sk.rand() < 0.5, y0 = R(0.08, kind === 'sat' ? 0.8 : 0.55) * H, y1 = y0 + (kind === 'sat' ? R(-0.35, 0.35) : R(0.15, 0.4)) * H;
      const x0 = ltr ? -40 : W + 40, x1 = ltr ? W + 40 : -40, dist = Math.hypot(x1 - x0, y1 - y0) + (kind === 'comet' ? 560 : 0);
      const ang = Math.atan2(y1 - y0, x1 - x0) * 180 / Math.PI, dur = kind === 'sat' ? R(26000, 46000) : R(150000, 200000);
      return [{ kind, ms: dur, outer: `left:${x0.toFixed(0)}px;top:${y0.toFixed(0)}px;transform:rotate(${ang.toFixed(2)}deg)`, inner: `width:${dist.toFixed(0)}px;animation:${kind === 'sat' ? 'st' : 'cm'}@ ${dur.toFixed(0)}ms linear both` }];
    }
    const dur = R(2600, 4200);
    return [{ kind: 'flare', ms: dur, outer: `left:${(R(0.05, 0.95) * W).toFixed(0)}px;top:${(R(0.05, 0.9) * H).toFixed(0)}px;transform:scale(${R(0.5, 1).toFixed(2)})`, inner: `animation:fr@ ${dur.toFixed(0)}ms ease-out both` }];
  }
  skyAdd(kind) {
    const parts = this.skyParts(kind), tok = ++this.sk.token;
    let n = 0;
    this.sk.slots = this.sk.slots.map((s) => {
      if (s.on || n >= parts.length) return s;
      const p = parts[n++], gen = s.gen + 1;
      return { on: true, tok, gen, kind: p.kind, outer: p.outer, inner: p.inner.replace('@', gen % 2 ? 'A' : 'B') };
    });
    if (!n) return;
    this.sk.live.set(tok, kind);
    this.sk.stats[kind]++; this.sk.stats.maxGroups = Math.max(this.sk.stats.maxGroups, this.sk.live.size);
    this.sk.log.push(Math.round(performance.now()) + ':' + kind);
    this.setState({ skySlots: this.sk.slots.slice() });
    const ms = Math.max(...parts.map((p) => p.ms)) + 120;
    this.skyLater(() => {
      if (!this.sk.live.delete(tok)) return;
      this.sk.slots = this.sk.slots.map((s) => (s.tok === tok ? { ...s, on: false, tok: 0 } : s));
      this.setState({ skySlots: this.sk.slots.slice() });
    }, ms);
  }

  /* ---- camera: a page change rotates, shifts, zooms, expands or warps the three
     depth layers at different rates, then flashes constellations now in view ---- */
  skyTransition(kind) {
    if (!this.sk || this.sk.busy || !this.skyLive()) return;
    kind = kind || this.skyPick(['rotate', 'shift', 'zoom', 'expand', 'warp']);
    const LIM = { far: { t: 20, s: 1.35 }, mid: { t: 320, s: 1.5 }, near: { t: 520, s: 1.4 } }, PAR = { far: 1, mid: 1.9, near: 3.2 }, ROT = { far: 1, mid: 0.6, near: 1.5 };
    const cl = (v, a, b) => Math.max(a, Math.min(b, v)), sg = this.sk.rand() < 0.5 ? -1 : 1, dx = this.skyR(-1, 1), dy = this.skyR(-0.7, 0.7);
    const to = {}, over = {};
    for (const k of ['far', 'mid', 'near']) {
      const c = this.sk.cam[k], L = LIM[k], n = { ...c };
      if (kind === 'rotate') n.r += sg * this.skyR(24, 46) * ROT[k];
      else if (kind === 'shift') { n.x = cl(c.x + dx * L.t * 0.8, -L.t, L.t); n.y = cl(c.y + dy * L.t * 0.8, -L.t, L.t); n.r += sg * this.skyR(4, 10) * ROT[k]; }
      else if (kind === 'zoom') { n.s = cl(c.s * (1 + 0.16 * PAR[k]), 1, L.s); n.r += sg * this.skyR(6, 14) * ROT[k]; }
      else if (kind === 'expand') { n.s = 1 + (c.s - 1) * 0.15; n.x *= 0.2; n.y *= 0.2; n.r += sg * this.skyR(10, 22) * ROT[k]; }
      else { n.s = cl(c.s * (1 + 0.15 * PAR[k]), 1, L.s); n.r += sg * this.skyR(14, 28) * ROT[k]; n.x = cl(dx * L.t * 0.4, -L.t, L.t); n.y = cl(dy * L.t * 0.4, -L.t, L.t); }
      to[k] = n;
      over[k] = { r: (c.r + n.r) / 2, x: (c.x + n.x) / 2, y: (c.y + n.y) / 2, s: Math.min(L.s, Math.max(c.s, n.s) * (1 + 0.3 * PAR[k] / 3.2)) };
    }
    this.sk.busy = true;
    const dur = kind === 'warp' ? 2200 : 1800;
    if (kind === 'warp') {
      this.setState({ skyCam: over, skyCamMs: dur / 2, skyCamEase: 'cubic-bezier(.55,0,.8,.45)', skyCamFade: true });
      this.skyLater(() => this.setState({ skyCam: to, skyCamMs: dur / 2, skyCamEase: 'cubic-bezier(.2,.55,.35,1)', skyCamFade: false }), dur / 2);
    } else {
      this.setState({ skyCam: to, skyCamMs: dur, skyCamEase: 'cubic-bezier(.65,0,.25,1)', skyCamFade: false });
    }
    this.sk.cam = to;
    this.skyLater(() => {
      const ids = this.skyVisible(to.far).slice(0, 4), gen = ++this.sk.flashGen;
      this.setState({ skyFlash: { gen, ids } });
      this.skyLater(() => { if (this.sk.flashGen === gen) this.setState({ skyFlash: null }); }, 4800);
    }, dur * 0.55);
    this.skyLater(() => { this.sk.busy = false; }, dur + 60);
  }
  /* Constellations whose centre lands on screen, in random order. The sphere turns
     once per 960 s round a pole at (54%, 40%). The far camera turns round the centre. */
  skyVisible(far) {
    const W = window.innerWidth, H = window.innerHeight, th = this.skyClock() / 960000 * 2 * Math.PI, rc = far.r * Math.PI / 180;
    const px = 0.54 * W, py = 0.4 * H, ids = [];
    this.skyCentres().forEach((c, i) => {
      const vx = c[0] - 1700, vy = c[1] - 1700;
      const ax = px + vx * Math.cos(th) - vy * Math.sin(th) - W / 2, ay = py + vx * Math.sin(th) + vy * Math.cos(th) - H / 2;
      const x = W / 2 + far.s * (ax * Math.cos(rc) - ay * Math.sin(rc)) + far.x, y = H / 2 + far.s * (ax * Math.sin(rc) + ay * Math.cos(rc)) + far.y;
      if (x > W * 0.08 && x < W * 0.92 && y > H * 0.1 && y < H * 0.9) ids.push(i);
    });
    for (let i = ids.length - 1; i > 0; i--) { const j = Math.floor(this.sk.rand() * (i + 1)); [ids[i], ids[j]] = [ids[j], ids[i]]; }
    return ids;
  }

  skyRenderVals() {
    const cam = this.state.skyCam, ms = this.state.skyCamMs, ease = this.state.skyCamEase;
    const t = (c, extra) => `transform:translate3d(${c.x.toFixed(1)}px,${c.y.toFixed(1)}px,0) rotate(${c.r.toFixed(2)}deg) scale(${c.s.toFixed(3)});transition:transform ${ms}ms ${ease}${extra || ''}`;
    const fl = this.state.skyFlash, out = {
      skyClass: this.state.sky === 'still' ? 'still' : '',
      skyCamFar: t(cam.far),
      skyCamMid: t(cam.mid),
      skyCamNear: t(cam.near, `,opacity ${ms}ms ${ease};opacity:${this.state.skyCamFade ? 0.45 : 1}`),
      skySlots: this.state.skySlots.map((s) => ({ cls: 'ev ' + (s.on ? s.kind : 'off'), outer: s.on ? s.outer : '', inner: s.on ? s.inner : '' }))
    };
    this.skyCentres().forEach((c, i) => {
      const k = fl ? fl.ids.indexOf(i) : -1;
      out['skyHi' + i] = k < 0 ? '' : (fl.gen % 2 ? 'flA' : 'flB') + (k ? ' d' + k : '');
    });
    return out;
  }
}

/* ---- harness: mounts SkyLogic and applies its values to the page ----
   app.js drives it through window.sky: setMoving() for the Sky setting and
   transition() on view changes. */
(function(){
  var comp = new SkyLogic(), skyEl = document.getElementById('sky');
  if (!skyEl) return;
  var cams = { far: skyEl.querySelector('.cam-far'), mid: skyEl.querySelector('.cam-mid'), near: skyEl.querySelector('.cam-near') };
  var slots = [].slice.call(skyEl.querySelectorAll('.ev')), his = [].slice.call(skyEl.querySelectorAll('.hi'));
  /* Write an attribute only when its value changed. */
  function set(el, prop, v) { if (el[prop] !== v) el[prop] = v; }
  function css(el, v) { if (el.__css !== v) { el.__css = v; el.style.cssText = v; } }
  function render() {
    var v = comp.skyRenderVals();
    set(skyEl, 'className', ('sky ' + v.skyClass).trim());
    css(cams.far, v.skyCamFar); css(cams.mid, v.skyCamMid); css(cams.near, v.skyCamNear);
    v.skySlots.forEach(function (s, i) { set(slots[i], 'className', s.cls); css(slots[i], s.outer); css(slots[i].firstChild, s.inner); });
    his.forEach(function (h, i) { set(h, 'className', ('hi ' + v['skyHi' + i]).trim()); });
  }
  comp.setState = function (p) { Object.assign(comp.state, p); render(); };
  comp.componentDidMount();
  render();
  addEventListener('sky:transition', function (e) { comp.skyTransition(e && e.detail && e.detail.kind); });
  addEventListener('pagehide', function () { comp.componentWillUnmount(); });
  window.sky = { comp: comp, transition: function (k) { comp.skyTransition(k); }, setMoving: function (on) { comp.skySetMoving(on); }, get stats() { return comp.sk.stats; } };
})();
