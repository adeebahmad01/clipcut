// Small enhancements for the website and docs. Every page works without it.

const REPO = 'adeebahmad01/clipcut';
const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];

// --- Header menu on small screens --------------------------------------------

function toggler(button, target, openClass, onChange = () => {}) {
  const set = (open) => {
    button.setAttribute('aria-expanded', String(open));
    target.classList.toggle(openClass, open);
    if (openClass === 'side-open') document.documentElement.classList.toggle('locked', open);
    const scrim = $('.scrim');
    if (scrim && openClass === 'side-open') scrim.hidden = !open;
    onChange(open);
  };
  button.addEventListener('click', () => set(button.getAttribute('aria-expanded') !== 'true'));
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && button.getAttribute('aria-expanded') === 'true') { set(false); button.focus(); }
  });
  return set;
}

const menuBtn = $('.menu-btn');
if (menuBtn) {
  const setMenu = toggler(menuBtn, $('.top'), 'menu-open');
  $$('#main-nav a').forEach((a) => a.addEventListener('click', () => setMenu(false)));
}

const sideBtn = $('.side-btn');
if (sideBtn) {
  const panel = $('#sidebar');
  const small = matchMedia('(max-width: 860px)');
  const background = [$('.top'), $('.doc'), $('.toc'), $('footer')].filter(Boolean);
  const setSide = toggler(sideBtn, document.body, 'side-open', (open) => {
    background.forEach((el) => { el.inert = open; });
    if (open) {
      panel.setAttribute('role', 'dialog');
      panel.setAttribute('aria-modal', 'true');
      $('#q').focus();
    } else {
      panel.removeAttribute('role');
      panel.removeAttribute('aria-modal');
      if (panel.contains(document.activeElement)) sideBtn.focus();
    }
  });
  $('.side-close').addEventListener('click', () => setSide(false));
  $('.scrim').addEventListener('click', () => setSide(false));
  $$('.side-nav a').forEach((a) => a.addEventListener('click', () => setSide(false)));
  small.addEventListener('change', () => setSide(false));
  panel.addEventListener('keydown', (e) => {
    if (e.key !== 'Tab' || !small.matches || sideBtn.getAttribute('aria-expanded') !== 'true') return;
    const stops = $$('a[href], button, input', panel);
    const first = stops[0];
    const last = stops.at(-1);
    if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
    if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
  });
}

// --- Landing page: the visitor's download and the latest version -------------

function detectTarget() {
  const ua = navigator.userAgent;
  const platform = (navigator.userAgentData && navigator.userAgentData.platform) || navigator.platform || '';
  if (/Win/i.test(platform) || /Windows/i.test(ua)) return { label: 'Windows (WSL2)', target: null, win: true };
  if (/Mac/i.test(platform) || /Mac OS X/i.test(ua)) {
    // Browsers report Intel on every Mac, so default to the common case.
    return { label: 'macOS', target: 'aarch64-apple-darwin' };
  }
  if (/Linux/i.test(platform) || /Linux/i.test(ua)) {
    const arm = /aarch64|arm64|armv8/i.test(platform + ' ' + ua);
    return { label: arm ? 'Linux ARM64' : 'Linux', target: arm ? 'aarch64-unknown-linux-gnu' : 'x86_64-unknown-linux-gnu' };
  }
  return null;
}

if ($('#hero-os')) {
  const found = detectTarget();
  if (found) {
    $('#hero-os').textContent = found.label;
    const card = found.win ? $('.plat.win') : $(`.plat[data-target="${found.target}"]`);
    if (card) card.classList.add('mine');
  }
  fetch(`https://api.github.com/repos/${REPO}/releases/latest`)
    .then((r) => (r.ok ? r.json() : null))
    .then((rel) => {
      if (!rel || !rel.tag_name) return;
      const link = document.createElement('a');
      link.href = `https://github.com/${REPO}/releases/latest`;
      link.textContent = rel.tag_name;
      $('#version-line').replaceChildren('Latest: ', link, ' · Linux and macOS · Windows via WSL2 · MIT license');
    })
    .catch(() => {});
}

// --- Copy buttons ------------------------------------------------------------

async function copy(btn, text) {
  try {
    await navigator.clipboard.writeText(text);
    btn.textContent = 'Copied';
  } catch {
    // Clipboard API is unavailable on an HTTP LAN preview. Select the actual
    // source instead of merely telling the user to copy an unselected block.
    const source = btn.dataset.copy
      ? document.getElementById(btn.dataset.copy)
      : $('code', btn.closest('.code'));
    const range = document.createRange();
    range.selectNodeContents(source);
    const selection = window.getSelection();
    selection.removeAllRanges();
    selection.addRange(range);
    btn.textContent = 'Select and copy';
  }
  setTimeout(() => { btn.textContent = 'Copy'; }, 1800);
}

$$('[data-copy]').forEach((btn) => {
  btn.addEventListener('click', () => copy(btn, document.getElementById(btn.dataset.copy).textContent));
});

$$('.prose .code').forEach((block) => {
  const btn = document.createElement('button');
  btn.type = 'button';
  btn.className = 'copy';
  btn.textContent = 'Copy';
  btn.addEventListener('click', () => copy(btn, $('code', block).textContent));
  block.append(btn);
});

// --- Docs: highlight the section in view ------------------------------------

const tocLinks = $$('.toc a');
if (tocLinks.length && 'IntersectionObserver' in window) {
  const byId = new Map(tocLinks.map((a) => [decodeURIComponent(a.hash.slice(1)), a]));
  const visible = new Set();
  const heads = [...byId.keys()].map((id) => document.getElementById(id)).filter(Boolean);
  const mark = () => {
    const first = heads.find((h) => visible.has(h)) || [...heads].reverse().find((h) => h.getBoundingClientRect().top < 120);
    tocLinks.forEach((a) => a.classList.toggle('on', first && byId.get(first.id) === a));
  };
  const io = new IntersectionObserver((entries) => {
    entries.forEach((e) => (e.isIntersecting ? visible.add(e.target) : visible.delete(e.target)));
    mark();
  }, { rootMargin: '-72px 0px -60% 0px' });
  heads.forEach((h) => io.observe(h));
}

// --- Docs: search ------------------------------------------------------------

const q = $('#q');
if (q) {
  const box = $('#q-results');
  const status = $('#q-status');
  let index = null;
  let active = -1;
  let searchRun = 0;
  const load = () => {
    index ||= fetch(document.body.dataset.search)
      .then((r) => { if (!r.ok) throw new Error('Search unavailable'); return r.json(); })
      .catch(() => { index = null; return null; });
    return index;
  };
  const esc = (s) => s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' })[c]);
  const snippet = (text, term) => {
    const i = text.toLowerCase().indexOf(term);
    const start = Math.max(0, i - 50);
    let s = (start ? '…' : '') + text.slice(start, start + 150) + (text.length > start + 150 ? '…' : '');
    s = esc(s);
    return i < 0 ? s : s.replace(new RegExp(`(${term.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')})`, 'ig'), '<mark>$1</mark>');
  };
  const open = (show) => {
    box.hidden = !show;
    q.setAttribute('aria-expanded', String(show));
    if (!show) {
      searchRun++;
      active = -1;
      q.removeAttribute('aria-activedescendant');
      status.textContent = '';
    }
  };
  const select = (i) => {
    const items = $$('a', box);
    if (!items.length) return;
    active = (i + items.length) % items.length;
    items.forEach((a, j) => a.setAttribute('aria-selected', String(j === active)));
    q.setAttribute('aria-activedescendant', items[active].id);
    items[active].scrollIntoView({ block: 'nearest' });
  };

  const run = async () => {
    const thisRun = ++searchRun;
    const terms = q.value.toLowerCase().split(/\s+/).filter(Boolean);
    if (!terms.length) { open(false); return; }
    const entries = await load();
    if (thisRun !== searchRun) return;
    if (!entries) {
      box.innerHTML = '<p class="none" role="option" aria-disabled="true" aria-selected="false">Search is unavailable. Browse the docs below or try again.</p>';
      status.textContent = 'Search is unavailable.';
      open(true);
      return;
    }
    const hits = [];
    for (const e of entries) {
      const t = e.t.toLowerCase();
      const x = e.x.toLowerCase();
      let score = 0;
      for (const term of terms) {
        if (t.includes(term)) score += 10;
        else if (x.includes(term)) score += 1;
        else { score = 0; break; }
      }
      if (score) hits.push([score, e]);
    }
    hits.sort((a, b) => b[0] - a[0]);
    active = -1;
    q.removeAttribute('aria-activedescendant');
    box.innerHTML = hits.length
      ? hits.slice(0, 8).map(([, e], i) => `<a id="q-hit-${i}" role="option" aria-selected="false" href="${e.u}"><b>${esc(e.t)}</b><span>${snippet(e.x, terms[0])}</span></a>`).join('')
      : `<p class="none" role="option" aria-disabled="true" aria-selected="false">No results for “${esc(q.value)}”</p>`;
    status.textContent = hits.length ? `${Math.min(8, hits.length)} results` : 'No results';
    open(true);
  };

  q.addEventListener('focus', load);
  q.addEventListener('input', run);
  q.addEventListener('keydown', (e) => {
    if (box.hidden) return;
    if (e.key === 'ArrowDown') { e.preventDefault(); select(active + 1); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); select(active < 0 ? -1 : active - 1); }
    else if (e.key === 'Enter') {
      const a = $$('a', box)[Math.max(active, 0)];
      if (a) { e.preventDefault(); location.href = a.href; }
    } else if (e.key === 'Escape' && !box.hidden) { e.stopPropagation(); q.value = ''; open(false); }
  });
  document.addEventListener('click', (e) => { if (!e.target.closest('.search')) open(false); });
  document.addEventListener('keydown', (e) => {
    const typing = /^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement.tagName);
    if ((e.key === '/' && !typing) || (e.key === 'k' && (e.metaKey || e.ctrlKey))) {
      e.preventDefault();
      if (sideBtn && getComputedStyle(sideBtn).display !== 'none' && sideBtn.getAttribute('aria-expanded') !== 'true') sideBtn.click();
      q.focus();
    }
  });
}
