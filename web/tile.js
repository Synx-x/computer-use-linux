// <desktop-tile>: one desktop on the wall (variant="wall") or in the focus rail (variant="rail").
// Light DOM on purpose, so the page's hover and :has() rules reach it.

const STATUS = {
  running: 'Running',
  needs: 'Needs you',
  idle: 'Idle',
};

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text != null) e.textContent = text;
  return e;
}

export function windowMock(lines, title) {
  const win = el('span', 'win');
  win.append(el('span', 'wt', title || ''));
  const wl = el('span', 'wl');
  for (const ln of lines || []) wl.append(el('span', '', ln));
  win.append(wl);
  return win;
}

class DesktopTile extends HTMLElement {
  #sig = '';
  #img = null;
  #live = false;

  /** Set the desktop record. Rebuilds only when something visible changed. */
  set data(d) {
    this.dataset.status = d.status;
    const sig = JSON.stringify([d.name, d.task, d.agent, d.last, d.ago, d.status, d.display, d.res, d.windowTitle, d.lines]);
    this.d = d;
    if (sig === this.#sig) return;
    this.#sig = sig;
    this.#build(d);
  }

  /** Real mode: swap in the latest thumbnail without a flicker. */
  refreshThumb() {
    const d = this.d;
    if (!d || !this.#img || d.status === 'idle') return;
    const probe = new Image();
    probe.onload = () => {
      if (!this.#img) return;
      this.#img.src = probe.src;
      this.#img.closest('.win').hidden = false;
      this.querySelector('.fallback')?.setAttribute('hidden', '');
    };
    probe.src = `/api/desktops/${encodeURIComponent(d.id)}/thumb.webp?t=${Date.now()}`;
  }

  #build(d) {
    const variant = this.getAttribute('variant') || 'wall';
    const rail = variant === 'rail';
    const mock = this.hasAttribute('mock');
    this.replaceChildren();
    this.#img = null;

    const hit = el('button', 'hit');
    hit.type = 'button';
    hit.setAttribute('aria-label', `Focus ${d.name}`);
    hit.addEventListener('click', () => this.#emit('watch'));

    if (d.status !== 'idle') {
      if (mock) {
        hit.append(windowMock(d.lines, d.windowTitle));
      } else {
        // Thumbnail first. The text mock shows until the first frame lands.
        const shot = el('span', 'win shot');
        shot.hidden = true;
        const img = new Image();
        img.alt = '';
        shot.append(img);
        this.#img = img;
        const fb = windowMock(d.lines, d.windowTitle);
        fb.classList.add('fallback');
        hit.append(shot, fb);
      }
    } else {
      hit.append(el('span', 'idle-note', 'Waiting for an agent'));
    }
    this.append(hit);

    const tl = el('div', 'tl');
    const st = el('span', 'st');
    st.style.color = d.status === 'needs' ? 'var(--amber)' : d.status === 'idle' ? 'var(--mute)' : 'var(--ink-3)';
    const dot = el('span', `dot ${d.status}`);
    st.append(dot, document.createTextNode(STATUS[d.status] || d.status));
    tl.append(st, el('span', 'meta', rail ? d.display : `${d.display} · ${d.res}`));
    this.append(tl);

    const band = el('div', 'band');
    const who = el('div', 'who');
    who.append(el('div', 'nm', d.name), el('div', 'tk', d.task));
    if (!rail) who.append(el('div', 'ag', `${d.agent} · ${d.last}, ${d.ago} ago`));
    band.append(who);
    if (rail) {
      band.append(el('span', 'go', 'Focus'));
      band.style.pointerEvents = 'none';
    } else {
      const acts = el('span', 'acts');
      const drive = el('button', '', 'Take over');
      drive.type = 'button';
      drive.addEventListener('click', () => this.#emit('drive'));
      const go = el('button', 'go', 'Focus');
      go.type = 'button';
      go.addEventListener('click', () => this.#emit('watch'));
      acts.append(drive, go);
      band.append(acts);
    }
    this.append(band);
    if (this.#img) this.refreshThumb();
  }

  #emit(mode) {
    this.dispatchEvent(new CustomEvent('focus-desktop', { bubbles: true, detail: { id: this.d.id, mode } }));
  }
}

customElements.define('desktop-tile', DesktopTile);
