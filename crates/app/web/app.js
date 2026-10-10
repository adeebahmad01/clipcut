// ClipCut web UI. Plain ES module: no framework, no build step, no CDN, works
// fully offline. Served from the binary next to index.html and style.css.
//
// NOTES FOR THE SERVER (beyond docs/API.md):
// - PUT /api/keys/{service} receives `{ keys: [5 strings] }`. "" keeps the key
//   already in that slot (as in API.md). The UI also sends the sentinel
//   "__remove__" for a slot the user removed: the server must drop that key
//   (later keys shift up). Slots past the end that are "" are ignored.
// - Media URLs carry a `?v=` cache-busting query string, so the files and
//   still endpoints must ignore query parameters other than `t`.
// - Candidate thumbnail URLs must be http(s).
//
// Layout of this file: utilities -> icons -> API + store -> live events ->
// shared widgets -> views (home, project, settings) -> router/boot.

// ------------------------------------------------------------- utilities --

const $ = (sel, root = document) => root.querySelector(sel);

/** Tiny element builder: h('div', { class, onclick, ... }, ...children). */
function h(tag, props, ...kids) {
  const el = document.createElement(tag);
  if (props) {
    for (const [k, v] of Object.entries(props)) {
      if (v == null || v === false) continue;
      if (k === 'class') el.className = v;
      else if (k === 'style' && typeof v === 'object') Object.assign(el.style, v);
      else if (k.startsWith('on') && typeof v === 'function') el.addEventListener(k.slice(2), v);
      else if (k === 'value' || k === 'checked' || k === 'muted' || k === 'selected' || k === 'indeterminate') el[k] = v;
      else if (k === 'href' && !safeLink(v)) continue;
      else if (v === true) el.setAttribute(k, '');
      else el.setAttribute(k, String(v));
    }
  }
  append(el, kids);
  return el;
}
function append(el, kids) {
  for (const kid of kids.flat(Infinity)) {
    if (kid == null || kid === false || kid === true) continue;
    el.append(kid instanceof Node ? kid : document.createTextNode(String(kid)));
  }
  return el;
}

function safeLink(value) {
  if (typeof value !== 'string') return false;
  try { return ['http:', 'https:'].includes(new URL(value, location.href).protocol); } catch { return false; }
}
function focusSnapshot(root) {
  const active = document.activeElement;
  if (!active?.id || !root.contains(active)) return null;
  return { id: active.id, start: active.selectionStart, end: active.selectionEnd, direction: active.selectionDirection };
}
function restoreFocus(saved) {
  if (!saved) return;
  const el = document.getElementById(saved.id);
  if (!el || el.disabled || el.closest('[hidden]')) return;
  el.focus({ preventScroll: true });
  if (saved.start != null && typeof el.setSelectionRange === 'function') {
    try { el.setSelectionRange(saved.start, saved.end, saved.direction || 'none'); } catch { /* select/number input */ }
  }
}
function disposeTree(root) {
  if (voicePreview.button && root.contains(voicePreview.button)) stopVoicePreview();
  for (const media of root.querySelectorAll('audio, video')) {
    media.pause();
    videoObserver?.unobserve(media);
  }
}

/** Re-build `el`'s children only when `sig` changed. */
function patch(el, sig, build) {
  if (el._sig === sig) return false;
  const saved = focusSnapshot(el);
  disposeTree(el);
  el._sig = sig;
  el.replaceChildren(...[build()].flat(Infinity).filter((n) => n != null && n !== false));
  restoreFocus(saved);
  return true;
}

/** Keyed list: reuses children whose signature did not change. */
function keyed(container, items, keyOf, sigOf, build) {
  const old = new Map();
  for (const c of [...container.children]) old.set(c._key, c);
  let prev = null;
  for (const item of items) {
    const k = keyOf(item);
    const s = sigOf(item);
    let el = old.get(k);
    old.delete(k);
    if (!el || el._sig !== s) {
      const saved = el && focusSnapshot(el);
      if (el) disposeTree(el);
      const n = build(item);
      n._key = k;
      n._sig = s;
      if (el) el.replaceWith(n);
      el = n;
      restoreFocus(saved);
    }
    const want = prev ? prev.nextSibling : container.firstChild;
    if (el !== want) container.insertBefore(el, want);
    prev = el;
  }
  for (const el of old.values()) { disposeTree(el); el.remove(); }
}

const timers = new Map();
function debounce(key, fn, ms) {
  clearTimeout(timers.get(key));
  timers.set(key, setTimeout(() => { timers.delete(key); fn(); }, ms));
}

const clamp = (v, a, b) => Math.min(b, Math.max(a, v));
const pct = (x) => `${Math.round(clamp(x || 0, 0, 1) * 100)}%`;
const words = (s) => (String(s || '').trim().match(/\S+/g) || []).length;
const WORDS_PER_SECOND = 2.6;

function fmtClock(s) {
  if (s == null || !isFinite(s)) return '–';
  const m = Math.floor(s / 60);
  const sec = s - m * 60;
  return `${m}:${sec.toFixed(1).padStart(4, '0')}`;
}
/** Timestamp in a long video: 4:05 or 1:02:05. */
function fmtStamp(s) {
  s = Math.max(0, Math.floor(s || 0));
  const hh = Math.floor(s / 3600);
  const mm = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, '0');
  return hh ? `${hh}:${String(mm).padStart(2, '0')}:${ss}` : `${mm}:${ss}`;
}
function fmtDuration(s) {
  if (s == null) return '';
  s = Math.round(s);
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${String(s % 60).padStart(2, '0')}s`;
}
function relTime(iso) {
  if (!iso) return '';
  const d = (Date.now() - new Date(iso).getTime()) / 1000;
  if (d < 45) return 'just now';
  if (d < 3600) return `${Math.round(d / 60)} min ago`;
  if (d < 86400) return `${Math.round(d / 3600)} h ago`;
  if (d < 86400 * 7) return `${Math.round(d / 86400)} d ago`;
  return new Date(iso).toLocaleDateString();
}
function hashStr(s) {
  let x = 0;
  for (let i = 0; i < s.length; i++) x = (x * 31 + s.charCodeAt(i)) | 0;
  return (x >>> 0).toString(36);
}

const fileUrl = (id, path, v) =>
  `/api/projects/${encodeURIComponent(id)}/files/${path.split('/').map(encodeURIComponent).join('/')}${v ? `?v=${v}` : ''}`;

async function copyText(text, what = 'Text') {
  const saved = focusSnapshot(document.body);
  const active = document.activeElement;
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    // Clipboard API needs a secure context; LAN http falls back to execCommand.
    const ta = h('textarea', { style: { position: 'fixed', opacity: '0' } }, text);
    document.body.append(ta);
    ta.select();
    let copied = false;
    try { copied = document.execCommand('copy'); } catch { /* unsupported */ }
    ta.remove();
    active?.focus?.({ preventScroll: true });
    restoreFocus(saved);
    if (!copied) { toast('Copy is unavailable here. Select the text and copy it manually.', 'error'); return; }
  }
  toast(`${what} copied`);
}

// ------------------------------------------------------------------ icons --

const ICONS = {
  plus: 'M12 5v14M5 12h14',
  play: 'M7 4.5v15l12.5-7.5z',
  check: 'M5 12.5l4.5 4.5L19 7',
  x: 'M6 6l12 12M18 6L6 18',
  alert: 'M12 9v4M12 16.5v.5M10.3 3.9L2.2 18a2 2 0 001.7 3h16.2a2 2 0 001.7-3L13.7 3.9a2 2 0 00-3.4 0z',
  info: 'M12 3a9 9 0 100 18 9 9 0 000-18zM12 11v5M12 7.6v.4',
  sliders: 'M4 21v-7M4 10V3M12 21v-9M12 8V3M20 21v-5M20 12V3M1 14h6M9 8h6M17 16h6',
  back: 'M19 12H5M11 18l-6-6 6-6',
  swap: 'M17 3l4 4-4 4M3 11V9a2 2 0 012-2h16M7 21l-4-4 4-4M21 13v2a2 2 0 01-2 2H3',
  sparkles: 'M12 3l1.8 5.2L19 10l-5.2 1.8L12 17l-1.8-5.2L5 10l5.2-1.8zM19 15.5l.7 1.8 1.8.7-1.8.7-.7 1.8-.7-1.8-1.8-.7 1.8-.7z',
  upload: 'M12 15V4M7 8.5L12 4l5 4.5M4 16v3a2 2 0 002 2h12a2 2 0 002-2v-3',
  download: 'M12 4v11M7 10.5l5 4.5 5-4.5M4 16v3a2 2 0 002 2h12a2 2 0 002-2v-3',
  copy: 'M9 9h11v11H9zM5 15H4V4h11v1',
  lock: 'M5 11h14v10H5zM8 11V7a4 4 0 018 0v4',
  unlock: 'M5 11h14v10H5zM8 11V7a4 4 0 017.8-1.2',
  edit: 'M4 20h4L19 9l-4-4L4 16zM13.5 6.5l4 4',
  trash: 'M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3',
  search: 'M11 4a7 7 0 100 14 7 7 0 000-14zM20 20l-4-4',
  stop: 'M7 7h10v10H7z',
  rerun: 'M3 12a9 9 0 109-9 9.7 9.7 0 00-6.7 2.8L3 8M3 3v5h5',
  eye: 'M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12zM12 9a3 3 0 100 6 3 3 0 000-6z',
  eyeOff: 'M3 3l18 18M10.6 5.1A9.9 9.9 0 0112 5c6.5 0 10 7 10 7a17 17 0 01-3.2 4.2M6.6 6.6A17 17 0 002 12s3.5 7 10 7a9.7 9.7 0 005.4-1.6M9.9 9.9a3 3 0 004.2 4.2',
  film: 'M5 3h14v18H5zM5 8h3M5 12h3M5 16h3M16 8h3M16 12h3M16 16h3M8 3v18M16 3v18',
  chevronDown: 'M6 9l6 6 6-6',
  chevronRight: 'M9 6l6 6-6 6',
  external: 'M14 4h6v6M20 4l-9 9M18 14v5a1 1 0 01-1 1H5a1 1 0 01-1-1V7a1 1 0 011-1h5',
  key: 'M7.5 11a4.5 4.5 0 100 9 4.5 4.5 0 000-9zM10.7 12.3L20 3M16.5 6.5l3 3M14 9l2 2',
  wrench: 'M14.7 6.3a4 4 0 00-5.4 5.4L3 18l3 3 6.3-6.3a4 4 0 005.4-5.4l-2.6 2.6-2.4-.6-.6-2.4z',
  bolt: 'M13 2L4 14h7l-1 8 9-12h-7z',
  doc: 'M6 2h9l5 5v15H6zM14 2v6h6M9 13h8M9 17h6',
  wave: 'M3 12h1M7 8v8M11 4v16M15 7v10M19 10v4',
  layers: 'M12 2l10 5-10 5L2 7zM2 12l10 5 10-5M2 17l10 5 10-5',
  phone: 'M8 2h8a2 2 0 012 2v16a2 2 0 01-2 2H8a2 2 0 01-2-2V4a2 2 0 012-2zM11 18h2',
  monitor: 'M3 5h18v11H3zM8 20h8M12 16v4',
  dup: 'M8 8h12v12H8zM4 16V4h12',
  dots: 'M5 12h.01M12 12h.01M19 12h.01',
  volume: 'M11 5L6 9H3v6h3l5 4zM15.5 8.5a5 5 0 010 7M18.5 5.5a9 9 0 010 13',
  image: 'M3 5h18v14H3zM3 16l5-5 4 4 3-3 6 6M15.5 9.5a.5.5 0 100-1 .5.5 0 000 1z',
  text: 'M5 6V4h14v2M12 4v16M9 20h6',
  globe: 'M12 3a9 9 0 100 18 9 9 0 000-18zM3 12h18M12 3c2.5 2.5 3.8 5.5 3.8 9s-1.3 6.5-3.8 9c-2.5-2.5-3.8-5.5-3.8-9S9.5 5.5 12 3z',
  list: 'M9 6h11M9 12h11M9 18h11M4.5 6h.01M4.5 12h.01M4.5 18h.01',
  scissors: 'M6 3a3 3 0 100 6 3 3 0 000-6zM6 15a3 3 0 100 6 3 3 0 000-6zM8.1 8.1L20 20M8.1 15.9L20 4',
  send: 'M22 2L11 13M22 2l-7 20-4-9-9-4z',
  calendar: 'M4 5h16v16H4zM4 10h16M8 3v4M16 3v4',
  pause: 'M8 5v14M16 5v14',
  square: 'M4 4h16v16H4z',
  trend: 'M3 17l6-6 4 4 8-8M15 7h6v6',
  youtube: 'M2.6 7.6a3 3 0 012.5-2.7C7.4 4.6 9.7 4.5 12 4.5s4.6.1 6.9.4a3 3 0 012.5 2.7c.2 1.5.3 2.9.3 4.4s-.1 2.9-.3 4.4a3 3 0 01-2.5 2.7c-2.3.3-4.6.4-6.9.4s-4.6-.1-6.9-.4a3 3 0 01-2.5-2.7C2.4 14.9 2.3 13.5 2.3 12s.1-2.9.3-4.4zM10 9v6l5-3z',
};
const FILLED = new Set(['play', 'bolt']);

function icon(name, cls = '') {
  const t = document.createElement('template');
  const fill = FILLED.has(name);
  t.innerHTML = `<svg class="icon ${cls}" viewBox="0 0 24 24" aria-hidden="true" focusable="false" fill="${fill ? 'currentColor' : 'none'}" stroke="currentColor" stroke-width="${fill ? 0 : 2}" stroke-linecap="round" stroke-linejoin="round"><path d="${ICONS[name] || ''}"/></svg>`;
  return t.content.firstChild;
}

// --------------------------------------------------------------- domain ---

const STAGES = [
  ['research', 'Research'], ['hooks', 'Hook'], ['script', 'Script'], ['beats', 'Beat sheet'],
  ['assets', 'Visuals'], ['voice', 'Voiceover'], ['render', 'Render'],
];
const STAGE_LABEL = Object.fromEntries(STAGES);
const STAGE_IDS = STAGES.map(([id]) => id);
/** Stages the pipeline pauses after when autopilot is off, with the ask. */
const REVIEW = { hooks: 'Pick a hook', script: 'Review the script', assets: 'Review the visuals' };

const KINDS = {
  youtube: { label: 'YouTube', desc: '1–3 s cut from a YouTube video' },
  article: { label: 'Article', desc: 'News/article screenshot with a highlighted quote' },
  meme: { label: 'Meme', desc: 'Meme template with a caption' },
  gif: { label: 'GIF', desc: 'Reaction GIF' },
  image: { label: 'Image', desc: 'Photo of a person, product or place' },
  stock: { label: 'Stock', desc: 'Stock footage (last resort)' },
  text: { label: 'Text', desc: 'Kinetic typography + icon; always works' },
};
const KIND_IDS = Object.keys(KINDS);

const BEAT_STATUS = {
  pending: 'Pending', searching: 'Searching', ready: 'Ready', fallback: 'Fallback', failed: 'Failed',
};

const FORMATS = [
  ['vertical', '9:16', 'phone', '9:16 vertical'],
  ['horizontal', '16:9', 'monitor', '16:9 horizontal'],
  ['square', '1:1', 'square', '1:1 square'],
];
const FMT_LABEL = Object.fromEntries(FORMATS.map(([id, ratio]) => [id, ratio]));
const FMT_LONG = Object.fromEntries(FORMATS.map(([id, , , long]) => [id, long]));
/** CSS modifier for media boxes: '' for vertical, else the format name. */
const fmtClass = (format) => (format && format !== 'vertical' ? format : '');
const CAPTION_PRESETS = [
  ['pop', 'Pop', 'Bold words that pop in'],
  ['hormozi', 'Hormozi', 'Huge yellow keywords, two words at a time'],
  ['mrbeast', 'MrBeast', 'Chunky outlined text with a bounce'],
  ['minimal', 'Minimal', 'Small clean lower captions'],
  ['karaoke', 'Karaoke', 'Full line, the spoken word lights up'],
];
const PRIVACY = [['public', 'Public', 'globe'], ['unlisted', 'Unlisted', 'eyeOff'], ['private', 'Private', 'lock']];

/** `datetime-local` value -> ISO string (or null when empty/invalid). */
function localToIso(value) {
  if (!value) return null;
  const d = new Date(value);
  return isNaN(d) ? null : d.toISOString();
}
/** A Date in local time, formatted for a `datetime-local` input. */
function localInputValue(date) {
  const pad = (n) => String(n).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
}
const fmtWhen = (iso) => (iso ? new Date(iso).toLocaleString([], { dateStyle: 'medium', timeStyle: 'short' }) : '');

/** Summarize a stage list into one state for chips and controls. */
function pipelineState(stages = [], running = false) {
  const by = Object.fromEntries(stages.map((s) => [s.stage, s]));
  const list = STAGE_IDS.map((id) => by[id] || { stage: id, status: 'pending', progress: 0 });
  const runningStage = list.find((s) => s.status === 'running');
  const failed = list.find((s) => s.status === 'failed');
  const firstPending = list.find((s) => s.status === 'pending');
  const doneCount = list.filter((s) => s.status === 'done' || s.status === 'skipped').length;
  const overall = list.reduce((a, s) => a + (s.status === 'done' || s.status === 'skipped' ? 1 : s.status === 'running' ? s.progress || 0 : 0), 0) / list.length;
  if (running) {
    const cur = runningStage || firstPending || list.at(-1);
    return { kind: 'running', stage: cur.stage, label: `${STAGE_LABEL[cur.stage]}…`, overall, list };
  }
  if (failed) return { kind: 'failed', stage: failed.stage, label: `${STAGE_LABEL[failed.stage]} failed`, error: failed.error, overall, list };
  if (doneCount === list.length) return { kind: 'done', stage: null, label: 'Done', overall: 1, list };
  if (doneCount === 0) return { kind: 'idle', stage: 'research', label: 'Not started', overall: 0, list };
  const lastDone = [...list].reverse().find((s) => s.status === 'done' || s.status === 'skipped');
  if (REVIEW[lastDone.stage]) return { kind: 'paused', stage: lastDone.stage, next: firstPending?.stage, label: REVIEW[lastDone.stage], overall, list };
  return { kind: 'stopped', stage: firstPending?.stage, next: firstPending?.stage, label: `Stopped before ${STAGE_LABEL[firstPending?.stage] || 'end'}`, overall, list };
}

// ------------------------------------------------------------ API + store --

async function api(method, path, body) {
  const init = { method, headers: {} };
  if (body instanceof FormData) init.body = body;
  else if (body !== undefined) {
    init.headers['content-type'] = 'application/json';
    init.body = JSON.stringify(body);
  }
  let res;
  try {
    res = await fetch(`/api${path}`, init);
  } catch {
    throw new Error('Cannot reach the ClipCut server. Is it still running?');
  }
  const ct = res.headers.get('content-type') || '';
  if (!res.ok) {
    let msg = `${res.status} ${res.statusText}`;
    try {
      if (ct.includes('json')) msg = (await res.json()).error || msg;
      else msg = (await res.text()).slice(0, 300) || msg;
    } catch { /* keep status text */ }
    const e = new Error(msg);
    e.status = res.status;
    throw e;
  }
  if (res.status === 204) return null;
  return ct.includes('json') ? res.json() : res.blob();
}

const state = {
  status: null,
  statusError: null,
  bannerDismissed: sessionStorage.getItem('cc-banner') === '1',
  config: null,
  configError: null,
  voices: [],
  projects: null,
  projectsError: null,
  project: null, // the open project (full Project)
  projectError: null,
  keys: null,
  keySaving: null,
  logs: new Map(), // project id -> [{ at, stage, beat_id, message, level, progress }]
  live: new Map(), // project id -> latest stage progress event
  beatLive: new Map(), // `${pid}/${beat}` -> { message, level, at }
  busy: new Map(), // `${pid}/${beat}` -> label of in-flight request
  actions: new Map(), // project id -> current mutation
  running: new Map(), // full Project does not include running; summaries/SSE do
  setup: { log: [], progress: null, running: null, pending: new Set(), uncertain: false },
  sse: 'connecting',
  queue: null, // { items, paused }
  queueError: null,
  youtube: null, // publish auth status
  publishing: new Set(), // project ids with an upload in flight
};

let revision = 0;
let projectsRequest = 0;
let keysRequest = 0;
const projectRequests = new Map();
function setRunning(id, running) {
  state.running.set(id, { running: !!running, revision: ++revision });
  if (state.project?.id === id) state.project.running = !!running;
  const summary = state.projects?.find((p) => p.id === id);
  if (summary) summary.running = !!running;
}
function isRunning(id) {
  return state.running.get(id)?.running ?? (state.project?.id === id && !!state.project.running);
}
function projectBusy(id) { return state.actions.has(id); }
function beatsBusy(id) { return [...state.busy.keys()].some((k) => k.startsWith(`${id}/`)); }
function editBlocked(id) { return isRunning(id) || projectBusy(id); }
function nextProjectRequest(id) {
  const request = (projectRequests.get(id) || 0) + 1;
  projectRequests.set(id, request);
  return request;
}
function acceptProject(p, request) {
  if (!p || currentView?.projectId !== p.id || projectRequests.get(p.id) !== request) return;
  const running = state.running.get(p.id);
  p.running = running ? running.running : p.stages.some((s) => s.status === 'running');
  if (state.project?.id === p.id && state.project.updated_at > p.updated_at) return;
  state.project = p;
  state.projectError = null;
}
function renderReady(p) {
  return !!p.render && !p.running && stageStatus(p, 'render') === 'done'
    && sessionStorage.getItem(`cc-stale-render-${p.id}`) !== p.render.rendered_at;
}
function invalidateRender(pid) {
  const p = state.project?.id === pid && state.project;
  if (p?.render) sessionStorage.setItem(`cc-stale-render-${pid}`, p.render.rendered_at);
}
function dirtyProject(ui, p) {
  if (ui.scriptDraft != null && ui.scriptDraft !== p.script?.text) return 'script';
  if (ui.meta && ['title', 'description', 'hashtags'].some((k) =>
    ui.meta[k] !== (k === 'hashtags' ? (p.script?.hashtags || []).join(' ') : p.script?.[k] || ''))) return 'script';
  if (ui.projectDraft && (['topic', 'notes', 'voice', 'format', 'target_seconds', 'caption_preset'].some((k) =>
    String(ui.projectDraft[k] ?? '') !== String(p[k] ?? ''))
    || JSON.stringify(ui.projectDraft.extra_formats || []) !== JSON.stringify(p.extra_formats || []))) return 'details';
  if (Object.entries(ui.edits).some(([id, e]) => {
    const v = p.beats.find((b) => b.id === id)?.visual;
    return v && (e.kind !== v.kind || e.query !== (v.query || '') || e.ost !== (v.on_screen_text || ''));
  })) return 'visuals';
  return null;
}

let currentView = null;
let updateQueued = false;
function update() {
  if (updateQueued) return;
  updateQueued = true;
  requestAnimationFrame(() => {
    updateQueued = false;
    renderChrome();
    currentView?.update();
  });
}

async function refreshStatus() {
  try { state.status = await api('GET', '/status'); state.statusError = null; } catch (e) { state.statusError = e.message; }
  update();
}
async function refreshProjects() {
  const request = ++projectsRequest;
  const started = revision;
  try {
    const projects = await api('GET', '/projects');
    if (request !== projectsRequest) return;
    for (const p of projects) {
      const known = state.running.get(p.id);
      if (!known || known.revision <= started) setRunning(p.id, p.running);
      else p.running = known.running;
    }
    state.projects = projects;
    state.projectsError = null;
  } catch (e) {
    if (request === projectsRequest) state.projectsError = e.message;
  }
  update();
}
async function refreshProject(id) {
  const request = nextProjectRequest(id);
  try {
    const p = await api('GET', `/projects/${encodeURIComponent(id)}`);
    acceptProject(p, request);
  } catch (e) {
    if (currentView?.projectId === id && projectRequests.get(id) === request) state.projectError = e.message;
  }
  update();
}
async function loadConfig() {
  try { state.config = await api('GET', '/config'); state.configError = null; } catch (e) { state.configError = e.message; }
  update();
}
async function refreshQueue() {
  try { state.queue = await api('GET', '/queue'); state.queueError = null; } catch (e) { state.queueError = e.message; }
  update();
}
async function refreshYoutube() {
  try { state.youtube = await api('GET', '/publish/youtube'); } catch { /* keep the last known status */ }
  update();
  return state.youtube;
}
async function loadVoices() {
  try { state.voices = await api('GET', '/voices'); } catch { state.voices = []; }
  update();
}

/** Replace one beat in the open project with the server's copy. */
function putBeat(pid, beat) {
  const p = state.project;
  if (!p || p.id !== pid || !beat) return;
  const i = p.beats.findIndex((b) => b.id === beat.id);
  if (i >= 0) p.beats[i] = beat;
  update();
}

/** Run a beat request with a per-beat spinner. */
async function beatAction(pid, beatId, label, path, body) {
  const key = `${pid}/${beatId}`;
  if (state.busy.has(key) || editBlocked(pid)) return null;
  state.busy.set(key, label);
  update();
  try {
    const base = `/projects/${encodeURIComponent(pid)}/beats/${encodeURIComponent(beatId)}`;
    let beat = await api(path.method || 'POST', `${base}${path.sub || ''}`, body);
    if (!(path.method === 'PATCH' && body && Object.keys(body).every((k) => k === 'locked'))) invalidateRender(pid);
    if (path.autoAfter) {
      putBeat(pid, beat);
      beat = await api('POST', `${base}/auto`);
    }
    putBeat(pid, beat);
    refreshProjects();
    refreshProject(pid);
    return beat;
  } catch (e) {
    toast(`${beatId}: ${e.message}`, 'error');
    // A compound PATCH may have succeeded even if its subsequent pick failed.
    refreshProjects();
    refreshProject(pid);
    return null;
  } finally {
    state.busy.delete(key);
    update();
  }
}

// ------------------------------------------------------------ live events --

let es = null;
let reconnectTimer = null;
function connectEvents() {
  clearTimeout(reconnectTimer);
  es?.close();
  const source = es = new EventSource('/api/events');
  source.onopen = () => {
    if (source !== es) return;
    const wasDown = state.sse === 'down';
    state.sse = 'up';
    if (wasDown) {
      refreshProjects();
      if (currentView?.projectId) refreshProject(currentView.projectId);
      refreshStatus();
      refreshQueue();
    }
    update();
  };
  source.onerror = () => {
    if (source !== es) return;
    state.sse = 'down';
    if (state.setup.running) state.setup.uncertain = true;
    update();
    // EventSource retries on its own unless the server answered with an error.
    if (source.readyState === EventSource.CLOSED) {
      clearTimeout(reconnectTimer);
      reconnectTimer = setTimeout(connectEvents, 3000);
    }
  };
  const receive = (handler) => (e) => {
    if (source !== es) return;
    try { const value = JSON.parse(e.data); if (value && typeof value === 'object') handler(value); } catch { /* ignore malformed events */ }
  };
  source.addEventListener('progress', receive(onProgress));
  source.addEventListener('project', receive(onProjectEvent));
  source.addEventListener('queue', receive(onQueue));
}

function onQueue(q) {
  if (!Array.isArray(q.items)) return;
  const before = new Map((state.queue?.items || []).map((i) => [i.id, `${i.status}/${i.project_id}`]));
  state.queue = q;
  state.queueError = null;
  // New projects appear and finished ones change state as the worker moves on.
  if (q.items.some((i) => before.get(i.id) !== `${i.status}/${i.project_id}`)) debounce('projects', refreshProjects, 300);
  update();
}

function onProgress(ev) {
  if (typeof ev.project_id !== 'string' || typeof ev.message !== 'string') return;
  const at = Date.now();
  if (ev.project_id === '_setup') {
    state.setup.log.push({ ...ev, at });
    if (state.setup.log.length > 80) state.setup.log.shift();
    if (ev.progress != null) state.setup.progress = ev.progress;
    for (const target of state.setup.pending) {
      if (ev.message === `${target} ready` || (ev.level === 'error' && ev.message.startsWith(`${target}:`))) state.setup.pending.delete(target);
    }
    if (state.setup.running && !state.setup.pending.size) { state.setup.running = null; state.setup.uncertain = false; }
    debounce('status', refreshStatus, 500);
    update();
    return;
  }
  const log = state.logs.get(ev.project_id) || [];
  log.push({ ...ev, at });
  if (log.length > 50) log.shift();
  state.logs.set(ev.project_id, log);
  if (ev.stage) state.live.set(ev.project_id, { ...ev, at });
  if (ev.beat_id) {
    const key = `${ev.project_id}/${ev.beat_id}`;
    state.beatLive.set(key, { message: ev.message, level: ev.level, at });
    debounce(`beatlive-${key}`, update, 4100);
  }
  update();
}

function onProjectEvent(d) {
  if (typeof d.id !== 'string' || typeof d.running !== 'boolean') return;
  setRunning(d.id, d.running);
  nextProjectRequest(d.id); // a request predating this event must not overwrite it
  debounce('projects', refreshProjects, 300);
  if (currentView?.projectId === d.id) debounce('project', () => refreshProject(d.id), 300);
  update();
}

// ------------------------------------------------------------------ toasts --

function toast(message, kind = 'info', ms = kind === 'error' ? 6000 : 3000) {
  const box = $('#toasts');
  const el = h('div', { class: `toast toast-${kind}`, role: kind === 'error' ? 'alert' : 'status' },
    icon(kind === 'error' ? 'alert' : 'check'),
    h('span', { class: 'toast-msg' }, message),
    h('button', { class: 'icon-btn sm', 'aria-label': 'Dismiss', onclick: () => close() }, icon('x')));
  const close = () => {
    el.classList.add('out');
    setTimeout(() => el.remove(), 200);
  };
  box.append(el);
  while (box.children.length > 4) box.firstChild.remove();
  setTimeout(close, ms);
}

/** Promise-based confirm using <dialog>. */
function confirmDialog({ title, body, ok = 'Continue', danger = false }) {
  return new Promise((resolve) => {
    const opener = document.activeElement;
    const dlg = h('dialog', { class: 'modal', 'aria-labelledby': 'dlg-title' },
      h('form', { method: 'dialog', class: 'modal-body' },
        h('h2', { id: 'dlg-title' }, title),
        body && h('p', { class: 'muted' }, body),
        h('div', { class: 'modal-actions' },
          h('button', { class: 'btn ghost', value: 'cancel', autofocus: danger }, 'Cancel'),
          h('button', { class: `btn ${danger ? 'danger' : 'primary'}`, value: 'ok', autofocus: !danger }, ok))));
    dlg.addEventListener('close', () => {
      document.body.classList.remove('dialog-open');
      if (opener?.isConnected) opener.focus({ preventScroll: true });
      resolve(dlg.returnValue === 'ok');
      setTimeout(() => dlg.remove(), 200);
    });
    dlg.addEventListener('click', (e) => { if (e.target === dlg) dlg.close('cancel'); });
    document.body.append(dlg);
    document.body.classList.add('dialog-open');
    dlg.showModal();
  });
}

// ---------------------------------------------------------- shared widgets --

const spinner = (cls = '') => h('span', { class: `spinner ${cls}`, role: 'progressbar', 'aria-label': 'Working' });

function kindBadge(kind, extra = '') {
  return h('span', { class: `kind kind-${kind} ${extra}`, title: KINDS[kind]?.desc || kind }, KINDS[kind]?.label || kind);
}

function statusChip(st) {
  const cls = { running: 'chip-running', failed: 'chip-failed', done: 'chip-done', paused: 'chip-paused', stopped: 'chip-paused', idle: 'chip-idle' }[st.kind];
  return h('span', { class: `chip ${cls}` }, st.kind === 'running' ? h('span', { class: 'dot' }) : null,
    st.kind === 'done' ? icon('check') : st.kind === 'failed' ? icon('alert') : null, st.label);
}

function progressBar(value, cls = '') {
  return h('div', { class: `bar ${cls}`, role: 'progressbar', 'aria-valuemin': 0, 'aria-valuemax': 100, 'aria-valuenow': Math.round((value || 0) * 100) },
    h('div', { class: 'bar-fill', style: { width: pct(value) } }));
}

/** Accessible on/off switch built on a checkbox. */
function toggle({ label, checked, onchange, hint, id, disabled = false }) {
  id = id || `t-${Math.random().toString(36).slice(2, 8)}`;
  return h('label', { class: 'switch', for: id },
    h('input', { type: 'checkbox', id, role: 'switch', checked, disabled, onchange: (e) => onchange(e.target.checked) }),
    h('span', { class: 'switch-track', 'aria-hidden': 'true' }, h('span', { class: 'switch-thumb' })),
    h('span', { class: 'switch-text' }, h('span', { class: 'switch-label' }, label), hint && h('span', { class: 'switch-hint' }, hint)));
}

/** Segmented control (radio group). options: [[value, label, iconName?]] */
function segmented({ name, value, options, onchange, label }) {
  const wrap = h('div', { class: 'seg', role: 'radiogroup', 'aria-label': label });
  const draw = (v) => {
    wrap.replaceChildren(...options.map(([val, text, ic], i) =>
      h('button', {
        type: 'button', id: `seg-${name}-${i}`, class: 'seg-btn', role: 'radio', 'aria-checked': String(val === v), 'data-name': name,
        tabindex: val === v ? 0 : -1,
        onclick: () => { draw(val); onchange(val); wrap.children[i].focus({ preventScroll: true }); },
        onkeydown: (e) => {
          const j = radioKeyIndex(e, i, options.length);
          if (j == null) return;
          draw(options[j][0]); onchange(options[j][0]); wrap.children[j].focus();
        },
      }, ic && icon(ic), text)));
  };
  wrap.setValue = draw;
  draw(value);
  return wrap;
}

function radioKeyIndex(e, i, length) {
  const j = e.key === 'Home' ? 0 : e.key === 'End' ? length - 1
    : ['ArrowRight', 'ArrowDown'].includes(e.key) ? (i + 1) % length
      : ['ArrowLeft', 'ArrowUp'].includes(e.key) ? (i + length - 1) % length : null;
  if (j != null) e.preventDefault();
  return j;
}

const reducedMotion = matchMedia('(prefers-reduced-motion: reduce)');
const videoObserver = 'IntersectionObserver' in window
  ? new IntersectionObserver((entries) => {
    for (const e of entries) {
      if (e.isIntersecting && !reducedMotion.matches) e.target.play().catch(() => {});
      else e.target.pause();
    }
  }, { rootMargin: '120px' })
  : null;

/** Preview an asset: muted looping video or image. */
function assetMedia(pid, asset, cls = 'media') {
  if (!asset) return h('div', { class: `${cls} media-empty` }, icon('image'));
  const src = fileUrl(pid, asset.path, hashStr(JSON.stringify([asset.path, asset.source_url, asset.provider, asset.credit])));
  const fail = (e) => e.target.replaceWith(h('div', { class: `${cls} media-empty media-broken`, title: 'Preview failed to load' }, icon('alert')));
  if (asset.kind === 'video') {
    const v = h('video', { class: cls, muted: true, loop: true, playsinline: true, preload: 'metadata', disablepictureinpicture: true, onerror: fail });
    v.defaultMuted = true;
    v.src = asset.trim_start_s ? `${src}#t=${asset.trim_start_s}` : src;
    if (videoObserver) videoObserver.observe(v);
    else v.autoplay = !reducedMotion.matches;
    return v;
  }
  return h('img', { class: cls, src, alt: '', loading: 'lazy', decoding: 'async', onerror: fail });
}

function beatMedia(pid, beat, cls = 'media') {
  if (beat.visual.kind === 'text' && !beat.asset) return h('div', { class: `${cls} text-visual`, role: 'img',
    'aria-label': `Text visual: ${beat.visual.on_screen_text || beat.text}` }, h('span', null, beat.visual.on_screen_text || beat.text));
  return assetMedia(pid, beat.asset, cls);
}

// One audio preview owns its request, object URL and playback across all views.
const voicePreview = { sequence: 0, button: null, audio: null, url: null };
function stopVoicePreview() {
  voicePreview.sequence++;
  const button = voicePreview.button;
  voicePreview.button = null;
  if (voicePreview.audio) {
    voicePreview.audio.onended = voicePreview.audio.onpause = null;
    voicePreview.audio.pause();
  }
  if (voicePreview.url) URL.revokeObjectURL(voicePreview.url);
  voicePreview.audio = null;
  voicePreview.url = null;
  if (button) { button.disabled = false; button.replaceChildren(icon('play'), 'Preview'); button.setAttribute('aria-pressed', 'false'); }
}
function voicePreviewButton(getVoice, id) {
  const button = h('button', { type: 'button', id, class: 'btn ghost sm', 'aria-label': 'Preview voice',
    'aria-pressed': 'false', title: 'Preview uses the saved voice engine and speed' }, icon('play'), 'Preview');
  button.onclick = async () => {
    if (voicePreview.button === button) { stopVoicePreview(); return; }
    stopVoicePreview();
    voicePreview.button = button;
    const sequence = voicePreview.sequence;
    button.disabled = true;
    button.replaceChildren(spinner(), 'Preview');
    try {
      const blob = await api('POST', '/voices/preview', { voice: getVoice() || state.config?.tts?.voice, text: 'Here is what your next short will sound like.' });
      if (sequence !== voicePreview.sequence || !button.isConnected) return;
      voicePreview.url = URL.createObjectURL(blob);
      const audio = voicePreview.audio = new Audio(voicePreview.url);
      audio.onended = audio.onpause = () => { if (sequence === voicePreview.sequence) stopVoicePreview(); };
      await audio.play();
      if (sequence !== voicePreview.sequence) return;
      button.replaceChildren(icon('stop'), 'Stop');
      button.setAttribute('aria-pressed', 'true');
    } catch (e) {
      if (sequence === voicePreview.sequence) { stopVoicePreview(); toast(e.message, 'error'); }
    } finally {
      if (sequence === voicePreview.sequence) button.disabled = false;
    }
  };
  return button;
}

const UPLOAD_ACCEPT = '.mp4,.mov,.webm,.mkv,.gif,.m4v,.avi,.png,.jpg,.jpeg,.webp,.bmp';
const UPLOAD_LIMIT = 256 * 1024 * 1024;
function uploadBody(file) {
  if (!UPLOAD_ACCEPT.split(',').some((ext) => file.name.toLowerCase().endsWith(ext))) {
    toast('Choose an MP4, MOV, WebM, MKV, GIF, M4V, AVI, PNG, JPG, WebP or BMP file. SVG uploads are not supported.', 'error');
    return null;
  }
  if (!file.size || file.size > UPLOAD_LIMIT) { toast('Choose a non-empty file no larger than 256 MiB.', 'error'); return null; }
  const body = new FormData();
  body.append('file', file, file.name);
  return body;
}

function emptyState(ic, title, body, action) {
  return h('div', { class: 'empty' }, h('div', { class: 'empty-icon' }, icon(ic)), h('h3', null, title), body && h('p', null, body), action);
}

function errorState(message, retry) {
  return h('div', { class: 'empty error' }, h('div', { class: 'empty-icon' }, icon('alert')), h('h3', null, 'Something went wrong'),
    h('p', null, message), retry && h('button', { class: 'btn', onclick: retry }, icon('rerun'), 'Try again'));
}

function voiceOptions(selected, { includeDefault = true } = {}) {
  const engines = [...new Set(state.voices.map((v) => v.engine))];
  const opts = [];
  if (includeDefault) {
    const def = state.config?.tts?.voice;
    opts.push(h('option', { value: '', selected: !selected }, def ? `Default voice (${def})` : 'Default voice'));
  }
  for (const eng of engines) {
    opts.push(h('optgroup', { label: eng }, state.voices.filter((v) => v.engine === eng)
      .map((v) => h('option', { value: v.id, selected: v.id === selected }, v.label || v.id))));
  }
  if (selected && !state.voices.some((v) => v.id === selected)) opts.push(h('option', { value: selected, selected: true }, selected));
  return opts;
}

/** Caption style picker. '' means the default from Preferences. */
function captionSelect({ id, value, onchange, disabled = false, includeDefault = true }) {
  const def = state.config?.video?.caption_preset || 'pop';
  const defLabel = CAPTION_PRESETS.find(([v]) => v === def)?.[1] || def;
  return h('select', { id, class: 'select sm', 'aria-label': 'Caption style', title: 'Caption style', disabled, onchange: (e) => onchange(e.target.value) },
    includeDefault ? h('option', { value: '', selected: !value }, `${defLabel} captions (default)`) : null,
    CAPTION_PRESETS.map(([v, label, desc]) => h('option', { value: v, selected: v === value, title: desc }, `${label} captions`)));
}

/** Checkboxes for the other aspect ratios rendered from the same edit. */
function alsoExport({ id, main, value = [], onchange, disabled = false }) {
  return h('div', { class: 'also', role: 'group', 'aria-label': 'Also export' },
    h('span', { class: 'also-label' }, 'Also export'),
    FORMATS.filter(([f]) => f !== main).map(([f, ratio, ic, long]) => h('label', { class: 'also-opt', title: `Also render ${long}` },
      h('input', { type: 'checkbox', id: `${id}-${f}`, checked: value.includes(f), disabled,
        onchange: (e) => onchange(FORMATS.map(([x]) => x).filter((x) => x !== main && (x === f ? e.target.checked : value.includes(x)))) }),
      icon(ic), ratio)));
}

/** "Need an idea?" row: Google Trends searches or news headlines; click one to use it. */
function ideasPicker(onpick, { id = 'ideas' } = {}) {
  const s = { source: null, items: null, error: null, request: 0 };
  const body = h('div', { class: 'ideas-list', 'aria-live': 'polite' });
  const btn = (source, ic, label) => h('button', { type: 'button', class: 'btn ghost sm', id: `${id}-${source}`, 'aria-pressed': 'false',
    onclick: () => load(source) }, icon(ic), label);
  const buttons = [btn('trends', 'trend', 'Trending'), btn('news', 'globe', 'In the news')];
  const draw = () => {
    for (const b of buttons) {
      const on = b.id === `${id}-${s.source}`;
      b.setAttribute('aria-pressed', String(on));
      b.classList.toggle('on', on);
    }
    if (!s.source) { body.replaceChildren(); return; }
    if (s.error) { body.replaceChildren(h('span', { class: 'ideas-err' }, icon('alert'), s.error)); return; }
    if (!s.items) { body.replaceChildren(spinner(), h('span', { class: 'muted' }, s.source === 'news' ? 'Reading the news…' : 'Checking Google Trends…')); return; }
    if (!s.items.length) { body.replaceChildren(h('span', { class: 'muted' }, 'Nothing came back. Try the other source.')); return; }
    body.replaceChildren(...s.items.map((idea) => h('button', {
      type: 'button', class: 'idea', title: [idea.detail, idea.url].filter(Boolean).join('\n') || idea.title,
      onclick: () => onpick(idea),
    }, idea.title, idea.detail && s.source === 'trends' ? h('span', { class: 'idea-detail' }, idea.detail) : null)));
  };
  async function load(source) {
    if (s.source === source && (s.items || !s.error)) { s.source = null; s.items = null; draw(); return; }
    const request = ++s.request;
    Object.assign(s, { source, items: null, error: null });
    draw();
    try {
      const items = await api('GET', `/ideas?source=${source}&limit=12`);
      if (request === s.request) s.items = items;
    } catch (e) {
      if (request === s.request) s.error = e.message;
    }
    if (request === s.request) draw();
  }
  return h('div', { class: 'ideas' },
    h('div', { class: 'ideas-head' }, h('span', { class: 'ideas-label' }, icon('sparkles'), 'Need an idea?'), buttons), body);
}

/** Publish options shared by the queue and clip forms. */
function publishOptions(opts, { id, onchange, spacing = true }) {
  const connected = !!state.youtube?.connected;
  if (!connected) {
    return h('div', { class: 'pub-opts off' }, icon('send'),
      h('span', null, 'Connect YouTube in ', h('a', { href: '#/settings/youtube' }, 'Settings'), ' to publish each video when it renders.'));
  }
  const set = (k, v) => { opts[k] = v; onchange?.(); };
  return h('div', { class: 'pub-opts' },
    toggle({ id: `${id}-on`, label: 'Publish to YouTube', checked: opts.on, hint: state.youtube.channel || 'when each video renders',
      onchange: (v) => set('on', v) }),
    opts.on ? [
      h('select', { id: `${id}-privacy`, class: 'select sm', 'aria-label': 'Privacy', onchange: (e) => set('privacy', e.target.value) },
        PRIVACY.map(([v, label]) => h('option', { value: v, selected: v === opts.privacy }, label))),
      h('label', { class: 'inline-field' }, icon('calendar'), h('span', { class: 'sr-only' }, 'First release'),
        h('input', { type: 'datetime-local', id: `${id}-at`, class: 'input sm', value: opts.at, min: localInputValue(new Date()),
          title: 'First release (leave empty to publish on render)', onchange: (e) => set('at', e.target.value) })),
      spacing ? h('label', { class: 'inline-field' }, h('span', null, 'then every'),
        h('input', { type: 'number', id: `${id}-every`, class: 'input sm num', min: 0, max: 720, step: 1, value: opts.everyHours,
          'aria-label': 'Hours between releases', onchange: (e) => set('everyHours', e.target.value) }), h('span', null, 'h')) : null,
    ] : null);
}
/** Request fields for publishOptions state (or {} when off). */
function publishBody(opts) {
  if (!opts.on || !state.youtube?.connected) return {};
  const hours = Number(opts.everyHours) || 0;
  return {
    publish: { privacy: opts.privacy, publish_at: localToIso(opts.at) },
    publish_every_minutes: hours > 0 ? Math.round(hours * 60) : undefined,
  };
}

// ------------------------------------------------------------------ chrome --

function renderChrome() {
  const route = currentRoute;
  for (const a of document.querySelectorAll('[data-nav]')) {
    const on = a.dataset.nav === route.name || (a.dataset.nav === 'home' && route.name === 'project');
    if (on) a.setAttribute('aria-current', 'page'); else a.removeAttribute('aria-current');
  }
  const pending = (state.queue?.items || []).filter((i) => i.status === 'waiting' || i.status === 'running').length;
  const qc = $('#queue-count');
  qc.hidden = !pending;
  qc.textContent = pending;
  qc.closest('a').setAttribute('aria-label', pending ? `Queue, ${pending} to make` : 'Queue');
  const conn = $('#conn');
  conn.dataset.state = state.sse;
  conn.title = state.sse === 'up' ? 'Live updates connected' : state.sse === 'down' ? 'Reconnecting to live updates…' : 'Connecting…';
  $('#conn-label').textContent = state.sse === 'down' ? 'Reconnecting…' : state.sse === 'up' ? 'Live' : 'Connecting…';
  conn.setAttribute('aria-label', conn.title);

  const banner = $('#banner');
  const s = state.status;
  const show = s && !s.ready && !state.bannerDismissed && !(route.name === 'settings' && route.tab === 'setup');
  banner.hidden = !show;
  if (show) {
    const failing = s.checks.filter((c) => !c.ok && c.required !== false);
    patch(banner, failing.map((c) => c.name).join(','), () => [
      icon('alert', 'banner-icon'),
      h('div', { class: 'banner-text' },
        h('strong', null, 'Setup needs attention. '),
        `${failing.length} required check${failing.length === 1 ? '' : 's'} failing: ${failing.map((c) => c.name).join(', ')}. Open setup before rendering.`),
      h('a', { class: 'btn sm', href: '#/settings/setup' }, icon('wrench'), 'Open setup'),
      h('button', {
        class: 'icon-btn', 'aria-label': 'Dismiss',
        onclick: () => { state.bannerDismissed = true; sessionStorage.setItem('cc-banner', '1'); update(); },
      }, icon('x')),
    ]);
  }
}

// -------------------------------------------------------------- home view --

const EXAMPLES = [
  'Why coffee is technically a fruit',
  'The 1977 Wow! signal',
  'How the Great Emu War was lost',
  'Why Finland has no homeless crisis',
  'The man who sold the Eiffel Tower twice',
];

function HomeView() {
  const cfg = state.config;
  const form = {
    topic: sessionStorage.getItem('cc-draft-topic') || '',
    notes: '',
    format: cfg?.video?.format || 'vertical',
    seconds: cfg?.video?.target_seconds || 45,
    voice: '',
    captions: '',
    also: [],
    autopilot: localStorage.getItem('cc-autopilot') === '1',
    formatTouched: false, secondsTouched: false,
    busy: false,
  };
  const clip = {
    url: sessionStorage.getItem('cc-draft-clip') || '',
    count: 3, len: '30-60', format: 'vertical', captions: '', also: [],
    pub: { on: false, privacy: 'public', at: '', everyHours: '' },
    busy: false, job: null, error: null,
  };
  let mode = sessionStorage.getItem('cc-home-mode') === 'clip' ? 'clip' : 'topic';

  const topic = h('textarea', {
    id: 'topic', class: 'topic-input', rows: 1, maxlength: 300, autocomplete: 'off', spellcheck: 'true',
    placeholder: `e.g. ${EXAMPLES[Math.floor(Math.random() * EXAMPLES.length)]}`,
    value: form.topic,
    oninput: (e) => { form.topic = e.target.value; sessionStorage.setItem('cc-draft-topic', form.topic); grow(); syncMake(); },
    onkeydown: (e) => {
      if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); submit(); }
    },
  });
  const grow = () => { topic.style.height = 'auto'; topic.style.height = `${topic.scrollHeight}px`; };

  const notes = h('textarea', {
    id: 'notes', class: 'notes-input', rows: 3, placeholder: 'Angle, audience, facts that must be in it, tone…',
    oninput: (e) => { form.notes = e.target.value; },
  });
  const notesWrap = h('div', { class: 'notes-wrap', hidden: true }, h('label', { class: 'field-label', for: 'notes' }, 'Notes for the writer'), notes);
  const notesBtn = h('button', {
    type: 'button', class: 'btn ghost sm', 'aria-expanded': 'false',
    onclick: () => {
      notesWrap.hidden = !notesWrap.hidden;
      notesBtn.setAttribute('aria-expanded', String(!notesWrap.hidden));
      notesBtn.classList.toggle('on', !notesWrap.hidden);
      if (!notesWrap.hidden) notes.focus();
    },
  }, icon('plus'), 'Notes');

  const voiceSel = h('select', { id: 'voice', class: 'select sm', 'aria-label': 'Voice', onchange: (e) => { form.voice = e.target.value; } });
  const make = h('button', { type: 'submit', class: 'btn primary lg make-btn' }, icon('bolt'), h('span', null, 'Make'));
  const syncMake = () => { make.disabled = form.busy || !form.topic.trim(); };

  async function submit(e) {
    e?.preventDefault();
    if (!form.topic.trim() || form.busy) { topic.focus(); return; }
    form.busy = true;
    make.replaceChildren(spinner('dark'), h('span', null, 'Starting…'));
    syncMake();
    try {
      const p = await api('POST', '/projects', {
        topic: form.topic.trim(), notes: form.notes.trim() || undefined, format: form.format,
        target_seconds: form.seconds, voice: form.voice || undefined, autopilot: form.autopilot,
        caption_preset: form.captions || undefined, extra_formats: form.also.length ? form.also : undefined,
      });
      sessionStorage.removeItem('cc-draft-topic');
      if (!state.running.has(p.id)) setRunning(p.id, true);
      p.running = isRunning(p.id);
      refreshProjects();
      if (currentView?.el === el) {
        state.project = p;
        location.hash = `#/p/${encodeURIComponent(p.id)}`;
      } else toast('Project created. Find it in Projects.');
    } catch (err) {
      toast(err.message, 'error');
      form.busy = false;
      make.replaceChildren(icon('bolt'), h('span', null, 'Make'));
      syncMake();
    }
  }

  const formatOptions = FORMATS.map(([v, ratio, ic]) => [v, ratio, ic]);
  const formatSeg = segmented({ name: 'format', label: 'Format', value: form.format,
    onchange: (v) => { form.format = v; form.formatTouched = true; form.also = form.also.filter((f) => f !== v); update(); },
    options: formatOptions });
  const lengthWrap = h('div', { class: 'length-options' });
  const alsoWrap = h('div', { class: 'also-wrap' });
  const composer = h('form', { class: 'composer', onsubmit: submit, hidden: mode !== 'topic' },
    h('label', { class: 'sr-only', for: 'topic' }, 'Topic'),
    topic,
    notesWrap,
    h('div', { class: 'composer-bar' },
      h('div', { class: 'composer-opts' },
        notesBtn,
        formatSeg, lengthWrap,
        h('div', { class: 'with-icon' }, icon('volume'), voiceSel),
        voicePreviewButton(() => form.voice, 'home-voice-preview'),
        h('div', { class: 'with-icon' }, icon('text'), captionSelect({ id: 'home-captions', value: form.captions, onchange: (v) => { form.captions = v; } })),
        alsoWrap,
        toggle({ label: 'Autopilot', checked: form.autopilot, hint: form.autopilot ? 'straight to video' : 'pause to review',
          onchange: (v) => {
            form.autopilot = v;
            localStorage.setItem('cc-autopilot', v ? '1' : '0');
            composer.querySelector('.switch-hint').textContent = v ? 'straight to video' : 'pause to review';
          } })),
      make));
  const ideas = ideasPicker((idea) => {
    form.topic = idea.title;
    topic.value = idea.title;
    sessionStorage.setItem('cc-draft-topic', form.topic);
    grow(); syncMake(); topic.focus();
  }, { id: 'home-ideas' });
  ideas.hidden = mode !== 'topic';

  // Clip mode: cut the best moments of a long video into shorts.
  const clipUrl = h('input', {
    id: 'clip-url', class: 'clip-url', type: 'url', inputmode: 'url', autocomplete: 'off', spellcheck: 'false',
    placeholder: 'https://www.youtube.com/watch?v=…', value: clip.url,
    oninput: (e) => { clip.url = e.target.value; sessionStorage.setItem('cc-draft-clip', clip.url); syncCut(); },
  });
  const cut = h('button', { type: 'submit', class: 'btn primary lg make-btn' }, icon('scissors'), h('span', null, 'Find clips'));
  const syncCut = () => {
    cut.disabled = clip.busy || !clip.url.trim();
    cut.replaceChildren(clip.busy ? spinner('dark') : icon('scissors'), h('span', null, clip.busy ? 'Finding…' : 'Find clips'));
  };
  const clipAlso = h('div', { class: 'also-wrap' });
  const clipPub = h('div', { class: 'clip-pub' });
  const clipStatus = h('div', { class: 'clip-status', 'aria-live': 'polite' });
  async function submitClip(e) {
    e?.preventDefault();
    const url = clip.url.trim();
    if (!url || clip.busy) { clipUrl.focus(); return; }
    if (!/^https?:\/\//i.test(url)) { toast('Paste the full video link, starting with https://', 'error'); clipUrl.focus(); return; }
    const [min_s, max_s] = clip.len.split('-').map(Number);
    Object.assign(clip, { busy: true, error: null, job: `clip-${Math.random().toString(36).slice(2, 12)}` });
    syncCut(); update();
    try {
      const projects = await api('POST', '/clips', {
        url, count: clip.count, min_s, max_s, format: clip.format, caption_preset: clip.captions || undefined,
        extra_formats: clip.also.length ? clip.also : undefined, job: clip.job, ...publishBody(clip.pub),
      });
      sessionStorage.removeItem('cc-draft-clip');
      clip.url = '';
      clipUrl.value = '';
      toast(projects.length ? `Found ${projects.length} clip${projects.length === 1 ? '' : 's'}. Rendering in the queue.` : 'No good moments found in that video.');
      refreshProjects();
      refreshQueue();
    } catch (err) {
      clip.error = err.message;
      toast(err.message, 'error');
    } finally {
      Object.assign(clip, { busy: false, job: null });
      syncCut(); update();
    }
  }
  const clipForm = h('form', { class: 'composer clip-composer', onsubmit: submitClip, hidden: mode !== 'clip' },
    h('label', { class: 'sr-only', for: 'clip-url' }, 'Video link'),
    h('div', { class: 'clip-url-row' }, icon('scissors'), clipUrl),
    h('div', { class: 'composer-bar' },
      h('div', { class: 'composer-opts' },
        h('select', { id: 'clip-count', class: 'select sm', 'aria-label': 'How many clips', onchange: (e) => { clip.count = Number(e.target.value); } },
          [1, 2, 3, 4, 5].map((n) => h('option', { value: n, selected: n === clip.count }, `${n} clip${n === 1 ? '' : 's'}`))),
        segmented({ name: 'clip-len', label: 'Clip length', value: clip.len, onchange: (v) => { clip.len = v; },
          options: [['15-30', '15–30s'], ['30-60', '30–60s'], ['45-90', '45–90s']] }),
        segmented({ name: 'clip-format', label: 'Format', value: clip.format, options: formatOptions,
          onchange: (v) => { clip.format = v; clip.also = clip.also.filter((f) => f !== v); update(); } }),
        h('div', { class: 'with-icon' }, icon('text'), captionSelect({ id: 'clip-captions', value: clip.captions, onchange: (v) => { clip.captions = v; } })),
        clipAlso),
      cut),
    clipPub,
    clipStatus);

  const HERO = {
    topic: ['What’s the video about?', 'Type a topic. ClipCut researches it, writes a punchy script, finds real clips, memes and screenshots, voices it and renders a short.'],
    clip: ['Clip a long video', 'Paste a YouTube link. ClipCut reads the captions, picks the strongest moments and reframes each one as a captioned short with sound effects.'],
  };
  const heroTitle = h('h1', { class: 'hero-title' }, HERO[mode][0]);
  const heroSub = h('p', { class: 'hero-sub' }, HERO[mode][1]);
  const modeSeg = segmented({ name: 'home-mode', label: 'What to make', value: mode,
    options: [['topic', 'From a topic', 'bolt'], ['clip', 'Clip a long video', 'scissors']],
    onchange: (v) => {
      mode = v;
      sessionStorage.setItem('cc-home-mode', v);
      composer.hidden = ideas.hidden = v !== 'topic';
      clipForm.hidden = v !== 'clip';
      heroTitle.textContent = HERO[v][0];
      heroSub.textContent = HERO[v][1];
      if (v === 'topic') grow();
    } });
  modeSeg.classList.add('mode-seg');

  const list = h('div', { class: 'pgrid' });
  const listHead = h('div', { class: 'section-head' });
  const el = h('div', { class: 'page home' },
    h('section', { class: 'hero' },
      modeSeg,
      heroTitle,
      heroSub,
      composer,
      clipForm,
      ideas),
    h('section', { class: 'recent' }, listHead, list));

  setTimeout(() => {
    grow(); syncMake(); syncCut();
    if (!matchMedia('(pointer: coarse)').matches) (mode === 'clip' ? clipUrl : topic).focus();
  });

  return {
    el,
    update() {
      if (state.config && !form.formatTouched && form.format !== state.config.video.format) {
        form.format = state.config.video.format; formatSeg.setValue(form.format);
        form.also = form.also.filter((f) => f !== form.format);
      }
      if (state.config && !form.secondsTouched) form.seconds = state.config.video.target_seconds;
      patch(lengthWrap, `${state.config?.video?.target_seconds || 45}`, () => segmented({
        name: 'length', label: 'Length', value: form.seconds, onchange: (v) => { form.seconds = v; form.secondsTouched = true; },
        options: [...new Set([30, 45, 60, state.config?.video?.target_seconds || 45])].sort((a, b) => a - b).map((n) => [n, `${n}s`]),
      }));
      patch(voiceSel, `${state.voices.length}|${state.config?.tts?.voice}`, () => voiceOptions(form.voice));
      patch(alsoWrap, JSON.stringify([form.format, form.also]), () => alsoExport({ id: 'home-also', main: form.format, value: form.also,
        onchange: (v) => { form.also = v; update(); } }));
      patch(clipAlso, JSON.stringify([clip.format, clip.also]), () => alsoExport({ id: 'clip-also', main: clip.format, value: clip.also,
        onchange: (v) => { clip.also = v; update(); } }));
      patch(clipPub, JSON.stringify([state.youtube?.connected, state.youtube?.channel, clip.pub]), () =>
        publishOptions(clip.pub, { id: 'clip-pub', onchange: update }));
      const clipLog = clip.job ? state.logs.get(clip.job)?.at(-1) : null;
      patch(clipStatus, JSON.stringify([clip.busy, clipLog?.message, clip.error]), () => clip.busy
        ? [spinner(), h('span', null, clipLog?.message || 'Reading the video…')]
        : clip.error ? h('span', { class: 'ideas-err' }, icon('alert'), clip.error)
          : h('span', { class: 'muted' }, 'Works with any video that has captions. Each clip renders in the ', h('a', { href: '#/queue' }, 'Queue'), '.'));
      const ps = state.projects;
      patch(listHead, ps ? ps.length : -1, () => [
        h('h2', null, 'Recent'), ps?.length ? h('span', { class: 'count' }, ps.length) : null,
      ]);
      if (state.projectsError && !ps) {
        patch(list, `err:${state.projectsError}`, () => errorState(state.projectsError, refreshProjects));
        list.classList.add('single');
        return;
      }
      if (!ps) {
        patch(list, 'loading', () => Array.from({ length: 4 }, () => h('div', { class: 'pcard skeleton' }, h('div', { class: 'pcard-thumb' }), h('div', { class: 'pcard-body' }, h('div', { class: 'sk-line' }), h('div', { class: 'sk-line short' })))));
        return;
      }
      if (!ps.length) {
        list.classList.add('single');
        patch(list, 'empty', () => emptyState('film', 'No videos yet', 'Your projects show up here, with live progress while they build.'));
        return;
      }
      list.classList.remove('single');
      if (list._sig === 'loading' || list._sig === 'empty' || String(list._sig).startsWith('err:')) { list.replaceChildren(); list._sig = 'list'; }
      keyed(list, ps, (p) => p.id, (p) => {
        const live = state.live.get(p.id);
        return JSON.stringify([p.updated_at, p.running, p.stages.map((s) => [s.status, s.progress]), p.title, p.thumb_url, p.kind, p.published?.length, p.running && live ? [live.message, live.progress] : 0, Math.floor(Date.now() / 60000)]);
      }, projectCard);
    },
    unmount() { disposeTree(el); },
  };
}

function projectCard(p) {
  const st = pipelineState(p.stages, p.running);
  const live = p.running ? state.live.get(p.id) : null;
  const hue = parseInt(hashStr(p.id), 36) % 360;
  const contain = p.format !== 'vertical';
  const published = p.published?.at(-1);
  const scheduled = published?.publish_at && new Date(published.publish_at) > Date.now();
  let thumb;
  if (p.thumb_url) {
    const src = `${p.thumb_url}${p.thumb_url.includes('?') ? '&' : '?'}v=${encodeURIComponent(p.updated_at)}`;
    thumb = [h('img', { class: 'thumb-bg', src, alt: '', loading: 'lazy', 'aria-hidden': 'true' }),
      h('img', { class: `thumb-fg ${contain ? 'contain' : ''}`, src, alt: '', loading: 'lazy' })];
  } else {
    thumb = h('div', { class: 'thumb-ph', style: { '--h': hue } }, h('span', { class: 'thumb-ph-text' }, p.title || p.topic));
  }
  return h('a', { class: `pcard ${p.running ? 'is-running' : ''}`, href: `#/p/${encodeURIComponent(p.id)}` },
    h('div', { class: 'pcard-thumb' }, thumb,
      h('div', { class: 'pcard-top' }, statusChip(st),
        h('span', { class: 'pcard-tags' },
          p.kind === 'clip' ? h('span', { class: 'fmt-tag clip-tag', title: 'Clipped from a long video' }, icon('scissors'), 'Clip') : null,
          h('span', { class: 'fmt-tag' }, FMT_LABEL[p.format] || '9:16'))),
      p.duration_s ? h('span', { class: 'pcard-dur' }, fmtDuration(p.duration_s)) : null,
      p.video_url ? h('span', { class: 'pcard-play', 'aria-hidden': 'true' }, icon('play')) : null,
      p.running ? h('div', { class: 'pcard-progress' },
        h('div', { class: 'pcard-live' }, live?.message || `${STAGE_LABEL[st.stage]}…`),
        h('div', { class: 'pcard-steps' }, st.list.map((s) => h('span', {
          class: `pstep ${s.status}`,
          style: s.status === 'running' ? { '--p': pct(s.stage === live?.stage && live?.progress != null ? live.progress : s.progress) } : null,
        })))) : null),
    h('div', { class: 'pcard-body' },
      h('div', { class: 'pcard-title' }, p.title || p.topic),
      h('div', { class: 'pcard-meta' },
        p.title && p.title !== p.topic ? h('span', { class: 'ellipsis' }, p.topic) : null,
        published ? h('span', { class: 'pub-tag nowrap', title: scheduled ? `Goes live ${fmtWhen(published.publish_at)}` : `On YouTube (${published.privacy})` },
          icon(scheduled ? 'calendar' : 'send'), scheduled ? 'Scheduled' : 'Published') : null,
        h('span', { class: 'nowrap' }, relTime(p.created_at))),
      st.kind === 'failed' && st.error ? h('div', { class: 'pcard-err ellipsis', title: st.error }, st.error) : null));
}

// ----------------------------------------------------------- project view --

const TABS = [
  ['visuals', 'Visuals', 'layers'], ['script', 'Script', 'doc'], ['hooks', 'Hooks', 'bolt'],
  ['research', 'Research', 'globe'], ['activity', 'Activity', 'wave'],
];
const STAGE_TAB = { research: 'research', hooks: 'hooks', script: 'script', beats: 'visuals', assets: 'visuals', voice: 'visuals', render: 'visuals' };

async function runProject(id, body, what = 'Started') {
  if (editBlocked(id) || beatsBusy(id)) return;
  if (currentView?.projectId === id && currentView.hasUnsaved()) { toast('Save or revert your edits before running the pipeline.', 'error'); return; }
  state.actions.set(id, 'run');
  const request = nextProjectRequest(id);
  const wasRunning = isRunning(id);
  setRunning(id, true);
  const runningRevision = state.running.get(id).revision;
  update();
  try {
    const p = await api('POST', `/projects/${encodeURIComponent(id)}/run`, body);
    acceptProject(p, request);
    toast(what);
  } catch (e) {
    if (state.running.get(id)?.revision === runningRevision) setRunning(id, wasRunning);
    toast(e.message, 'error');
  } finally {
    state.actions.delete(id);
    refreshProjects();
    refreshProject(id);
    update();
  }
}

async function patchProject(id, body, okMsg) {
  if (editBlocked(id) || beatsBusy(id)) return false;
  state.actions.set(id, 'save');
  const request = nextProjectRequest(id);
  update();
  try {
    const p = await api('PATCH', `/projects/${encodeURIComponent(id)}`, body);
    if (Object.keys(body).some((k) => ['topic', 'notes', 'voice', 'format', 'target_seconds', 'chosen_hook', 'script_text', 'caption_preset', 'extra_formats'].includes(k))) invalidateRender(id);
    acceptProject(p, request);
    if (okMsg) toast(okMsg);
    return true;
  } catch (e) {
    toast(e.message, 'error');
    return false;
  } finally {
    state.actions.delete(id);
    refreshProjects();
    refreshProject(id);
    update();
  }
}

async function cancelProject(id) {
  if (projectBusy(id)) return;
  state.actions.set(id, 'cancel');
  const request = nextProjectRequest(id);
  const runningRevision = state.running.get(id)?.revision;
  update();
  try {
    const p = await api('POST', `/projects/${encodeURIComponent(id)}/cancel`);
    if (state.running.get(id)?.revision === runningRevision) setRunning(id, false);
    acceptProject(p, request);
    toast('Pipeline cancelled. Review your work or retry a stage.');
  } catch (e) { toast(e.message, 'error'); }
  finally {
    state.actions.delete(id);
    refreshProjects(); refreshProject(id); update();
  }
}

function ProjectView(id, initialTab) {
  const ui = {
    tab: initialTab || null, manualTab: !!initialTab, editing: null, edits: {},
    scriptDraft: null, showDraft: false, meta: null, drawer: null, details: false, projectDraft: null,
    pub: { privacy: 'public', at: '', format: null },
  };
  if (state.project?.id !== id) state.project = null;
  state.projectError = null;
  refreshProject(id);

  const head = h('header', { class: 'phead' });
  const details = h('section', { class: 'project-details card', hidden: true });
  const media = h('div', { class: 'preview-media' });
  const overlay = h('div', { class: 'preview-overlay' });
  const preview = h('div', { class: 'preview' }, media, overlay);
  const stepper = h('ol', { class: 'stepper', 'aria-label': 'Pipeline stages' });
  const live = h('div', { class: 'live', 'aria-live': 'polite' });
  const controls = h('div', { class: 'controls' });
  const note = h('div', { class: 'pause-note' });
  const kit = h('section', { class: 'kit card', hidden: true });
  const tabsNav = h('div', { class: 'tabs', role: 'tablist', 'aria-label': 'Project sections' });
  const panes = Object.fromEntries(TABS.map(([t]) => [t, h('section', { class: `pane pane-${t}`, role: 'tabpanel', id: `pane-${t}`,
    'aria-labelledby': `tab-${t}`, tabindex: 0, hidden: true })]));
  const body = h('div', { class: 'pbody' },
    head, details,
    h('section', { class: 'studio card' }, preview, h('div', { class: 'studio-side' }, stepper, live, note, controls)),
    kit,
    h('div', { class: 'tabs-wrap' }, tabsNav),
    Object.values(panes));
  const el = h('div', { class: 'page project' }, body);

  function autoTab(p, st) {
    if (st.kind === 'paused') return STAGE_TAB[st.stage];
    if (p.beats.length) return 'visuals';
    if (p.script) return 'script';
    if (p.hooks.length) return 'hooks';
    if (p.research && !state.project.running) return 'research';
    return 'activity';
  }

  function setTab(t, manual = true) {
    if (!TABS.some(([id]) => id === t)) return;
    ui.tab = t;
    if (manual) ui.manualTab = true;
    const want = `#/p/${encodeURIComponent(id)}/${t}`;
    if (manual && location.hash !== want) history.replaceState(null, '', want);
    update();
  }

  function render() {
    const p = state.project;
    if (!p || p.id !== id) {
      if (state.projectError) {
        patch(body, `err:${state.projectError}`, () => [
          h('a', { class: 'back', href: '#/' }, icon('back'), 'Projects'),
          errorState(state.projectError, () => refreshProject(id))]);
      } else {
        patch(body, 'loading', () => h('div', { class: 'loading-page' }, spinner('lg'), 'Loading project…'));
      }
      return;
    }
    if (body._sig) { // recover from loading/error placeholder
      body._sig = undefined;
      disposeTree(body);
      body.replaceChildren(head, details, h('section', { class: 'studio card' }, preview, h('div', { class: 'studio-side' }, stepper, live, note, controls)),
        kit, h('div', { class: 'tabs-wrap' }, tabsNav), ...Object.values(panes));
    }
    const st = pipelineState(p.stages, p.running);
    if (!ui.manualTab) ui.tab = autoTab(p, st);
    document.title = `${p.script?.title || p.topic} · ClipCut`;

    renderHead(p, st);
    renderDetails(p);
    renderPreview(p, st);
    renderStepper(p, st);
    renderControls(p, st);
    renderKit(p);
    patch(tabsNav, JSON.stringify([ui.tab, p.beats.length, p.hooks.length, !!p.script, !!p.research, (state.logs.get(id) || []).length > 0]), () =>
      TABS.map(([t, label, ic]) => {
        const count = t === 'visuals' ? p.beats.length : t === 'hooks' ? p.hooks.length : null;
        return h('button', {
          type: 'button', role: 'tab', class: 'tab', id: `tab-${t}`, 'aria-selected': String(ui.tab === t), 'aria-controls': `pane-${t}`,
          tabindex: ui.tab === t ? 0 : -1, onclick: () => setTab(t),
          onkeydown: (e) => {
            const i = TABS.findIndex(([x]) => x === t);
            const j = radioKeyIndex(e, i, TABS.length);
            if (j == null) return;
            const nt = TABS[(j + TABS.length) % TABS.length][0];
            setTab(nt);
            requestAnimationFrame(() => $(`#tab-${nt}`)?.focus());
          },
        }, icon(ic), label, count ? h('span', { class: 'count' }, count) : null);
      }));
    for (const [t, pane] of Object.entries(panes)) pane.hidden = t !== ui.tab;
    PANES[ui.tab](panes[ui.tab], p, st, ui);
  }

  function renderHead(p, st) {
    const voice = p.voice || state.config?.tts?.voice;
    const isClip = p.kind === 'clip';
    const src = p.clip;
    patch(head, JSON.stringify([p.topic, p.script?.title, p.format, p.extra_formats, p.kind, src?.url, p.target_seconds, voice, p.autopilot, p.running, projectBusy(id), beatsBusy(id), ui.details]), () => [
      h('a', { class: 'back', href: '#/' }, icon('back'), 'Projects'),
      h('div', { class: 'phead-row' },
        h('div', { class: 'phead-text' },
          h('h1', { class: 'ptitle' }, p.script?.title || p.topic),
          h('div', { class: 'pmeta' },
            p.script?.title && !isClip ? h('span', { class: 'ellipsis' }, p.topic) : null,
            isClip ? h('span', { class: 'fmt-tag clip-tag' }, icon('scissors'), 'Clip') : null,
            isClip && src ? h('a', { class: 'ellipsis src-link', href: src.url, target: '_blank', rel: 'noopener noreferrer', title: 'Open the source video' },
              `${src.title || 'Source video'}${src.channel ? ` · ${src.channel}` : ''} · ${fmtStamp(src.start_s)}–${fmtStamp(src.end_s)}`) : null,
            h('span', { class: 'fmt-tag' }, FMT_LABEL[p.format] || '9:16'),
            p.extra_formats?.length ? h('span', { title: 'Also exported from the same edit' }, `+ ${p.extra_formats.map((f) => FMT_LABEL[f] || f).join(' ')}`) : null,
            isClip ? null : h('span', null, `${p.target_seconds}s target`),
            voice && !isClip ? h('span', null, icon('volume'), voice) : null)),
        h('div', { class: 'phead-actions' },
          toggle({
            id: 'project-autopilot', label: 'Autopilot', checked: p.autopilot, hint: p.autopilot ? 'no pauses' : 'pauses to review', disabled: editBlocked(id) || beatsBusy(id),
            onchange: (v) => patchProject(p.id, { autopilot: v }, v ? 'Autopilot on' : 'Autopilot off: pauses at hook, script and visuals'),
          }),
          h('button', { class: 'btn ghost sm', id: 'project-edit', 'aria-expanded': String(ui.details), 'aria-controls': 'project-details-form',
            onclick: () => { ui.details = !ui.details; update(); } }, icon('sliders'), 'Details'),
          h('button', {
            class: 'btn ghost sm', title: isClip ? 'Duplicate (cuts the same moment again)' : 'Duplicate (keeps research, hooks and script)', 'aria-label': 'Duplicate project',
            disabled: editBlocked(id) || beatsBusy(id),
            onclick: async () => {
              if (editBlocked(id) || beatsBusy(id)) return;
              const view = currentView;
              state.actions.set(id, 'duplicate'); update();
              try {
                const c = await api('POST', `/projects/${encodeURIComponent(p.id)}/duplicate`);
                toast('Duplicated');
                refreshProjects();
                if (view === currentView) location.hash = `#/p/${encodeURIComponent(c.id)}`;
              } catch (e) { toast(e.message, 'error'); }
              finally { state.actions.delete(id); update(); }
            },
          }, icon('dup'), h('span', { class: 'hide-sm' }, 'Duplicate')),
          h('button', {
            class: 'btn ghost sm danger-text', title: 'Delete project', 'aria-label': 'Delete project',
            disabled: editBlocked(id) || beatsBusy(id),
            onclick: async () => {
              if (!(await confirmDialog({ title: 'Delete this project?', body: 'The project folder, its assets and the rendered video are removed.', ok: 'Delete', danger: true }))) return;
              if (editBlocked(id) || beatsBusy(id)) return;
              const view = currentView;
              state.actions.set(id, 'delete'); update();
              try {
                await api('DELETE', `/projects/${encodeURIComponent(p.id)}`);
                toast('Project deleted');
                sessionStorage.removeItem(`cc-stale-render-${id}`);
                refreshProjects();
                if (view === currentView) { state.project = null; ui.scriptDraft = ui.meta = ui.projectDraft = null; ui.edits = {}; location.hash = '#/'; }
              } catch (e) { toast(e.message, 'error'); }
              finally { state.actions.delete(id); update(); }
            },
          }, icon('trash'), h('span', { class: 'hide-sm' }, 'Delete')))),
    ]);
  }

  function renderDetails(p) {
    details.hidden = !ui.details;
    if (!ui.details) return;
    const blocked = editBlocked(id) || beatsBusy(id);
    const isClip = p.kind === 'clip';
    patch(details, JSON.stringify([p.topic, p.notes, p.voice, p.format, p.target_seconds, p.caption_preset, p.extra_formats, p.kind, blocked,
      state.voices.length, state.config?.video?.caption_preset, !!ui.projectDraft]), () => {
      const styling = { format: p.format, caption_preset: p.caption_preset || '', extra_formats: [...(p.extra_formats || [])] };
      const draft = ui.projectDraft || (isClip ? styling
        : { topic: p.topic, notes: p.notes || '', voice: p.voice || '', target_seconds: p.target_seconds, ...styling });
      const set = (key, value) => {
        ui.projectDraft = { ...draft, ...(ui.projectDraft || {}), [key]: value };
        update();
      };
      const edit = (key) => (e) => set(key, key === 'target_seconds' ? Number(e.target.value) : e.target.value);
      const field = (key, label, control) => h('div', { class: `field ${['topic', 'notes'].includes(key) ? 'span' : ''}` },
        h('label', { class: 'field-label', for: `project-${key}` }, label), control);
      // The checkboxes depend on the main format, so they redraw in place.
      const alsoWrap = h('div', { class: 'also-wrap' });
      const drawAlso = () => {
        const d = ui.projectDraft || draft;
        alsoWrap.replaceChildren(alsoExport({ id: 'project-also', main: d.format, value: d.extra_formats, disabled: blocked,
          onchange: (v) => { set('extra_formats', v); drawAlso(); } }));
      };
      drawAlso();
      const src = p.clip;
      return h('form', { id: 'project-details-form', class: 'project-details-form', onsubmit: async (e) => {
        e.preventDefault();
        const draft = ui.projectDraft;
        if (!draft || blocked) return;
        if (!isClip && (!draft.topic.trim() || draft.target_seconds < 10 || draft.target_seconds > 600)) return;
        const body = {};
        for (const key of Object.keys(draft)) {
          if (key === 'extra_formats') {
            if (JSON.stringify(draft[key]) !== JSON.stringify(p.extra_formats || [])) body[key] = draft[key];
            continue;
          }
          const value = typeof draft[key] === 'string' ? draft[key].trim() : draft[key];
          if (String(value) !== String(p[key] ?? '')) body[key] = key === 'caption_preset' ? value || null : value;
        }
        if (!Object.keys(body).length) return;
        const what = isClip ? 'Changing the format, captions or extra exports means the clip renders again.'
          : 'Changing the topic, notes, voice, format, captions or length invalidates affected downstream work. Continue the pipeline afterwards to rebuild it.';
        if (!(await confirmDialog({ title: 'Save project details?', body: what, ok: 'Save details' }))) return;
        if (await patchProject(id, body, 'Project details saved')) { ui.projectDraft = null; update(); }
      } },
      h('h2', { class: 'span' }, isClip ? 'Clip details' : 'Project details'),
      p.running ? h('p', { class: 'muted span' }, 'Cancel the pipeline before making review edits.') : null,
      isClip && src ? h('dl', { class: 'clip-source span' },
        h('dt', null, 'Source'), h('dd', null, h('a', { href: src.url, target: '_blank', rel: 'noopener noreferrer' },
          src.title || src.url, src.channel ? ` · ${src.channel}` : '', icon('external'))),
        h('dt', null, 'Moment'), h('dd', null, `${fmtStamp(src.start_s)}–${fmtStamp(src.end_s)} (${fmtDuration(src.end_s - src.start_s)})`),
        src.hook ? [h('dt', null, 'Hook'), h('dd', null, src.hook)] : null,
        src.reason ? [h('dt', null, 'Why this moment'), h('dd', { class: 'muted' }, src.reason)] : null) : null,
      isClip ? null : [
        field('topic', 'Topic', h('input', { id: 'project-topic', class: 'input', maxlength: 300, required: true, value: draft.topic, disabled: blocked, oninput: edit('topic') })),
        field('notes', 'Notes for the writer', h('textarea', { id: 'project-notes', class: 'input', rows: 3, value: draft.notes, disabled: blocked, oninput: edit('notes') })),
        field('voice', 'Voice', h('div', { class: 'row gap' },
          h('select', { id: 'project-voice', class: 'select', disabled: blocked, onchange: edit('voice') }, voiceOptions(draft.voice)),
          voicePreviewButton(() => ui.projectDraft?.voice ?? draft.voice, 'project-voice-preview'))),
      ],
      field('format', 'Format', h('select', { id: 'project-format', class: 'select', disabled: blocked, onchange: (e) => {
        const format = e.target.value;
        ui.projectDraft = { ...draft, ...(ui.projectDraft || {}) };
        ui.projectDraft.extra_formats = ui.projectDraft.extra_formats.filter((f) => f !== format);
        set('format', format);
        drawAlso();
      } }, FORMATS.map(([value, , , long]) => h('option', { value, selected: value === draft.format }, long)))),
      field('caption_preset', 'Captions', captionSelect({ id: 'project-caption_preset', value: draft.caption_preset, disabled: blocked, onchange: (v) => set('caption_preset', v) })),
      isClip ? null : field('target_seconds', 'Target length (seconds)', h('input', { id: 'project-target_seconds', class: 'input', type: 'number', min: 10, max: 600, required: true, value: draft.target_seconds, disabled: blocked, oninput: edit('target_seconds') })),
      h('div', { class: 'field span' }, alsoWrap),
      h('div', { class: 'row gap end span' },
        h('button', { class: 'btn ghost sm', type: 'button', disabled: blocked || !ui.projectDraft, onclick: () => { ui.projectDraft = null; update(); } }, 'Revert'),
        h('button', { class: 'btn primary sm', type: 'submit', disabled: blocked || !ui.projectDraft }, 'Save details')));
    });
  }

  function renderPreview(p, st) {
    preview.classList.toggle('horizontal', p.format === 'horizontal');
    preview.classList.toggle('square', p.format === 'square');
    const lastReady = [...p.beats].reverse().find((b) => b.asset && (b.status === 'ready' || b.status === 'fallback'));
    const ready = renderReady(p);
    const mediaSig = ready ? `r:${p.render.rendered_at}` : lastReady ? `b:${JSON.stringify(lastReady.asset)}` : 'ph';
    patch(media, mediaSig, () => {
      if (ready) {
        const v = h('video', {
          class: 'final', controls: true, playsinline: true, preload: 'metadata',
          poster: `/api/projects/${encodeURIComponent(p.id)}/still?t=0.5&v=${encodeURIComponent(p.render.rendered_at)}`,
        });
        v.src = fileUrl(p.id, p.render.path, encodeURIComponent(p.render.rendered_at));
        return v;
      }
      if (lastReady) return assetMedia(p.id, lastReady.asset, 'media cover');
      return h('div', { class: 'preview-ph', style: { '--h': parseInt(hashStr(p.id), 36) % 360 } }, h('span', null, p.topic));
    });
    const lv = state.live.get(p.id);
    const showOverlay = !ready;
    const stage = st.list.find((s) => s.stage === st.stage);
    const sp = lv?.stage === st.stage && lv.progress != null ? lv.progress : stage?.progress || 0;
    patch(overlay, JSON.stringify([showOverlay, st.kind, st.stage, Math.round(st.overall * 100), Math.round(sp * 100), lastReady?.id]), () => {
      if (!showOverlay) return null;
      const big = st.kind === 'running' ? pct(st.overall) : st.kind === 'failed' ? '!' : st.kind === 'paused' ? '❚❚' : st.kind === 'done' ? '✓' : '';
      return h('div', { class: `ov ov-${st.kind}` },
        lastReady ? h('span', { class: 'ov-tag' }, `Preview · ${lastReady.id}`) : null,
        h('div', { class: 'ov-center' },
          st.kind === 'running' ? h('div', { class: 'ring', style: { '--p': st.overall } }, h('span', null, big)) : big ? h('div', { class: 'ov-big' }, big) : null,
          h('div', { class: 'ov-label' }, st.kind === 'running' ? `${STAGE_LABEL[st.stage]}` : st.label)));
    });
    preview.classList.toggle('has-overlay', showOverlay);
  }

  function renderStepper(p, st) {
    const lv = state.live.get(p.id);
    const blocked = editBlocked(id) || beatsBusy(id) || !!dirtyProject(ui, p);
    const sig = JSON.stringify([st.list.map((s) => [s.status, Math.round((s.progress || 0) * 100), s.error, s.started_at, s.finished_at]), lv?.stage, lv?.progress, lv?.message, p.running, ui.tab, blocked]);
    patch(stepper, sig, () => st.list.map((s, i) => {
      const running = s.status === 'running';
      const prog = running && lv?.stage === s.stage && lv.progress != null ? lv.progress : s.progress;
      let took = '';
      if (s.status === 'done' && s.started_at && s.finished_at) {
        const secs = (new Date(s.finished_at) - new Date(s.started_at)) / 1000;
        if (secs >= 0) took = fmtDuration(Math.max(1, secs));
      }
      const canRerun = !p.running && (s.status !== 'pending' || st.list.slice(0, i).every((x) => x.status === 'done' || x.status === 'skipped'));
      return h('li', { class: `step ${s.status} ${st.kind === 'paused' && st.stage === s.stage ? 'paused' : ''}` },
        h('button', { type: 'button', class: 'step-main', onclick: () => setTab(STAGE_TAB[s.stage]), title: `Show ${STAGE_LABEL[s.stage]}` },
          h('span', { class: 'step-icon' },
            s.status === 'done' ? icon('check') : s.status === 'failed' ? icon('x') : running ? h('span', { class: 'step-ring', style: { '--p': prog || 0 } }) : s.status === 'skipped' ? '–' : String(i + 1)),
          h('span', { class: 'step-text' },
            h('span', { class: 'step-label' }, STAGE_LABEL[s.stage]),
            running ? h('span', { class: 'step-msg' }, (lv?.stage === s.stage && lv.message) || s.message || 'Working…') : null,
            s.status === 'failed' && s.error ? h('span', { class: 'step-err' }, s.error) : null),
          h('span', { class: 'step-aside' }, running ? pct(prog) : took)),
        canRerun ? h('button', {
          type: 'button', class: 'icon-btn sm step-rerun', 'aria-label': `Re-run from ${STAGE_LABEL[s.stage]}`, title: `Re-run from ${STAGE_LABEL[s.stage]}`,
          disabled: blocked,
          onclick: async () => {
            const later = st.list.slice(i).some((x) => x.status === 'done');
            if (later && !(await confirmDialog({ title: `Re-run from ${STAGE_LABEL[s.stage]}?`, body: `${STAGE_LABEL[s.stage]} and every later stage are reset and run again. Locked visuals are kept.`, ok: 'Re-run' }))) return;
            runProject(p.id, { from: s.stage }, `Re-running from ${STAGE_LABEL[s.stage]}`);
          },
        }, icon('rerun')) : null);
    }));
  }

  function renderControls(p, st) {
    const lv = state.live.get(p.id);
    const logs = state.logs.get(p.id) || [];
    const last = logs.at(-1);
    patch(live, JSON.stringify([p.running, last?.message, last?.level, st.kind, st.error]), () => {
      if (p.running && last) return [h('span', { class: `live-dot lvl-${last.level}` }), h('span', { class: 'live-msg' }, last.message)];
      if (p.running) return [h('span', { class: 'live-dot' }), h('span', { class: 'live-msg' }, lv?.message || 'Starting…')];
      if (st.kind === 'failed') return [icon('alert', 'err-text'), h('span', { class: 'live-msg err-text' }, st.error || 'Stage failed')];
      return null;
    });
    live.hidden = !live.childNodes.length;

    const needHook = st.kind === 'paused' && st.stage === 'hooks' && p.chosen_hook == null;
    const dirty = dirtyProject(ui, p);
    const action = state.actions.get(id);
    const beatBusy = beatsBusy(id);
    const stale = !!p.render && !renderReady(p);
    const blocked = !!action || beatBusy || !!dirty;
    patch(note, JSON.stringify([st.kind, st.stage, needHook, ui.tab, dirty, beatBusy, stale]), () => {
      if (dirty) return h('div', { class: 'callout' }, icon('edit'),
        h('div', null, h('strong', null, 'Unsaved edits'), h('p', null, 'Save or revert your changes before continuing or re-running.')),
        h('button', { class: 'btn sm', onclick: () => { if (dirty === 'details') ui.details = true; else setTab(dirty); update(); } }, 'Review edits'));
      if (beatBusy) return h('div', { class: 'callout' }, spinner(), h('p', null, 'Finish the visual change before continuing.'));
      if (st.kind === 'done' && stale) return h('div', { class: 'callout' }, icon('info'), h('p', null, 'Re-render to include your visual changes. The previous download is no longer current.'));
      if (st.kind !== 'paused') return null;
      const hint = { hooks: needHook ? 'Choose the hook you like best, then continue.' : 'Hook chosen. Continue to write the script.',
        script: 'Edit the narration if you like, then continue to plan the beats.',
        assets: 'Swap any visual you don’t like, then continue to voice and render.' }[st.stage];
      return h('div', { class: 'callout' }, icon('info'), h('div', null, h('strong', null, `Paused · ${REVIEW[st.stage]}`), h('p', null, hint)),
        ui.tab !== STAGE_TAB[st.stage] ? h('button', { class: 'btn sm', onclick: () => setTab(STAGE_TAB[st.stage]) }, 'Open') : null);
    });
    note.hidden = !note.childNodes.length;

    patch(controls, JSON.stringify([st.kind, st.stage, st.next, p.running, needHook, blocked, action]), () => {
      const btns = [];
      if (p.running) {
        btns.push(h('button', {
          class: 'btn danger-ghost', disabled: !!action, onclick: () => cancelProject(p.id),
        }, action === 'cancel' ? spinner() : icon('stop'), action === 'cancel' ? 'Cancelling…' : 'Cancel'));
      } else if (st.kind === 'paused' || st.kind === 'stopped') {
        btns.push(h('button', { class: 'btn primary', disabled: needHook || blocked, title: needHook ? 'Pick a hook first' : dirty ? 'Save or revert your edits first' : null, onclick: () => runProject(p.id, undefined, 'Continuing') },
          icon('play'), needHook ? 'Pick a hook to continue' : `Continue to ${STAGE_LABEL[st.next] || 'next stage'}`));
      } else if (st.kind === 'failed') {
        btns.push(h('button', { class: 'btn primary', disabled: blocked, onclick: () => runProject(p.id, { from: st.stage }, `Retrying ${STAGE_LABEL[st.stage]}`) }, icon('rerun'), `Retry ${STAGE_LABEL[st.stage]}`));
      } else if (st.kind === 'idle') {
        btns.push(h('button', { class: 'btn primary', disabled: blocked, onclick: () => runProject(p.id, undefined) }, icon('play'), 'Run'));
      } else if (st.kind === 'done') {
        btns.push(h('button', { class: 'btn', disabled: blocked, onclick: () => runProject(p.id, { from: 'render' }, 'Re-rendering') }, icon('rerun'), 'Re-render'));
      }
      return btns;
    });
  }

  function renderKit(p) {
    kit.hidden = !renderReady(p);
    if (!renderReady(p)) return;
    const s = p.script || {};
    const r = p.render;
    patch(kit, JSON.stringify([r.rendered_at, s.title, s.description, s.hashtags, p.format, r.variants, r.subtitles, r.thumbnail_jpg]), () => {
      const row = (label, text, what) => h('div', { class: 'kit-row' },
        h('div', { class: 'kit-label' }, label),
        h('div', { class: 'kit-val' }, text || h('span', { class: 'muted' }, '—')),
        text ? h('button', { class: 'icon-btn', 'aria-label': `Copy ${what}`, title: `Copy ${what}`, onclick: () => copyText(text, what) }, icon('copy')) : null);
      const tags = (s.hashtags || []).join(' ');
      const dl = (file) => `/api/projects/${encodeURIComponent(p.id)}/download?file=${file}`;
      const files = [
        ...(r.variants || []).map((v) => h('a', { class: 'btn ghost sm', href: dl(v.format), download: '', title: `${v.width}×${v.height}` },
          icon('download'), `${FMT_LABEL[v.format] || v.format} MP4`)),
        r.subtitles ? [
          h('a', { class: 'btn ghost sm', href: dl('srt'), download: '', title: 'Captions for YouTube, Premiere, Resolve…' }, icon('doc'), 'SRT'),
          h('a', { class: 'btn ghost sm', href: dl('vtt'), download: '', title: 'Captions for the web' }, icon('doc'), 'VTT'),
        ] : null,
        r.thumbnail_jpg ? h('a', { class: 'btn ghost sm', href: dl('thumb'), download: '' }, icon('image'), 'Thumbnail') : null,
        h('a', { class: 'btn ghost sm', href: fileUrl(p.id, r.credits_path), target: '_blank', rel: 'noopener' }, icon('doc'), 'Credits'),
      ];
      return [
        h('div', { class: 'kit-head' },
          h('div', null, h('h2', null, 'Ready to post'),
            h('p', { class: 'muted' }, `${fmtDuration(r.duration_s)} · ${r.width}×${r.height} · rendered ${relTime(r.rendered_at)}`)),
          h('div', { class: 'kit-actions' },
            h('a', { class: 'btn primary', href: dl('main'), download: '' }, icon('download'),
              r.variants?.length ? `Download ${FMT_LABEL[p.format] || ''} MP4` : 'Download MP4'))),
        h('div', { class: 'kit-files', role: 'group', 'aria-label': 'Other downloads' }, files),
        h('div', { class: `kit-body ${r.thumbnail_jpg ? 'has-thumb' : ''}` },
          r.thumbnail_jpg ? h('a', { class: 'kit-thumb', href: fileUrl(p.id, r.thumbnail_jpg, encodeURIComponent(r.rendered_at)), target: '_blank', rel: 'noopener', title: 'Open the thumbnail' },
            h('img', { src: fileUrl(p.id, r.thumbnail_jpg, encodeURIComponent(r.rendered_at)), alt: 'Thumbnail', loading: 'lazy', decoding: 'async' })) : null,
          h('div', { class: 'kit-text' },
            row('Title', s.title, 'Title'),
            row('Description', s.description, 'Description'),
            row('Hashtags', tags, 'Hashtags'),
            h('button', { class: 'btn ghost sm kit-all', onclick: () => copyText([s.title, '', s.description, '', tags].join('\n'), 'Post text') }, icon('copy'), 'Copy all'))),
        h('div', { class: 'kit-pub' }),
      ];
    });
    renderPublish(p, kit.querySelector('.kit-pub'));
  }

  function renderPublish(p, box) {
    const yt = state.youtube;
    const busy = state.publishing.has(p.id);
    const last = busy ? (state.logs.get(p.id) || []).at(-1) : null;
    const formats = [p.format, ...(p.render.variants || []).map((v) => v.format)];
    patch(box, JSON.stringify([yt?.connected, yt?.channel, p.published, formats, busy, last?.message, last?.progress, p.running]), () => {
      const published = (p.published || []).slice().reverse().map((rec) => {
        const scheduled = rec.publish_at && new Date(rec.publish_at) > new Date();
        return h('li', null, icon(scheduled ? 'calendar' : 'send'),
          h('a', { href: rec.url, target: '_blank', rel: 'noopener noreferrer' }, rec.url.replace(/^https?:\/\/(www\.)?/, ''), icon('external')),
          h('span', { class: 'tag' }, FMT_LABEL[rec.format] || rec.format),
          h('span', { class: 'muted' }, scheduled ? `goes ${rec.privacy} ${fmtWhen(rec.publish_at)}` : `${rec.privacy} · ${relTime(rec.uploaded_at)}`));
      });
      const list = published.length ? h('ul', { class: 'pub-list', 'aria-label': 'Published copies' }, published) : null;
      if (!yt?.connected) {
        return [h('div', { class: 'pub-connect' }, icon('send'),
          h('div', null, h('strong', null, 'Publish to YouTube Shorts'),
            h('p', { class: 'muted' }, 'Connect your channel once, then upload and schedule from here.')),
          h('a', { class: 'btn sm', href: '#/settings/youtube' }, 'Connect YouTube')), list];
      }
      if (busy) {
        return [h('div', { class: 'pub-busy', 'aria-live': 'polite' }, spinner(),
          h('div', null, h('strong', null, 'Uploading to YouTube…'), h('p', { class: 'muted' }, last?.message || 'Starting the upload')),
          last?.progress != null ? progressBar(last.progress) : null), list];
      }
      const pub = ui.pub;
      if (!formats.includes(pub.format)) pub.format = p.format;
      const at = h('input', { type: 'datetime-local', id: 'pub-at', class: 'input sm', value: pub.at, min: localInputValue(new Date()),
        disabled: pub.privacy === 'private', title: 'Leave empty to publish now',
        onchange: (e) => { pub.at = e.target.value; go.replaceChildren(icon(pub.at ? 'calendar' : 'send'), pub.at ? 'Schedule' : 'Publish now'); } });
      const go = h('button', { class: 'btn primary', type: 'button', disabled: p.running, onclick: () => publishNow(p) },
        icon(pub.at && pub.privacy !== 'private' ? 'calendar' : 'send'), pub.at && pub.privacy !== 'private' ? 'Schedule' : 'Publish now');
      return [
        h('div', { class: 'pub-head' },
          h('div', null, h('h3', null, 'Publish to YouTube'),
            h('p', { class: 'muted' }, yt.channel ? `As ${yt.channel}. ` : '', 'Uses the title, description and hashtags above.'))),
        h('div', { class: 'pub-row' },
          segmented({ name: 'pub-privacy', label: 'Privacy', value: pub.privacy, options: PRIVACY,
            onchange: (v) => { pub.privacy = v; at.disabled = v === 'private'; go.replaceChildren(icon(pub.at && v !== 'private' ? 'calendar' : 'send'), pub.at && v !== 'private' ? 'Schedule' : 'Publish now'); } }),
          h('label', { class: 'inline-field', title: 'Scheduled videos upload private and go public at this time' },
            icon('calendar'), h('span', { class: 'sr-only' }, 'Release time'), at),
          formats.length > 1 ? h('select', { id: 'pub-format', class: 'select sm', 'aria-label': 'Which render to upload',
            onchange: (e) => { pub.format = e.target.value; } },
          formats.map((f) => h('option', { value: f, selected: f === pub.format }, FMT_LONG[f] || f))) : null,
          go),
        list,
      ];
    });
  }

  async function publishNow(p) {
    if (state.publishing.has(p.id)) return;
    const pub = ui.pub;
    const at = pub.privacy === 'private' ? null : localToIso(pub.at);
    if (pub.at && pub.privacy !== 'private' && !at) { toast('Pick a valid release time.', 'error'); return; }
    if (at && new Date(at) <= new Date()) { toast('Pick a release time in the future, or clear it to publish now.', 'error'); return; }
    const format = pub.format || p.format;
    if ((p.published || []).some((rec) => rec.format === format) && !(await confirmDialog({
      title: 'Upload another copy?', body: 'This render is already on YouTube. Publishing again uploads a second copy.', ok: 'Upload again' }))) return;
    state.publishing.add(p.id);
    update();
    try {
      const rec = await api('POST', `/projects/${encodeURIComponent(p.id)}/publish`, { privacy: pub.privacy, publish_at: at, format });
      toast(rec.publish_at ? `Scheduled for ${fmtWhen(rec.publish_at)}` : `Published as ${rec.privacy}`);
      pub.at = '';
      await refreshProject(p.id);
    } catch (e) {
      toast(e.message, 'error');
    } finally {
      state.publishing.delete(p.id);
      update();
    }
  }

  const onKey = (e) => {
    if (e.key === 'Escape' && ui.editing && !document.querySelector('dialog[open]')) {
      ui.editing = null;
      update();
    }
  };
  document.addEventListener('keydown', onKey);

  return {
    el,
    projectId: id,
    hasUnsaved: () => !!(state.project?.id === id && dirtyProject(ui, state.project)),
    setTab: (t) => { if (t) setTab(t, true); },
    update: render,
    unmount() {
      document.removeEventListener('keydown', onKey);
      ui.drawer?.close();
      disposeTree(el);
      document.title = 'ClipCut';
    },
  };
}

// ------------------------------------------------------------ project panes --

function stageStatus(p, stage) {
  return p.stages.find((s) => s.stage === stage)?.status || 'pending';
}
/** Empty state for a pane whose stage hasn't produced anything yet. */
function pendingPane(p, stage, ic, waitTitle, idleTitle, body) {
  const running = p.running && stageStatus(p, stage) === 'running';
  return h('div', { class: 'empty' },
    h('div', { class: 'empty-icon' }, running ? spinner('lg') : icon(ic)),
    h('h3', null, running ? waitTitle : idleTitle), body && h('p', null, body));
}

/** Word-level LCS diff: [{ t: 'same'|'del'|'ins', w }] runs. */
function wordDiff(a, b) {
  const A = a.split(/\s+/).filter(Boolean);
  const B = b.split(/\s+/).filter(Boolean);
  const n = A.length;
  const m = B.length;
  if (n * m > 1_000_000) return [{ t: 'del', w: a }, { t: 'ins', w: b }];
  const norm = (w) => w.toLowerCase().replace(/[^\p{L}\p{N}']/gu, '');
  const dp = Array.from({ length: n + 1 }, () => new Uint16Array(m + 1));
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      dp[i][j] = norm(A[i]) === norm(B[j]) ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1]);
    }
  }
  const out = [];
  const push = (t, w) => {
    const last = out.at(-1);
    if (last?.t === t) last.w += ` ${w}`; else out.push({ t, w });
  };
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (norm(A[i]) === norm(B[j])) { push('same', B[j]); i++; j++; }
    else if (dp[i + 1][j] >= dp[i][j + 1]) push('del', A[i++]);
    else push('ins', B[j++]);
  }
  while (i < n) push('del', A[i++]);
  while (j < m) push('ins', B[j++]);
  return out;
}

const PANES = {
  hooks(pane, p, st, ui) {
    const blocked = editBlocked(p.id) || beatsBusy(p.id);
    patch(pane, JSON.stringify([p.hooks, p.chosen_hook, blocked, stageStatus(p, 'hooks'), stageStatus(p, 'script'), p.kind]), () => {
      if (p.kind === 'clip') {
        const hk = p.hooks[0];
        return emptyState('scissors', hk ? `“${hk.text}”` : 'From the source video',
          hk ? `The opening line shown over the clip.${hk.why ? ` Why this moment: ${hk.why}` : ''}` : 'Clips open on the speaker’s own words, so there is no hook to pick.');
      }
      if (!p.hooks.length) return pendingPane(p, 'hooks', 'bolt', 'Writing hook options…', 'No hooks yet', 'Hook options appear after research.');
      const best = Math.max(...p.hooks.map((x) => x.score || 0));
      const choose = async (i) => {
        if (i === p.chosen_hook) return;
        if (stageStatus(p, 'script') !== 'pending' && !(await confirmDialog({
          title: 'Use this hook?', body: 'Changing the hook resets the script and every later stage (beats, visuals, voiceover, render).', ok: 'Use hook',
        }))) return;
        if (dirtyProject(ui, p) && !(await confirmDialog({ title: 'Discard your unsaved edits?', body: 'Changing the hook rebuilds the script and later stages.', ok: 'Discard & use hook', danger: true }))) return;
        if (await patchProject(p.id, { chosen_hook: i }, 'Hook chosen')) {
          ui.scriptDraft = ui.meta = null; ui.edits = {}; ui.editing = null; update();
          if (!p.autopilot) toast('Press Continue to write the script');
        }
      };
      return [
        h('div', { class: 'pane-head' }, h('div', null, h('h2', null, 'Pick the opening line'),
          h('p', { class: 'muted' }, 'The first second decides whether people keep watching. Scores are the writer’s own 0–10 estimate.'))),
        h('div', { class: 'hooks', role: 'radiogroup', 'aria-label': 'Hook options' }, p.hooks.map((hk, i) => {
          const chosen = p.chosen_hook === i;
          return h('button', {
            type: 'button', id: `hook-${i}`, class: `hook ${chosen ? 'chosen' : ''}`, role: 'radio', 'aria-checked': String(chosen),
            tabindex: chosen || (p.chosen_hook == null && i === 0) ? 0 : -1,
            disabled: blocked, onclick: () => choose(i),
            onkeydown: (e) => { const j = radioKeyIndex(e, i, p.hooks.length); if (j != null) { $(`#hook-${j}`)?.focus(); choose(j); } },
          },
          h('div', { class: 'hook-top' },
            h('span', { class: 'score', style: { '--s': (hk.score || 0) / 10 } }, (hk.score || 0).toFixed(1)),
            hk.angle ? h('span', { class: 'tag' }, hk.angle) : null,
            hk.score === best ? h('span', { class: 'tag tag-accent' }, 'Top pick') : null,
            h('span', { class: 'hook-radio', 'aria-hidden': 'true' }, chosen ? icon('check') : null)),
          h('p', { class: 'hook-text' }, hk.text),
          hk.why ? h('p', { class: 'hook-why' }, hk.why) : null);
        })),
      ];
    });
  },

  script(pane, p, st, ui) {
    const s = p.script;
    const blocked = editBlocked(p.id) || beatsBusy(p.id);
    const later = ['beats', 'assets', 'voice', 'render'].some((x) => stageStatus(p, x) !== 'pending');
    const isClip = p.kind === 'clip';
    patch(pane, JSON.stringify([s?.text, s?.draft, s?.title, s?.description, s?.hashtags, ui.showDraft, !!ui.meta, blocked, later, stageStatus(p, 'script'), p.target_seconds, isClip]), () => {
      if (!s) return pendingPane(p, 'script', 'doc', 'Writing the script…', 'No script yet', 'The script is written after you pick a hook.');
      const draft = ui.scriptDraft ?? s.text;
      const stats = h('div', { class: 'script-stats' });
      const fill = h('div', { class: 'bar-fill' });
      const meter = h('div', { class: 'bar' }, fill);
      const save = h('button', { class: 'btn primary sm', type: 'button' }, icon('check'), 'Save script');
      const revert = h('button', { class: 'btn ghost sm', type: 'button' }, 'Revert');
      const ta = h('textarea', { class: 'script-editor', id: 'script-text', rows: 10, value: draft, disabled: blocked, spellcheck: 'true' });
      const sync = () => {
        const w = words(ta.value);
        const secs = w / WORDS_PER_SECOND;
        const ratio = secs / (p.target_seconds || 45);
        stats.replaceChildren(
          h('span', null, h('strong', null, w), ' words'),
          h('span', null, '≈ ', h('strong', null, `${Math.round(secs)}s`), ` of ${p.target_seconds}s`),
          h('span', { class: 'muted' }, `${s.sentences.length} saved sentences`));
        fill.style.width = pct(Math.min(1, ratio));
        meter.className = `bar ${ratio > 1.15 ? 'warn' : ratio < 0.7 ? 'low' : ''}`;
        const dirty = ta.value !== s.text;
        save.disabled = !dirty || blocked || !ta.value.trim();
        revert.disabled = blocked;
        revert.hidden = !dirty;
      };
      ta.addEventListener('input', () => { ui.scriptDraft = ta.value; sync(); ta.style.height = 'auto'; ta.style.height = `${ta.scrollHeight + 2}px`; update(); });
      requestAnimationFrame(() => { ta.style.height = 'auto'; ta.style.height = `${ta.scrollHeight + 2}px`; });
      revert.onclick = () => { ui.scriptDraft = null; ta.value = s.text; sync(); update(); };
      save.onclick = async () => {
        if (later && !(await confirmDialog({ title: 'Save the new script?', body: 'This re-splits the narration and resets the beat sheet, visuals, voiceover and render.', ok: 'Save' }))) return;
        save.disabled = true;
        if (await patchProject(p.id, { script_text: ta.value.trim() }, 'Script saved')) { ui.scriptDraft = null; ui.edits = {}; ui.editing = null; update(); }
        else sync();
      };
      sync();

      const meta = ui.meta || { title: s.title, description: s.description, hashtags: (s.hashtags || []).join(' ') };
      const metaField = (key, label, el) => h('div', { class: 'field' }, h('label', { class: 'field-label', for: `meta-${key}` }, label), el);
      const onMeta = (key) => (e) => { ui.meta = { ...meta, ...(ui.meta || {}), [key]: e.target.value }; metaSave.disabled = blocked || !dirtyProject(ui, p); update(); };
      const metaSave = h('button', {
        class: 'btn sm', type: 'button', disabled: blocked || !ui.meta,
        onclick: async () => {
          const m = ui.meta || meta;
          const tags = m.hashtags.split(/[\s,]+/).filter(Boolean).map((t) => (t.startsWith('#') ? t : `#${t}`));
          if (await patchProject(p.id, { title: m.title, description: m.description, hashtags: tags }, 'Details saved')) { ui.meta = null; update(); }
        },
      }, 'Save details');

      const metaBlock = h('div', { class: 'card-inset meta' },
        h('h3', null, 'Post details'),
        metaField('title', 'Title', h('input', { id: 'meta-title', class: 'input', value: meta.title, disabled: blocked, maxlength: 100, oninput: onMeta('title') })),
        metaField('description', 'Description', h('textarea', { id: 'meta-description', class: 'input', rows: 3, value: meta.description, disabled: blocked, oninput: onMeta('description') })),
        metaField('hashtags', 'Hashtags', h('input', { id: 'meta-hashtags', class: 'input', value: meta.hashtags, disabled: blocked, placeholder: '#shorts #facts', oninput: onMeta('hashtags') })),
        h('div', { class: 'row gap end' },
          h('button', { class: 'btn ghost sm', type: 'button', disabled: blocked || !ui.meta, onclick: () => {
            ui.meta = null; pane._sig = null; update();
          } }, 'Revert details'), metaSave));
      if (isClip) {
        return [
          h('div', { class: 'pane-head' }, h('div', null, h('h2', null, 'Transcript'),
            h('p', { class: 'muted' }, 'The source video’s own captions for this moment. They become the on-screen captions.'))),
          h('blockquote', { class: 'transcript card-inset' }, s.text || h('span', { class: 'muted' }, 'No captions in this window.')),
          metaBlock,
        ];
      }

      let compare = null;
      if (ui.showDraft && s.draft) {
        compare = h('div', { class: 'diff card-inset' },
          h('div', { class: 'diff-legend' }, h('span', { class: 'd-del' }, 'removed by unslop'), h('span', { class: 'd-ins' }, 'added')),
          h('p', null, wordDiff(s.draft, s.text).map((r) => h('span', { class: `d-${r.t}` }, r.w, ' '))));
      }
      return [
        h('div', { class: 'pane-head' },
          h('div', null, h('h2', null, 'Narration'), h('p', { class: 'muted' }, 'Final script after the unslop pass. Edit freely; saving rebuilds the beats.')),
          s.draft ? h('button', { class: `btn ghost sm ${ui.showDraft ? 'on' : ''}`, 'aria-pressed': String(ui.showDraft), onclick: () => { ui.showDraft = !ui.showDraft; update(); } },
            icon('eye'), ui.showDraft ? 'Hide first draft' : 'Compare with first draft') : null),
        compare,
        h('div', { class: 'script-box' },
          h('label', { class: 'sr-only', for: 'script-text' }, 'Script'), ta,
          h('div', { class: 'script-foot' }, h('div', { class: 'script-meter' }, stats, meter), h('div', { class: 'row gap' }, revert, save))),
        metaBlock,
      ];
    });
  },

  research(pane, p) {
    const r = p.research;
    patch(pane, JSON.stringify([r, stageStatus(p, 'research'), p.kind, p.clip?.url]), () => {
      if (p.kind === 'clip') {
        const src = p.clip;
        return emptyState('scissors', 'From the source video', 'Clips skip research: every word comes from the video itself.',
          src?.url ? h('a', { class: 'btn ghost sm', href: src.url, target: '_blank', rel: 'noopener noreferrer' }, icon('external'), 'Open the source') : null);
      }
      if (!r) return pendingPane(p, 'research', 'globe', 'Researching…', 'No research yet', 'ClipCut searches the web and writes a short briefing first.');
      const host = (u) => { try { return new URL(u).hostname.replace(/^www\./, ''); } catch { return u; } };
      return [
        h('div', { class: 'pane-head' }, h('h2', null, 'Briefing')),
        h('p', { class: 'summary' }, r.summary),
        r.angles?.length ? h('div', { class: 'chips-row' }, h('span', { class: 'field-label' }, 'Angles'), r.angles.map((a) => h('span', { class: 'tag' }, a))) : null,
        r.keywords?.length ? h('div', { class: 'chips-row' }, h('span', { class: 'field-label' }, 'Keywords'), r.keywords.map((a) => h('span', { class: 'tag mono' }, a))) : null,
        h('details', { class: 'fold', open: true },
          h('summary', null, icon('chevronRight'), `Facts (${r.facts.length})`),
          h('ol', { class: 'facts' }, r.facts.map((f) => h('li', null,
            h('span', null, f.text),
            f.source_url ? h('a', { class: 'src', href: f.source_url, target: '_blank', rel: 'noopener noreferrer' }, f.source_title || host(f.source_url), icon('external')) : h('span', { class: 'src muted' }, 'no source'))))),
        h('details', { class: 'fold' },
          h('summary', null, icon('chevronRight'), `Sources (${r.sources.length})`),
          h('ul', { class: 'sources' }, r.sources.map((s) => h('li', null,
            h('a', { href: s.url, target: '_blank', rel: 'noopener noreferrer' }, s.title || s.url, icon('external')),
            h('span', { class: 'muted mono' }, host(s.url)),
            s.snippet ? h('p', { class: 'muted' }, s.snippet) : null)))),
      ];
    });
  },

  activity(pane, p) {
    const logs = state.logs.get(p.id) || [];
    patch(pane, `${logs.length}:${logs.at(-1)?.at}`, () => {
      if (!logs.length) return emptyState('wave', 'Quiet for now', 'Live messages from the pipeline show up here while it runs (last 50).');
      return [
        h('div', { class: 'pane-head' }, h('h2', null, 'Activity'), h('span', { class: 'muted' }, 'Newest first · last 50')),
        h('ol', { class: 'log' }, [...logs].reverse().map((l) => h('li', { class: `lvl-${l.level}` },
          h('time', null, new Date(l.at).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' })),
          l.stage ? h('span', { class: 'tag' }, STAGE_LABEL[l.stage] || l.stage) : null,
          l.beat_id ? h('span', { class: 'tag mono' }, l.beat_id) : null,
          h('span', { class: 'log-msg' }, l.message)))),
      ];
    });
  },

  visuals(pane, p, st, ui) {
    if (!pane._built) {
      pane._built = true;
      pane._head = h('div', { class: 'vis-head' });
      pane._voice = h('div', { class: 'voice-wrap' });
      pane._list = h('div', { class: 'beats' });
      pane._empty = h('div');
      pane.append(pane._voice, pane._head, pane._empty, pane._list);
    }
    patch(pane._voice, JSON.stringify([p.voice_track?.path, p.voice_track?.duration_s, stageStatus(p, 'voice')]), () => {
      const v = p.voice_track;
      if (!v || stageStatus(p, 'voice') !== 'done') return null;
      return h('div', { class: 'voice card-inset' },
        h('div', { class: 'voice-icon' }, icon('wave')),
        h('div', { class: 'voice-info' }, h('strong', null, 'Voiceover'), h('span', { class: 'muted' }, `${v.voice} · ${v.engine} · ${fmtDuration(v.duration_s)}`),
          h('span', { class: 'muted sm' }, 'Beat and word timings are approximate, not forced-aligned.')),
        h('audio', { controls: true, 'aria-label': 'Project voiceover', preload: 'metadata', src: fileUrl(p.id, v.path, hashStr(`${v.duration_s}${v.voice}${p.updated_at}`)) }));
    });
    const counts = {};
    for (const b of p.beats) counts[b.status] = (counts[b.status] || 0) + 1;
    patch(pane._head, JSON.stringify([counts, p.beats.length, st.kind, st.stage, p.kind, p.format]), () => {
      if (!p.beats.length) return null;
      if (p.kind === 'clip') return h('div', null, h('h2', null, 'The clip'),
        h('p', { class: 'muted' }, `The moment from the source video, reframed to ${FMT_LONG[p.format] || p.format} with captions and sound effects.`));
      return [
        h('div', null, h('h2', null, 'Visuals'),
          h('p', { class: 'muted' }, `${p.beats.length} beats. Review while paused; pinned media is kept when re-running visuals (not when rebuilding the beat sheet). Uploads: images or video, up to 256 MiB.`)),
        h('div', { class: 'vis-counts' }, ['ready', 'fallback', 'searching', 'failed', 'pending'].filter((k) => counts[k])
          .map((k) => h('span', { class: `chip chip-b-${k}` }, `${counts[k]} ${BEAT_STATUS[k].toLowerCase()}`))),
      ];
    });
    patch(pane._empty, p.beats.length ? 'has' : `none:${stageStatus(p, 'beats')}`, () =>
      p.beats.length ? null : pendingPane(p, 'beats', 'layers', 'Planning the beat sheet…', 'No beats yet', 'After the script, ClipCut plans a visual for every 1–3 seconds of narration.'));
    const now = Date.now();
    keyed(pane._list, p.beats, (b) => b.id, (b) => {
      const key = `${p.id}/${b.id}`;
      const lv = state.beatLive.get(key);
      const fresh = lv && now - lv.at < 4000 ? lv.message : '';
      return JSON.stringify([b, state.busy.get(key), fresh, ui.editing === b.id, editBlocked(p.id), ui.edits[b.id]?.kind, p.format, p.kind]);
    }, (b) => beatRow(p, b, ui, now));
    ui.drawer?.render();
  },
};

function beatRow(p, b, ui, now) {
  const key = `${p.id}/${b.id}`;
  const busy = state.busy.get(key);
  const lv = state.beatLive.get(key);
  const liveMsg = lv && now - lv.at < 4000 ? lv : null;
  const working = busy || (p.running && b.status === 'searching') || (liveMsg && p.running);
  const v = b.visual;
  const time = b.end_s > 0 ? `${fmtClock(b.start_s)} – ${fmtClock(b.end_s)}` : null;
  const fileInput = h('input', {
    type: 'file', accept: UPLOAD_ACCEPT, hidden: true, 'aria-label': `Upload visual for ${b.id}`,
    onchange: (e) => {
      const f = e.target.files[0];
      if (!f) return;
      const fd = uploadBody(f);
      e.target.value = '';
      if (!fd) return;
      beatAction(p.id, b.id, 'Uploading', { sub: '/upload' }, fd).then((nb) => nb && toast(`${b.id}: using your file`));
    },
  });
  const disabled = !!busy || editBlocked(p.id);
  const editing = ui.editing === b.id;
  // A clip's footage is the source moment itself; only its styling is editable.
  const fixed = p.kind === 'clip';

  return h('article', { class: `beat st-${b.status} ${b.locked ? 'is-locked' : ''} ${working ? 'is-busy' : ''}`, 'aria-label': `Beat ${b.id}` },
    h('div', { class: `beat-media ${fmtClass(p.format)}` },
      beatMedia(p.id, b, 'media'),
      v.on_screen_text && (b.asset || v.kind !== 'text') ? h('span', { class: 'ost-overlay' }, v.on_screen_text) : null,
      working ? h('div', { class: 'media-busy' }, spinner()) : null,
      fixed ? null : h('button', { class: 'media-hit', 'aria-label': `Swap visual for ${b.id}`, onclick: () => openSwap(p.id, b.id, ui), disabled })),
    h('div', { class: 'beat-main' },
      h('div', { class: 'beat-top' },
        h('span', { class: 'beat-id mono' }, b.id),
        time ? h('span', { class: 'beat-time mono', title: 'Approximate beat timing' }, `≈ ${time}`) : null,
        kindBadge(v.kind),
        b.asset && b.asset.provider && b.asset.provider !== PROVIDER_OF_KIND[v.kind] ? h('span', { class: 'tag mono sm' }, b.asset.provider) : null,
        h('span', { class: `bstatus bs-${b.status}` }, BEAT_STATUS[b.status] || b.status),
        fixed ? null : h('button', {
          class: `icon-btn sm pin ${b.locked ? 'on' : ''}`, 'aria-pressed': String(b.locked), disabled: disabled || !b.asset,
          title: !b.asset ? 'Only downloaded or uploaded media can be pinned' : b.locked ? 'Pinned: re-running visuals keeps this media. Click to unpin.' : 'Pin this media so visual re-runs keep it',
          'aria-label': b.locked ? 'Unpin visual' : 'Pin visual',
          onclick: () => beatAction(p.id, b.id, 'Saving', { method: 'PATCH' }, { locked: !b.locked }),
        }, icon(b.locked ? 'lock' : 'unlock'))),
      h('p', { class: 'beat-text' }, b.text),
      editing ? beatEditor(p, b, ui, disabled) : h('div', { class: 'beat-plan' },
        v.on_screen_text ? h('span', { class: 'plan-item' }, h('span', { class: 'plan-k' }, 'On screen'), h('span', { class: 'ost' }, v.on_screen_text)) : null,
        v.query ? h('span', { class: 'plan-item' }, h('span', { class: 'plan-k' }, 'Query'), h('span', { class: 'mono ellipsis' }, v.query)) : null,
        v.meme?.template ? h('span', { class: 'plan-item' }, h('span', { class: 'plan-k' }, 'Template'), v.meme.template) : null),
      b.error ? h('p', { class: `beat-err ${b.status === 'failed' ? 'err-text' : 'warn-text'}` }, icon('alert'), b.error) : null,
      working ? h('p', { class: 'beat-live' }, spinner('xs'), busy ? `${busy}…` : liveMsg?.message || 'Searching…') : null),
    fixed ? null : h('div', { class: 'beat-actions' },
      h('button', { class: 'btn sm', disabled, onclick: () => openSwap(p.id, b.id, ui) }, icon('swap'), 'Swap'),
      h('button', {
        class: 'btn ghost sm', disabled, title: 'Re-run the automatic search and pick for this beat (unpins it)',
        onclick: () => beatAction(p.id, b.id, 'Auto-picking', { sub: '/auto' }).then((nb) => nb && toast(`${b.id}: new automatic pick`)),
      }, icon('sparkles'), 'Auto'),
      h('button', { class: 'btn ghost sm', disabled, onclick: () => fileInput.click() }, icon('upload'), 'Upload'),
      h('button', {
        class: `btn ghost sm ${editing ? 'on' : ''}`, disabled, 'aria-expanded': String(editing),
        onclick: () => { ui.editing = editing ? null : b.id; update(); },
      }, icon('edit'), 'Edit'),
      fileInput));
}

const PROVIDER_OF_KIND = { youtube: 'youtube', article: 'article', meme: 'imgflip', gif: 'giphy', text: 'text' };

function beatEditor(p, b, ui, disabled) {
  const e = ui.edits[b.id] || (ui.edits[b.id] = { kind: b.visual.kind, query: b.visual.query || '', ost: b.visual.on_screen_text || '' });
  const id = (k) => `ed-${b.id}-${k}`;
  const save = async (auto) => {
    const visual = { ...b.visual, kind: e.kind, query: e.query.trim(), on_screen_text: e.ost.trim() || null };
    const useAuto = auto || visual.kind === 'text';
    const nb = await beatAction(p.id, b.id, useAuto ? 'Saving & picking' : 'Saving', { method: 'PATCH', autoAfter: useAuto }, { visual });
    if (!nb) return;
    ui.editing = null;
    delete ui.edits[b.id];
    toast(`${b.id}: ${visual.kind === 'text' ? 'using text visual' : 'saved'}`);
    update();
  };
  return h('form', { class: 'beat-edit', onsubmit: (ev) => { ev.preventDefault(); save(false); } },
    h('fieldset', { disabled, class: 'beat-edit-fields' },
    h('div', { class: 'field' }, h('label', { class: 'field-label', for: id('kind') }, 'Kind'),
      h('select', { id: id('kind'), class: 'select sm', onchange: (ev) => { e.kind = ev.target.value; update(); } },
        KIND_IDS.map((k) => h('option', { value: k, selected: k === e.kind }, KINDS[k].label)))),
    h('div', { class: 'field grow' }, h('label', { class: 'field-label', for: id('query') }, 'Search query'),
      h('input', { id: id('query'), class: 'input sm', value: e.query, disabled: e.kind === 'text', oninput: (ev) => { e.query = ev.target.value; update(); } })),
    h('div', { class: 'field grow' }, h('label', { class: 'field-label', for: id('ost') }, 'On-screen text (≤ 5 words, empty = captions only)'),
      h('input', { id: id('ost'), class: 'input sm', value: e.ost, maxlength: 60, oninput: (ev) => { e.ost = ev.target.value; update(); } })),
    h('div', { class: 'row gap end full' },
      h('button', { type: 'button', class: 'btn ghost sm', onclick: () => { ui.editing = null; delete ui.edits[b.id]; update(); } }, 'Cancel'),
      e.kind !== 'text' ? h('button', { type: 'button', class: 'btn sm', onclick: () => save(true) }, icon('sparkles'), 'Save & auto-pick') : null,
      h('button', { type: 'submit', class: 'btn primary sm' }, e.kind === 'text' ? 'Use text visual' : 'Save'))));
}

// ------------------------------------------------------------- swap drawer --

function openSwap(pid, beatId, ui) {
  if (editBlocked(pid) || state.busy.has(`${pid}/${beatId}`)) return;
  ui.drawer?.close();
  const beat0 = state.project?.beats.find((b) => b.id === beatId);
  if (!beat0) return;
  const opener = document.activeElement;
  const local = { kind: beat0.visual.kind, query: beat0.visual.query || '', ost: beat0.visual.on_screen_text || '', choosing: null };
  const body = h('div', { class: 'drawer-body' });
  const dlg = h('dialog', { class: 'drawer', 'aria-labelledby': 'drawer-title' },
    h('header', { class: 'drawer-head' },
      h('div', null, h('h2', { id: 'drawer-title' }, `Swap visual · `, h('span', { class: 'mono' }, beatId)),
        h('p', { class: 'muted drawer-quote' }, `“${beat0.text}”`)),
      h('button', { class: 'icon-btn', 'aria-label': 'Close', onclick: () => close() }, icon('x'))),
    body);
  const close = () => { if (dlg.open) dlg.close(); };
  dlg.addEventListener('close', () => {
    if (ui.drawer?.el === dlg) ui.drawer = null;
    disposeTree(dlg);
    document.body.classList.remove('dialog-open');
    dlg.classList.add('closing');
    setTimeout(() => dlg.remove(), 180);
    if (opener?.isConnected) opener.focus({ preventScroll: true });
  });
  dlg.addEventListener('click', (e) => { if (e.target === dlg) close(); });

  const search = async () => {
    const beat = state.project?.id === pid && state.project.beats.find((b) => b.id === beatId);
    if (!beat || editBlocked(pid)) return;
    if (local.kind === 'text') {
      const visual = { ...beat.visual, kind: 'text', on_screen_text: local.ost.trim() || null };
      if (await beatAction(pid, beatId, 'Making text visual', { method: 'PATCH', autoAfter: true }, { visual })) {
        delete ui.edits[beatId]; ui.editing = null; toast(`${beatId}: using text visual`); close();
      }
    } else {
      await beatAction(pid, beatId, 'Searching', { sub: '/search' }, { kind: local.kind, query: local.query.trim() || undefined });
    }
  };
  const choose = async (i) => {
    local.choosing = i;
    render();
    const nb = await beatAction(pid, beatId, 'Downloading', { sub: '/choose' }, { index: i });
    local.choosing = null;
    if (nb) { toast(`${beatId}: visual swapped`); close(); } else render();
  };

  function render() {
    const p = state.project;
    const b = p?.id === pid && p.beats.find((x) => x.id === beatId);
    if (!b) { close(); return; }
    const busy = state.busy.get(`${pid}/${beatId}`);
    const blocked = !!busy || editBlocked(pid);
    patch(body, JSON.stringify([b, busy, local.kind, local.choosing, blocked]), () => {
      const fileInput = h('input', {
        type: 'file', accept: UPLOAD_ACCEPT, hidden: true, 'aria-label': `Upload visual for ${beatId}`,
        onchange: async (e) => {
          const f = e.target.files[0];
          if (!f) return;
          const fd = uploadBody(f);
          e.target.value = '';
          if (!fd) return;
          if (await beatAction(pid, beatId, 'Uploading', { sub: '/upload' }, fd)) { toast(`${beatId}: using your file`); close(); }
        },
      });
      const q = h('input', {
        class: 'input', id: 'swap-query', value: local.query, disabled: blocked, placeholder: 'What should it show?', enterkeyhint: 'search',
        oninput: (e) => { local.query = e.target.value; },
      });
      return [
        h('div', { class: 'swap-current' },
          h('div', { class: `swap-thumb ${fmtClass(p.format)}` }, beatMedia(pid, b, 'media')),
          h('div', { class: 'swap-cur-info' },
            h('div', { class: 'row gap wrap' }, kindBadge(b.visual.kind), h('span', { class: `bstatus bs-${b.status}` }, BEAT_STATUS[b.status]), b.locked ? h('span', { class: 'tag' }, icon('lock'), 'Pinned') : null),
            b.asset?.credit ? h('p', { class: 'muted sm' }, b.asset.credit) : h('p', { class: 'muted sm' }, b.asset ? b.asset.provider : 'No asset yet'),
            b.asset?.source_url ? h('a', { class: 'link sm', href: b.asset.source_url, target: '_blank', rel: 'noopener noreferrer' }, 'Source', icon('external')) : null,
            h('div', { class: 'row gap wrap' },
              h('button', { class: 'btn ghost sm', disabled: blocked, onclick: () => beatAction(pid, beatId, 'Auto-picking', { sub: '/auto' }).then((nb) => { if (nb) { toast(`${beatId}: new automatic pick`); close(); } }) }, icon('sparkles'), 'Auto pick'),
              h('button', { class: 'btn ghost sm', disabled: blocked, onclick: () => fileInput.click() }, icon('upload'), 'Upload file'),
              fileInput))),
        h('form', { class: 'swap-search', onsubmit: (e) => { e.preventDefault(); search(); } },
          h('div', { class: 'kind-picker', role: 'radiogroup', 'aria-label': 'Visual kind' }, KIND_IDS.map((k, i) => h('button', {
            type: 'button', id: `swap-kind-${k}`, role: 'radio', tabindex: local.kind === k ? 0 : -1, disabled: blocked,
            'aria-checked': String(local.kind === k), class: `kind-opt kind-${k}`, title: KINDS[k].desc,
            onclick: () => { local.kind = k; render(); },
            onkeydown: (e) => { const j = radioKeyIndex(e, i, KIND_IDS.length); if (j != null) { local.kind = KIND_IDS[j]; render(); $(`#swap-kind-${local.kind}`, dlg)?.focus(); } },
          }, KINDS[k].label))),
          h('div', { class: 'search-row' },
            h('label', { class: 'sr-only', for: local.kind === 'text' ? 'swap-text' : 'swap-query' }, local.kind === 'text' ? 'On-screen text' : 'Search query'),
            h('div', { class: 'input-icon' }, icon(local.kind === 'text' ? 'text' : 'search'), local.kind === 'text'
              ? h('input', { id: 'swap-text', class: 'input', value: local.ost, maxlength: 60, disabled: blocked,
                placeholder: 'Short on-screen text (or use narration)', oninput: (e) => { local.ost = e.target.value; } }) : q),
            h('button', { class: 'btn primary', type: 'submit', disabled: blocked }, busy ? spinner('dark') : icon(local.kind === 'text' ? 'text' : 'search'), local.kind === 'text' ? 'Use text visual' : 'Search')),
          h('p', { class: 'hint' }, local.kind === 'text' ? 'No download needed. A short text card replaces the current asset.' : 'You can also upload an image or video, up to 256 MiB. SVG is not supported.')),
        local.kind === 'text' ? null : [
        h('div', { class: 'cands-head' }, h('h3', null, 'Candidates'), b.candidates.length ? h('span', { class: 'muted sm' }, `${b.candidates.length} results · tap to use`) : null),
        busy === 'Searching'
          ? h('div', { class: 'cands' }, Array.from({ length: 6 }, () => h('div', { class: 'cand skeleton' })))
          : b.candidates.length
            ? h('div', { class: `cands ${fmtClass(p.format)}` }, b.candidates.map((c, i) => h('div', { class: `cand ${b.chosen === i ? 'chosen' : ''} ${local.choosing === i ? 'choosing' : ''}` },
              h('button', {
                type: 'button', class: 'cand-btn', disabled: blocked, 'aria-label': `Use ${c.title}`,
                onclick: () => choose(i),
              },
              c.thumbnail_url && safeLink(c.thumbnail_url) ? h('img', { src: c.thumbnail_url, alt: '', loading: 'lazy', referrerpolicy: 'no-referrer', onerror: (e) => e.target.replaceWith(h('span', { class: 'cand-ph' }, icon('image'))) }) : h('span', { class: 'cand-ph' }, icon(c.kind === 'text' ? 'text' : 'image')),
              h('span', { class: 'cand-top' }, kindBadge(c.kind, 'sm'), c.duration_s ? h('span', { class: 'cand-dur mono' }, `${c.duration_s.toFixed(1)}s`) : null),
              b.chosen === i ? h('span', { class: 'cand-chosen' }, icon('check'), 'In use') : null,
              local.choosing === i ? h('span', { class: 'cand-busy' }, spinner()) : null),
              h('div', { class: 'cand-info' },
                h('span', { class: 'cand-title', title: c.title }, c.title),
                h('span', { class: 'cand-meta' }, h('span', { class: 'mono' }, c.provider), c.start_s != null ? h('span', { class: 'mono' }, `@${fmtClock(c.start_s)}`) : null,
                  c.page_url ? h('a', { href: c.page_url, target: '_blank', rel: 'noopener noreferrer', 'aria-label': 'Open source page', title: 'Open source page' }, icon('external')) : null)))))
            : h('div', { class: 'empty sm' }, h('div', { class: 'empty-icon' }, icon('search')), h('p', null, 'No candidates yet. Pick a kind, type a query and search.'))],
      ];
    });
  }

  ui.drawer = { el: dlg, close, render };
  document.body.append(dlg);
  document.body.classList.add('dialog-open');
  render();
  dlg.showModal();
  requestAnimationFrame(() => $('#swap-query, #swap-text', dlg)?.focus({ preventScroll: true }));
}

// ------------------------------------------------------------- queue view --

const QUEUE_STATUS = {
  waiting: ['Waiting', 'chip-idle'], running: ['Making', 'chip-running'], done: ['Done', 'chip-done'],
  failed: ['Failed', 'chip-failed'], cancelled: ['Cancelled', 'chip-paused'],
};
const QUEUE_MAX = 200;

function QueueView() {
  const form = {
    topics: sessionStorage.getItem('cc-draft-queue') || '',
    format: state.config?.video?.format || 'vertical', formatTouched: false,
    seconds: '', voice: '', captions: '', also: [],
    pub: { on: false, privacy: 'public', at: '', everyHours: '' },
    busy: false,
  };
  const acting = new Set(); // queue item ids with a request in flight
  refreshQueue();

  // Same rule as `clipcut batch`: one topic per line, blank and # lines skipped.
  const lines = () => form.topics.split('\n').map((l) => l.trim()).filter((l) => l && !l.startsWith('#'));
  const topics = h('textarea', {
    id: 'queue-topics', class: 'queue-topics', rows: 5, spellcheck: 'true', value: form.topics,
    placeholder: 'One topic per line\nWhy octopuses have three hearts\nThe 1904 Olympic marathon',
    oninput: (e) => { form.topics = e.target.value; sessionStorage.setItem('cc-draft-queue', form.topics); syncAdd(); },
    onkeydown: (e) => { if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) { e.preventDefault(); submit(); } },
  });
  const add = h('button', { type: 'submit', class: 'btn primary' });
  const count = h('span', { class: 'muted sm', 'aria-live': 'polite' });
  const syncAdd = () => {
    const n = lines().length;
    add.disabled = form.busy || !n || n > QUEUE_MAX;
    add.replaceChildren(form.busy ? spinner('dark') : icon('plus'), n > 1 ? `Queue ${n} videos` : 'Queue video');
    count.textContent = n > QUEUE_MAX ? `${n} topics; the limit is ${QUEUE_MAX} at once` : n ? `${n} topic${n === 1 ? '' : 's'}` : '';
    count.classList.toggle('err-text', n > QUEUE_MAX);
  };

  async function submit(e) {
    e?.preventDefault();
    const list = lines();
    if (!list.length || form.busy) { topics.focus(); return; }
    if (list.length > QUEUE_MAX) { toast(`Queue at most ${QUEUE_MAX} topics at once.`, 'error'); return; }
    if (form.pub.on && form.pub.at && !(new Date(form.pub.at) > Date.now())) {
      toast('Pick a first release time in the future, or leave it empty to publish on render.', 'error');
      return;
    }
    form.busy = true;
    syncAdd();
    try {
      const added = await api('POST', '/queue', {
        topics: list, format: form.format, target_seconds: form.seconds ? Number(form.seconds) : undefined,
        voice: form.voice || undefined, caption_preset: form.captions || undefined,
        extra_formats: form.also.length ? form.also : undefined, ...publishBody(form.pub),
      });
      form.topics = '';
      topics.value = '';
      sessionStorage.removeItem('cc-draft-queue');
      toast(`Queued ${added.length} video${added.length === 1 ? '' : 's'}`);
      refreshQueue();
    } catch (err) { toast(err.message, 'error'); }
    form.busy = false;
    syncAdd();
  }

  const formatSeg = segmented({ name: 'queue-format', label: 'Format', value: form.format, options: FORMATS.map(([v, ratio, ic]) => [v, ratio, ic]),
    onchange: (v) => { form.format = v; form.formatTouched = true; form.also = form.also.filter((f) => f !== v); update(); } });
  const lengthSel = h('select', { id: 'queue-length', class: 'select sm', 'aria-label': 'Length', onchange: (e) => { form.seconds = e.target.value; } });
  const voiceSel = h('select', { id: 'queue-voice', class: 'select sm', 'aria-label': 'Voice', onchange: (e) => { form.voice = e.target.value; } });
  const alsoWrap = h('div', { class: 'also-wrap' });
  const pubWrap = h('div', { class: 'clip-pub' });
  const ideas = ideasPicker((idea) => {
    if (lines().includes(idea.title)) { toast('Already in the list'); return; }
    const kept = form.topics.trimEnd();
    form.topics = kept ? `${kept}\n${idea.title}` : idea.title;
    topics.value = form.topics;
    sessionStorage.setItem('cc-draft-queue', form.topics);
    syncAdd();
  }, { id: 'queue-ideas' });

  const composer = h('form', { class: 'composer queue-composer', onsubmit: submit },
    h('label', { class: 'field-label', for: 'queue-topics' }, 'Topics'),
    topics,
    h('div', { class: 'composer-bar' },
      h('div', { class: 'composer-opts' },
        formatSeg,
        h('div', { class: 'with-icon' }, icon('film'), lengthSel),
        h('div', { class: 'with-icon' }, icon('volume'), voiceSel),
        h('div', { class: 'with-icon' }, icon('text'), captionSelect({ id: 'queue-captions', value: form.captions, onchange: (v) => { form.captions = v; } })),
        alsoWrap),
      h('div', { class: 'row gap' }, count, add)),
    pubWrap);
  syncAdd();

  const head = h('div', { class: 'section-head queue-head' });
  const list = h('ol', { class: 'queue-list' });
  const listWrap = h('div', { class: 'queue-items' });
  const el = h('div', { class: 'page queue' },
    h('header', { class: 'shead' }, h('h1', null, 'Queue'),
      h('p', { class: 'muted' }, 'Line up topics and ClipCut makes them one after another, each on autopilot. Leave it running and publish as they finish.')),
    composer, ideas,
    h('section', { class: 'queue-section' }, head, listWrap));

  async function act(item, what, method, path, body) {
    if (acting.has(item.id)) return;
    acting.add(item.id);
    update();
    try {
      const q = await api(method, path, body);
      if (q?.items) state.queue = q;
      if (what) toast(what);
    } catch (e) { toast(e.message, 'error'); }
    acting.delete(item.id);
    refreshQueue();
  }
  async function remove(item) {
    if (item.status === 'running' && !(await confirmDialog({ title: 'Stop this video?', body: 'The pipeline is cancelled and the item is marked cancelled. Its project keeps whatever was finished, so Retry picks up from there.', ok: 'Stop', danger: true }))) return;
    act(item, item.status === 'running' ? 'Stopped' : null, 'DELETE', `/queue/${encodeURIComponent(item.id)}`);
  }
  async function setPaused(paused) {
    try {
      state.queue = await api('PATCH', '/queue', { paused });
      toast(paused ? 'Queue paused. The current video still finishes.' : 'Queue resumed');
    } catch (e) { toast(e.message, 'error'); }
    update();
  }
  async function clearFinished() {
    try {
      const { removed } = await api('POST', '/queue/clear');
      toast(removed ? `Cleared ${removed} finished item${removed === 1 ? '' : 's'}` : 'Nothing to clear');
    } catch (e) { toast(e.message, 'error'); }
    refreshQueue();
  }

  function row(item, i) {
    const [label, chip] = QUEUE_STATUS[item.status] || [item.status, 'chip-idle'];
    const project = item.project_id ? state.projects?.find((p) => p.id === item.project_id) : null;
    const live = item.status === 'running' && item.project_id ? state.live.get(item.project_id) : null;
    const plan = item.publish;
    const busy = acting.has(item.id);
    const preset = CAPTION_PRESETS.find(([v]) => v === item.caption_preset)?.[1];
    const title = project?.title || item.topic;
    return h('li', { class: `qitem q-${item.status}` },
      h('span', { class: 'qnum mono', 'aria-hidden': 'true' }, i + 1),
      h('div', { class: 'qbody' },
        h('div', { class: 'qtopic' }, item.project_id ? h('a', { href: `#/p/${encodeURIComponent(item.project_id)}` }, title) : title),
        h('div', { class: 'qmeta' },
          h('span', { class: `chip ${chip}` }, item.status === 'running' ? h('span', { class: 'dot' }) : null,
            item.status === 'done' ? icon('check') : item.status === 'failed' ? icon('alert') : null, label),
          project?.kind === 'clip' ? h('span', { class: 'fmt-tag clip-tag' }, icon('scissors'), 'Clip') : null,
          h('span', { class: 'fmt-tag' }, [item.format || state.config?.video?.format || 'vertical', ...(item.extra_formats || [])].map((f) => FMT_LABEL[f] || f).join(' + ')),
          item.target_seconds ? h('span', { class: 'muted sm' }, `${item.target_seconds}s`) : null,
          preset ? h('span', { class: 'muted sm' }, `${preset} captions`) : null,
          plan ? h('span', { class: 'pub-tag', title: plan.publish_at ? `Goes ${plan.privacy} ${fmtWhen(plan.publish_at)}` : `Publishes as ${plan.privacy} when it renders` },
            icon(plan.publish_at ? 'calendar' : 'send'), plan.publish_at ? fmtWhen(plan.publish_at) : `Publish ${plan.privacy}`) : null,
          h('span', { class: 'muted sm nowrap' }, item.finished_at ? relTime(item.finished_at) : item.started_at ? `started ${relTime(item.started_at)}` : `added ${relTime(item.added_at)}`)),
        live ? h('div', { class: 'qlive muted sm ellipsis' }, live.message) : null,
        item.error && item.status !== 'cancelled' ? h('div', { class: 'qerr', title: item.error }, icon('alert'), h('span', { class: 'ellipsis' }, item.error)) : null,
        item.video_url && safeLink(item.video_url) ? h('a', { class: 'link sm', href: item.video_url, target: '_blank', rel: 'noopener noreferrer' }, icon('youtube'), 'Watch on YouTube', icon('external')) : null),
      h('div', { class: 'qactions' },
        item.status === 'failed' || item.status === 'cancelled' ? h('button', { type: 'button', class: 'btn ghost sm', disabled: busy,
          onclick: () => act(item, 'Back in line', 'POST', `/queue/${encodeURIComponent(item.id)}/retry`) }, busy ? spinner() : icon('rerun'), 'Retry') : null,
        h('button', { type: 'button', class: `icon-btn sm ${item.status === 'running' ? 'danger-text' : ''}`, disabled: busy,
          'aria-label': `${item.status === 'running' ? 'Stop' : 'Remove'} “${title}”`, title: item.status === 'running' ? 'Stop' : 'Remove',
          onclick: () => remove(item) }, icon(item.status === 'running' ? 'stop' : 'x'))));
  }

  return {
    el,
    hasUnsaved: () => false,
    update() {
      const cfg = state.config;
      if (cfg && !form.formatTouched && form.format !== cfg.video.format) {
        form.format = cfg.video.format;
        formatSeg.setValue(form.format);
        form.also = form.also.filter((f) => f !== form.format);
      }
      patch(lengthSel, `${cfg?.video?.target_seconds}`, () => [
        h('option', { value: '', selected: !form.seconds }, `Default length (${cfg?.video?.target_seconds || 45}s)`),
        [30, 45, 60, 90, 120, 180].map((n) => h('option', { value: n, selected: String(n) === form.seconds }, `${n}s`)),
      ]);
      patch(voiceSel, `${state.voices.length}|${cfg?.tts?.voice}`, () => voiceOptions(form.voice));
      patch(alsoWrap, JSON.stringify([form.format, form.also]), () => alsoExport({ id: 'queue-also', main: form.format, value: form.also,
        onchange: (v) => { form.also = v; update(); } }));
      patch(pubWrap, JSON.stringify([state.youtube?.connected, state.youtube?.channel, form.pub]), () =>
        publishOptions(form.pub, { id: 'queue-pub', onchange: update }));

      const q = state.queue;
      const items = q?.items || [];
      const running = items.find((i) => i.status === 'running');
      const waiting = items.filter((i) => i.status === 'waiting').length;
      const finished = items.filter((i) => !['waiting', 'running'].includes(i.status)).length;
      patch(head, JSON.stringify([!!q, q?.paused, items.length, waiting, !!running, finished]), () => [
        h('h2', null, 'Line-up'), items.length ? h('span', { class: 'count' }, items.length) : null,
        q ? h('span', { class: `queue-state muted sm ${q.paused ? 'warn-text' : ''}` },
          q.paused ? (running ? 'Paused after the current video' : 'Paused') : running ? `Making 1, ${waiting} waiting` : waiting ? `${waiting} waiting` : items.length ? 'All done' : '') : null,
        h('div', { class: 'row gap push' },
          q && (items.length || q.paused) ? h('button', { type: 'button', class: 'btn ghost sm', onclick: () => setPaused(!q.paused) },
            icon(q.paused ? 'play' : 'pause'), q.paused ? 'Resume' : 'Pause') : null,
          finished ? h('button', { type: 'button', class: 'btn ghost sm', onclick: clearFinished }, icon('trash'), 'Clear finished') : null),
      ]);
      if (state.queueError && !q) {
        patch(listWrap, `err:${state.queueError}`, () => errorState(state.queueError, refreshQueue));
        return;
      }
      if (!q) { patch(listWrap, 'loading', () => h('div', { class: 'loading-page' }, spinner('lg'), 'Loading the queue…')); return; }
      if (!items.length) {
        patch(listWrap, 'empty', () => emptyState('list', 'Nothing queued', 'Add a few topics above, or find clips in a long video from the home page. They render here one at a time.'));
        return;
      }
      patch(listWrap, 'list', () => list);
      keyed(list, items, (i) => i.id, (item) => {
        const live = item.status === 'running' && item.project_id ? state.live.get(item.project_id)?.message : null;
        const project = item.project_id ? state.projects?.find((p) => p.id === item.project_id) : null;
        return JSON.stringify([item, items.indexOf(item), live, project?.title, project?.kind, acting.has(item.id), Math.floor(Date.now() / 60000)]);
      }, (item) => row(item, items.indexOf(item)));
    },
    unmount() { disposeTree(el); },
  };
}

// ---------------------------------------------------------- settings view --

const SETTINGS_TABS = [['setup', 'Setup', 'wrench'], ['keys', 'API keys', 'key'], ['prefs', 'Preferences', 'sliders'], ['youtube', 'YouTube', 'send']];
const GCP = 'https://console.cloud.google.com';
const SETUP_TARGETS = [
  ['yt-dlp', 'yt-dlp', 'Downloads short YouTube cuts. Standalone binary, no Python.'],
  ['kokoro', 'Kokoro voice', 'Natural local text-to-speech (about 330 MB of model files).'],
  ['searxng', 'SearXNG', 'Private local meta-search in a container (needs podman or docker).'],
];
const MAX_KEYS = 5;
const KEY_STATE = { ready: ['Ready', 'ok'], cooling_down: ['Cooling down', 'warn'], invalid: ['Invalid', 'bad'] };

function SettingsView(initialTab) {
  const ui = { tab: SETTINGS_TABS.some(([t]) => t === initialTab) ? initialTab : 'setup', keyEdits: {}, cfg: null, cfgDirty: false, cfgSaving: false,
    yt: { client: '', paste: '', busy: null, poll: null } };
  const tabs = h('div', { class: 'tabs', role: 'tablist', 'aria-label': 'Settings sections' });
  const panes = Object.fromEntries(SETTINGS_TABS.map(([t]) => [t, h('section', { class: `pane spane-${t}`, role: 'tabpanel', id: `spane-${t}`,
    'aria-labelledby': `stab-${t}`, tabindex: 0, hidden: true })]));
  const el = h('div', { class: 'page settings' },
    h('header', { class: 'shead' }, h('h1', null, 'Settings'), h('p', { class: 'muted' }, 'Everything ClipCut needs runs locally or on free tiers.')),
    h('div', { class: 'tabs-wrap' }, tabs), Object.values(panes));

  if (!state.keys) loadKeys();
  refreshStatus();
  refreshYoutube();

  function setTab(t) {
    if (!SETTINGS_TABS.some(([id]) => id === t)) return;
    if (t !== ui.tab) stopVoicePreview();
    ui.tab = t;
    const want = `#/settings/${t}`;
    if (location.hash !== want) history.replaceState(null, '', want);
    currentRoute = parseRoute();
    update();
  }

  function render() {
    patch(tabs, `${ui.tab}|${state.status?.ready}|${state.status?.checks.filter((c) => !c.ok && c.required !== false).length}`, () => SETTINGS_TABS.map(([t, label, ic]) => h('button', {
      type: 'button', role: 'tab', class: 'tab', id: `stab-${t}`, 'aria-selected': String(ui.tab === t), 'aria-controls': `spane-${t}`,
      tabindex: ui.tab === t ? 0 : -1, onclick: () => setTab(t),
      onkeydown: (e) => {
        const i = SETTINGS_TABS.findIndex(([x]) => x === t);
        const j = radioKeyIndex(e, i, SETTINGS_TABS.length);
        if (j == null) return;
        const nt = SETTINGS_TABS[(j + SETTINGS_TABS.length) % SETTINGS_TABS.length][0];
        setTab(nt);
        requestAnimationFrame(() => $(`#stab-${nt}`)?.focus());
      },
    }, icon(ic), label, t === 'setup' && state.status && !state.status.ready ? h('span', { class: 'count warn' }, state.status.checks.filter((c) => !c.ok && c.required !== false).length) : null)));
    for (const [t, pane] of Object.entries(panes)) pane.hidden = t !== ui.tab;
    ({ setup: renderSetup, keys: renderKeys, prefs: renderPrefs, youtube: renderYoutube })[ui.tab](panes[ui.tab]);
  }

  // ---- setup / doctor
  function renderSetup(pane) {
    const s = state.status;
    const su = state.setup;
    patch(pane, JSON.stringify([s, state.statusError, su.running, su.log.length, su.progress, su.uncertain, state.sse]), () => {
      if (state.statusError && !s) return errorState(state.statusError, refreshStatus);
      if (!s) return h('div', { class: 'loading-page' }, spinner('lg'), 'Checking your system…');
      const failing = s.checks.filter((c) => !c.ok);
      const required = failing.filter((c) => c.required !== false);
      const optional = failing.filter((c) => c.required === false);
      const install = async (what) => {
        if (su.running || state.sse !== 'up') return;
        su.running = what;
        su.pending = new Set(what === 'all' ? SETUP_TARGETS.map(([id]) => id) : [what]);
        su.uncertain = false;
        su.progress = 0;
        su.log.push({ at: Date.now(), message: `Starting setup: ${what}`, level: 'info' });
        update();
        try {
          await api('POST', '/setup', { what });
        } catch (e) {
          su.running = null;
          su.pending.clear();
          su.log.push({ at: Date.now(), message: e.message, level: 'error' });
          toast(e.message, 'error');
          update();
        }
      };
      const lastErr = [...su.log].reverse().find((l) => l.level === 'error');
      return [
        state.statusError ? h('div', { class: 'callout' }, icon('alert'), h('p', null, `Status could not be refreshed: ${state.statusError}`),
          h('button', { class: 'btn sm', onclick: refreshStatus }, 'Retry')) : null,
        h('div', { class: `doctor-head card ${s.ready ? 'ok' : 'warn'}` },
          h('div', { class: 'doctor-icon' }, icon(s.ready ? 'check' : 'alert')),
          h('div', { class: 'grow' },
            h('h2', null, s.ready ? 'Ready to make videos' : `${required.length} required check${required.length === 1 ? '' : 's'} to fix`),
            h('p', { class: 'muted' }, s.ready ? optional.length ? `Required checks pass. ${optional.length} optional enhancement${optional.length === 1 ? '' : 's'} unavailable; fallback sources or voices may be used.` : 'Every check passes. Make some videos.'
              : 'Fix required tools before rendering. Optional sources can use fallbacks.')),
          h('button', { class: 'btn ghost sm', onclick: () => { refreshStatus(); toast('Re-checked'); } }, icon('rerun'), 'Re-check')),
        h('ul', { class: 'checks card' }, s.checks.map((c) => h('li', { class: `check ${c.ok ? 'ok' : 'bad'}` },
          h('span', { class: 'check-icon' }, icon(c.ok ? 'check' : 'x')),
          h('div', { class: 'check-body' },
            h('div', { class: 'check-name' }, c.name, h('span', { class: 'tag' }, c.required === false ? 'optional' : 'required')),
            c.detail ? h('div', { class: 'check-detail muted' }, c.detail) : null,
            !c.ok && c.fix ? h('div', { class: 'check-fix' }, icon('wrench'), h('code', null, c.fix)) : null)))),
        h('div', { class: 'setup card' },
          h('div', { class: 'pane-head' }, h('div', null, h('h2', null, 'One-click installs'),
            h('p', { class: 'muted' }, 'Downloads go into the ClipCut data folder. Nothing is installed system-wide.')),
          h('button', { class: 'btn primary', disabled: !!su.running || state.sse !== 'up', onclick: () => install('all') }, su.running === 'all' ? spinner('dark') : icon('download'), 'Install all')),
          state.sse !== 'up' ? h('p', { class: 'warn-text sm' }, 'Waiting for live updates before starting an install.') : null,
          h('div', { class: 'targets' }, SETUP_TARGETS.map(([id, label, desc]) => {
            const check = s.checks.find((c) => c.fix === `setup:${id}` || c.name.toLowerCase().includes(id));
            const ok = check?.ok;
            return h('div', { class: `target ${ok ? 'ok' : ''}` },
              h('div', { class: 'grow' }, h('div', { class: 'target-name' }, label, ok ? h('span', { class: 'tag tag-ok' }, icon('check'), 'Installed') : null),
                h('p', { class: 'muted sm' }, desc)),
              h('button', { class: `btn sm ${ok ? 'ghost' : ''}`, disabled: !!su.running || state.sse !== 'up', onclick: () => install(id) },
                su.running === id ? spinner() : icon(ok ? 'rerun' : 'download'), ok ? 'Reinstall' : 'Install'));
          })),
          su.running || su.log.length ? h('div', { class: 'setup-log' },
            su.running ? h('div', { class: 'setup-progress' }, h('span', null, spinner('xs'), `Installing ${su.running}…`), progressBar(su.progress || 0)) : null,
            su.running && su.uncertain ? h('div', { class: 'callout' }, icon('alert'), h('div', null,
              h('strong', null, 'Install status is uncertain'), h('p', null, 'Live updates were interrupted. Re-check the system; a missed completion cannot be recovered automatically.')),
              h('button', { class: 'btn sm', onclick: () => { su.running = null; su.pending.clear(); su.uncertain = false; refreshStatus(); update(); } }, 'Clear local tracker')) : null,
            !su.running && lastErr ? h('p', { class: 'err-text sm' }, icon('alert'), `Last error: ${lastErr.message}`) : null,
            h('ol', { class: 'log compact' }, su.log.slice(-12).reverse().map((l) => h('li', { class: `lvl-${l.level}` },
              h('time', null, new Date(l.at).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' })),
              h('span', { class: 'log-msg' }, l.message))))) : null),
        h('p', { class: 'muted sm foot-note' }, `ClipCut ${s.version} · data in `, h('code', null, s.home),
          s.network_url ? [' · open on your phone at ', h('a', { class: 'link', href: s.network_url }, s.network_url)] : null),
      ];
    });
  }

  // ---- API keys
  function renderKeys(pane) {
    const k = state.keys;
    patch(pane, JSON.stringify([k, state.keySaving, k ? Object.keys(ui.keyEdits).filter((s) => isDirty(s)) : 0,
      Object.entries(ui.keyEdits).map(([sid, e]) => [sid, [...e.removed]])]), () => {
      if (!k) return h('div', { class: 'loading-page' }, spinner('lg'), 'Loading keys…');
      if (k.error) return errorState(k.error, loadKeys);
      return [
        h('div', { class: 'callout' }, icon('info'), h('div', null,
          h('strong', null, 'All optional, all free tiers. '),
          'ClipCut works without any key (it falls back to DuckDuckGo, YouTube and text visuals). Keys add more sources. Add up to 5 per service: when one hits a rate limit, ClipCut rotates to the next.')),
        h('div', { class: 'services' }, k.services.filter((svc) => ['giphy', 'tenor', 'pexels', 'pixabay', 'unsplash', 'brave', 'tavily'].includes(svc.id)).map((svc) => serviceCard(svc))),
      ];
    });
  }
  function edits(sid) {
    return ui.keyEdits[sid] || (ui.keyEdits[sid] = { values: Array(MAX_KEYS).fill(''), removed: new Set() });
  }
  function isDirty(sid) {
    const e = ui.keyEdits[sid];
    return !!e && (e.values.some((v) => v.trim()) || e.removed.size > 0);
  }
  function clearKeyDraft(sid) {
    const draft = ui.keyEdits[sid];
    if (draft) { draft.values.fill(''); draft.removed.clear(); delete ui.keyEdits[sid]; }
    for (const input of el.querySelectorAll('input[type="password"]')) {
      if (!sid || input.dataset.service === sid) input.value = '';
    }
  }
  function serviceCard(svc) {
    const [name, ...rest] = svc.label.split(' — ');
    let desc = rest.join(' — ');
    const optional = /\(optional\)/i.test(desc);
    desc = desc.replace(/\s*\(optional\)\s*/i, '');
    const site = desc.match(/\(([\w.-]+\.\w+[^)]*)\)/);
    if (site) desc = desc.replace(site[0], '').trim();
    const e = edits(svc.id);
    const saving = state.keySaving === svc.id;
    const savingAny = !!state.keySaving;
    const dirty = isDirty(svc.id);
    const nKeys = svc.slots.length;
    const firstEmpty = nKeys;
    const visibleRows = MAX_KEYS;

    const save = async () => {
      if (state.keySaving || !isDirty(svc.id)) return;
      state.keySaving = svc.id;
      ++keysRequest; // ignore GET responses issued before this write
      update();
      const keys = e.values.map((v, i) => (e.removed.has(i) ? '__remove__' : v.trim()));
      try {
        state.keys = await api('PUT', `/keys/${encodeURIComponent(svc.id)}`, { keys });
        clearKeyDraft(svc.id);
        toast(`${name} keys saved`);
        refreshStatus();
      } catch (err) { toast(err.message, 'error'); }
      state.keySaving = null;
      update();
    };

    const rows = [];
    for (let i = 0; i < visibleRows; i++) {
      const slot = svc.slots.find((s) => s.slot === i);
      const removed = e.removed.has(i);
      const id = `key-${svc.id}-${i}`;
      const input = h('input', {
        id, class: 'input sm mono', type: 'password', autocomplete: 'new-password', spellcheck: 'false',
        'data-service': svc.id, 'aria-label': `${name} API key slot ${i + 1}`, 'aria-describedby': `hint-${svc.id}-${i}`,
        value: e.values[i], disabled: removed || savingAny,
        placeholder: slot ? 'Paste a new key to replace' : i === firstEmpty ? 'Paste an API key' : 'Another key (optional)',
        oninput: (ev) => {
          const was = isDirty(svc.id);
          e.values[i] = ev.target.value;
          if (was !== isDirty(svc.id) || (i === visibleRows - 1 && ev.target.value && visibleRows < MAX_KEYS)) {
            update();
            requestAnimationFrame(() => { const n = $(`#${id}`); if (n && document.activeElement !== n) { n.focus(); n.setSelectionRange(n.value.length, n.value.length); } });
          }
        },
        onkeydown: (ev) => { if (ev.key === 'Enter' && isDirty(svc.id)) { ev.preventDefault(); save(); } },
      });
      rows.push(h('div', { class: `key-row ${removed ? 'removed' : ''}` },
        h('label', { class: 'key-slot mono', for: id }, `#${i + 1}`),
        slot ? h('span', { id: `hint-${svc.id}-${i}`, class: 'key-hint mono', title: 'Masked saved key, last 4 characters only' }, `•••• ${String(slot.hint || '').slice(-4)}`) : h('span', { id: `hint-${svc.id}-${i}`, class: 'key-hint empty mono' }, 'empty'),
        slot ? h('span', { class: `kstate ks-${KEY_STATE[slot.state]?.[1] || 'ok'}` }, KEY_STATE[slot.state]?.[0] || slot.state) : h('span', { class: 'kstate-none' }),
        h('div', { class: 'key-input' }, input),
        slot ? h('button', {
          type: 'button', class: `btn ghost sm ${removed ? '' : 'danger-text'}`, disabled: savingAny,
          'aria-label': `${removed ? 'Undo removal of' : 'Remove'} ${name} key slot ${i + 1}`,
          onclick: () => { if (removed) e.removed.delete(i); else { e.removed.add(i); e.values[i] = ''; } update(); },
        }, removed ? [icon('rerun'), 'Undo'] : [icon('trash'), h('span', { class: 'hide-sm' }, 'Remove')]) : h('span')));
    }

    return h('article', { class: `svc card ${nKeys ? 'has-keys' : ''}` },
      h('header', { class: 'svc-head' },
        h('div', { class: 'grow' },
          h('h3', null, name, optional ? h('span', { class: 'tag' }, 'optional') : null, h('span', { class: 'tag tag-free' }, 'free tier')),
          h('p', { class: 'muted sm' }, desc, site ? [' · ', h('a', { class: 'link', href: `https://${site[1].split(/\s/)[0]}`, target: '_blank', rel: 'noopener noreferrer' }, site[1], icon('external'))] : null)),
        h('span', { class: `svc-count ${nKeys ? 'on' : ''}` }, nKeys ? `${nKeys} key${nKeys === 1 ? '' : 's'}` : 'no keys')),
      h('div', { class: 'key-rows' }, rows),
      dirty || saving ? h('div', { class: 'svc-foot' },
        e.removed.size ? h('span', { class: 'muted sm' }, `${e.removed.size} to remove`) : h('span'),
        h('div', { class: 'row gap' },
          h('button', { type: 'button', class: 'btn ghost sm', disabled: savingAny, onclick: () => { clearKeyDraft(svc.id); update(); } }, 'Discard'),
          h('button', { type: 'button', class: 'btn primary sm', disabled: savingAny, onclick: save }, saving ? spinner('dark') : icon('check'), 'Save'))) : null);
  }

  // ---- YouTube publishing
  function watchSignIn() {
    clearInterval(ui.yt.poll);
    const until = Date.now() + 180_000;
    ui.yt.poll = setInterval(async () => {
      const s = await refreshYoutube();
      if (s?.connected) toast(`Connected to ${s.channel || 'YouTube'}`);
      if (s?.connected || !s?.connecting || Date.now() > until) { clearInterval(ui.yt.poll); ui.yt.poll = null; }
    }, 2000);
  }
  function renderYoutube(pane) {
    const yt = state.youtube;
    const y = ui.yt;
    patch(pane, JSON.stringify([yt, y.busy]), () => {
      if (!yt) return h('div', { class: 'loading-page' }, spinner('lg'), 'Checking YouTube…');
      const busy = !!y.busy;
      const remote = !['localhost', '127.0.0.1', '[::1]'].includes(location.hostname);
      const ext = (href, text) => h('a', { class: 'link', href, target: '_blank', rel: 'noopener noreferrer' }, text, icon('external'));
      const act = async (what, fn) => {
        if (y.busy) return;
        y.busy = what;
        update();
        try { await fn(); } catch (e) { toast(e.message, 'error'); }
        y.busy = null;
        update();
      };
      const connect = () => act('connect', async () => {
        // Open the tab inside the click so popup blockers let it through.
        const win = window.open('about:blank', '_blank');
        if (win) win.opener = null;
        let url;
        try { ({ url } = await api('POST', '/publish/youtube/connect')); } catch (e) { win?.close(); throw e; }
        if (win && !win.closed) win.location.href = url; else window.open(url, '_blank', 'noopener');
        await refreshYoutube();
        watchSignIn();
      });
      const disconnect = async () => {
        if (!(await confirmDialog({ title: 'Disconnect YouTube?', body: 'ClipCut signs out and gives up its access. Videos you already uploaded stay on YouTube.', ok: 'Disconnect', danger: true }))) return;
        act('disconnect', async () => { state.youtube = await api('DELETE', '/publish/youtube'); toast('YouTube disconnected'); });
      };
      const forget = async () => {
        if (!(await confirmDialog({ title: 'Forget the OAuth client?', body: 'ClipCut signs out of YouTube and deletes the saved client ID and secret. Add the client again to publish.', ok: 'Forget client', danger: true }))) return;
        act('forget', async () => { state.youtube = await api('DELETE', '/publish/youtube?all=true'); toast('OAuth client removed'); });
      };
      const finish = () => act('paste', async () => {
        const url = y.paste.trim();
        if (!url) return;
        state.youtube = await api('POST', '/publish/youtube/callback-url', { url });
        y.paste = '';
        clearInterval(y.poll);
        if (state.youtube.connected) toast(`Connected to ${state.youtube.channel || 'YouTube'}`);
      });
      const saveClient = () => act('client', async () => {
        const json = y.client.trim();
        if (!json) return;
        state.youtube = await api('PUT', '/publish/youtube', { client_json: json });
        y.client = '';
        toast('OAuth client saved');
      });

      const status = yt.connected
        ? h('div', { class: 'doctor-head card ok' }, h('div', { class: 'doctor-icon' }, icon('check')),
          h('div', { class: 'grow' },
            h('h2', null, yt.channel ? `Connected to ${yt.channel}` : 'Connected to YouTube'),
            h('p', { class: 'muted' }, yt.connected_at ? `Signed in ${relTime(yt.connected_at)}. ` : '',
              'Finished videos get a Publish button, and the queue can publish as it goes.',
              yt.channel_id ? [' ', ext(`https://studio.youtube.com/channel/${encodeURIComponent(yt.channel_id)}`, 'YouTube Studio')] : null)),
          h('div', { class: 'row gap wrap' },
            h('button', { type: 'button', class: 'btn ghost sm', disabled: busy, onclick: connect }, y.busy === 'connect' ? spinner() : icon('rerun'), 'Reconnect'),
            h('button', { type: 'button', class: 'btn ghost sm danger-text', disabled: busy, onclick: disconnect }, y.busy === 'disconnect' ? spinner() : icon('x'), 'Disconnect')))
        : h('div', { class: 'doctor-head card warn' }, h('div', { class: 'doctor-icon' }, icon(yt.configured ? 'send' : 'alert')),
          h('div', { class: 'grow' },
            h('h2', null, yt.connecting ? 'Waiting for Google…' : yt.configured ? 'Ready to connect' : 'Not set up yet'),
            h('p', { class: 'muted' }, yt.connecting ? 'Finish signing in on the Google tab. This page updates when you are done.'
              : yt.configured ? 'Sign in with the Google account that owns your channel.'
                : 'Publishing uses your own free Google OAuth client. Follow the steps below once, then connect.')),
          yt.configured ? h('button', { type: 'button', class: 'btn primary', disabled: busy, onclick: connect },
            y.busy === 'connect' || yt.connecting ? spinner('dark') : icon('send'), yt.connecting ? 'Open sign-in again' : 'Connect YouTube') : null);

      const paste = yt.connecting || (remote && yt.configured && !yt.connected) ? h('div', { class: 'card yt-paste' },
        h('label', { class: 'field-label', for: 'yt-paste' }, remote ? 'Signing in from another device?' : 'Sign-in page didn’t load?'),
        h('p', { class: 'muted sm' }, 'After you allow access, Google sends the browser to an http://127.0.0.1 address, which only loads on the computer running ClipCut. ',
          'If that page fails, copy its full address from the address bar and paste it here.'),
        h('div', { class: 'row gap' },
          h('input', { id: 'yt-paste', class: 'input mono grow', value: y.paste, spellcheck: 'false', autocomplete: 'off', disabled: !yt.connecting,
            placeholder: yt.connecting ? 'http://127.0.0.1:…/?state=…&code=…' : 'Press Connect YouTube first',
            oninput: (e) => { y.paste = e.target.value; }, onkeydown: (e) => { if (e.key === 'Enter') { e.preventDefault(); finish(); } } }),
          h('button', { type: 'button', class: 'btn sm', disabled: busy || !yt.connecting, onclick: finish }, y.busy === 'paste' ? spinner() : icon('check'), 'Finish sign-in'))) : null;

      const save = h('button', { type: 'submit', class: 'btn primary sm', disabled: busy || !y.client.trim() },
        y.busy === 'client' ? spinner('dark') : icon('check'), yt.configured ? 'Replace client' : 'Save client');
      const sync = () => { save.disabled = !!y.busy || !y.client.trim(); };
      const area = h('textarea', { id: 'yt-client', class: 'input mono', rows: 4, spellcheck: 'false', autocomplete: 'off', value: y.client,
        placeholder: '{"installed":{"client_id":"….apps.googleusercontent.com","client_secret":"…"}}',
        oninput: (e) => { y.client = e.target.value; sync(); } });
      const file = h('input', { type: 'file', id: 'yt-client-file', class: 'sr-only', accept: '.json,application/json',
        onchange: async (e) => {
          const f = e.target.files?.[0];
          e.target.value = '';
          if (!f) return;
          if (f.size > 64 * 1024) { toast('That file is too big to be an OAuth client JSON.', 'error'); return; }
          y.client = await f.text();
          area.value = y.client;
          sync();
        } });
      const client = h('form', { class: 'card yt-client', onsubmit: (e) => { e.preventDefault(); saveClient(); } },
        h('div', { class: 'pane-head' },
          h('div', null, h('h2', null, 'OAuth client'),
            h('p', { class: 'muted sm' }, yt.configured
              ? ['Saved: ', h('code', null, yt.client_id || 'client'), '. The secret stays on this computer and is never shown again.']
              : 'Free, from your own Google Cloud project. The secret stays on this computer.')),
          yt.configured ? h('button', { type: 'button', class: 'btn ghost sm danger-text', disabled: busy, onclick: forget }, y.busy === 'forget' ? spinner() : icon('trash'), 'Forget client') : null),
        h('label', { class: 'field-label', for: 'yt-client' }, yt.configured ? 'Replace it with another client JSON' : 'Client JSON'),
        area,
        h('div', { class: 'row gap between wrap' },
          h('label', { class: 'btn ghost sm', for: 'yt-client-file' }, icon('upload'), 'Choose the downloaded file…', file), save));

      const steps = h('section', { class: 'card yt-steps' },
        h('h2', null, 'One-time setup, about 10 minutes'),
        h('ol', { class: 'steps' },
          h('li', null, ext(`${GCP}/projectcreate`, 'Create a Google Cloud project'), '. Any name works, and no billing account is needed.'),
          h('li', null, 'Enable the ', ext(`${GCP}/apis/library/youtube.googleapis.com`, 'YouTube Data API v3'), ' for that project.'),
          h('li', null, 'Set up the ', ext(`${GCP}/apis/credentials/consent`, 'OAuth consent screen'), ': choose External, then add your own Google account under Test users.'),
          h('li', null, 'Open ', ext(`${GCP}/apis/credentials`, 'Credentials'), ' and pick Create credentials → OAuth client ID → ', h('strong', null, 'Desktop app'), '. Download the JSON.'),
          h('li', null, 'Paste the JSON above or choose the file, save it, then press Connect YouTube.')));

      const limit = (ic, title, ...body) => h('li', null, icon(ic), h('div', null, h('strong', null, title), ' ', body));
      const limits = h('section', { class: 'card yt-limits' },
        h('h2', null, 'What Google allows'),
        h('ul', { class: 'limits' },
          limit('lock', 'Uploads are private until Google audits your project.', 'YouTube locks videos uploaded through an unaudited API project to private. ',
            ext('https://support.google.com/youtube/contact/yt_api_form', 'Request the free audit'), ' to publish publicly or on a schedule.'),
          limit('calendar', 'Test sign-ins last 7 days.', 'While the consent screen is in Testing, reconnect once a week, or push it to production and click through the “unverified app” warning.'),
          limit('bolt', 'About 6 uploads a day.', 'The free quota is 10,000 units a day and each upload costs about 1,600.'),
          limit('image', 'Custom thumbnails need a verified channel.', 'ClipCut sets one on 16:9 uploads; Shorts show a frame YouTube picks.'),
          limit('phone', 'TikTok isn’t supported.', 'Its posting API needs an app TikTok has audited, and unaudited apps can only post privately. Download the 9:16 MP4 from the Post kit and upload it in the TikTok app.')));

      return [status, paste, client, steps, limits];
    });
  }

  // ---- preferences
  function renderPrefs(pane) {
    if (!state.config) {
      patch(pane, state.configError || 'loading', () => state.configError ? errorState(state.configError, loadConfig) : h('div', { class: 'loading-page' }, spinner('lg'), 'Loading preferences…'));
      return;
    }
    if (!ui.cfg) ui.cfg = structuredClone(state.config);
    patch(pane, JSON.stringify([state.voices.length, ui.cfgBuild || 0]), () => prefsForm(pane));
    syncSaveBar(pane);
  }
  function syncSaveBar(pane) {
    const bar = $('.savebar', pane);
    if (!bar) return;
    bar.classList.toggle('show', ui.cfgDirty || ui.cfgSaving);
    bar.hidden = !ui.cfgDirty && !ui.cfgSaving;
    for (const input of pane.querySelectorAll('input, select, textarea, button')) {
      input.disabled = ui.cfgSaving || (voicePreview.button === input && !voicePreview.audio);
    }
  }
  function prefsForm(pane) {
    const c = ui.cfg;
    const dirty = () => { ui.cfgDirty = true; syncSaveBar(pane); };
    const bind = (sec, key, kind = 'text') => (e) => {
      const t = e.target;
      let v = kind === 'bool' ? t.checked : t.value;
      if (kind === 'num') v = t.value === '' ? c[sec][key] : Number(t.value);
      if (kind === 'opt') v = t.value.trim() || null;
      c[sec][key] = v;
      dirty();
      if (sec === 'tts' && key === 'engine') { ui.cfgBuild = (ui.cfgBuild || 0) + 1; update(); }
    };
    const field = (label, control, hint, id) => h('div', { class: 'field' }, h('label', { class: 'field-label', for: id }, label), control, hint ? h('p', { class: 'hint' }, hint) : null);
    const text = (sec, key, label, hint, extra = {}) => {
      const id = `cfg-${sec}-${key}`;
      return field(label, h('input', { id, class: 'input', value: c[sec][key] ?? '', oninput: bind(sec, key, extra.opt ? 'opt' : 'text'), ...extra.attrs }), hint, id);
    };
    const num = (sec, key, label, hint, attrs = {}) => {
      const id = `cfg-${sec}-${key}`;
      return field(label, h('input', { id, class: 'input', type: 'number', inputmode: 'decimal', required: true, value: c[sec][key], oninput: bind(sec, key, 'num'), ...attrs }), hint, id);
    };
    const select = (sec, key, label, options, hint) => {
      const id = `cfg-${sec}-${key}`;
      const numeric = typeof c[sec][key] === 'number';
      return field(label, h('select', { id, class: 'select', onchange: bind(sec, key, numeric ? 'num' : 'text') }, options.map(([v, t]) => h('option', { value: v, selected: String(c[sec][key]) === String(v) }, t))), hint, id);
    };
    const range = (sec, key, label, min, max, step, fmt = (v) => `${Math.round(v * 100)}%`) => {
      const id = `cfg-${sec}-${key}`;
      const out = h('output', { for: id, class: 'range-val mono' }, fmt(c[sec][key]));
      return h('div', { class: 'field' }, h('div', { class: 'field-label row between' }, h('label', { for: id }, label), out),
        h('input', { id, type: 'range', class: 'range', min, max, step, value: c[sec][key], oninput: (e) => { c[sec][key] = Number(e.target.value); out.textContent = fmt(c[sec][key]); dirty(); } }));
    };
    const sw = (sec, key, label, hint) => toggle({ label, hint, checked: !!c[sec][key], onchange: (v) => { c[sec][key] = v; dirty(); } });

    const preview = voicePreviewButton(() => c.tts.voice, 'settings-voice-preview');
    const voiceId = 'cfg-tts-voice';
    const voiceSel = h('select', { id: voiceId, class: 'select', onchange: bind('tts', 'voice') }, voiceOptions(c.tts.voice, { includeDefault: false }));

    const save = async () => {
      if (ui.cfgSaving || !ui.cfgDirty || !$('.prefs', pane).reportValidity()) return;
      ui.cfgSaving = true;
      syncSaveBar(pane);
      try {
        state.config = await api('PUT', '/config', structuredClone(ui.cfg));
        ui.cfg = structuredClone(state.config);
        ui.cfgDirty = false;
        ui.cfgBuild = (ui.cfgBuild || 0) + 1;
        toast('Preferences saved');
        refreshStatus();
      } catch (e) { toast(e.message, 'error'); }
      ui.cfgSaving = false;
      update();
    };
    const revert = () => { ui.cfg = structuredClone(state.config); ui.cfgDirty = false; ui.cfgBuild = (ui.cfgBuild || 0) + 1; update(); };
    const group = (title, ic, desc, ...kids) => h('section', { class: 'pgroup card' },
      h('header', null, h('div', { class: 'pgroup-icon' }, icon(ic)), h('div', null, h('h2', null, title), desc ? h('p', { class: 'muted sm' }, desc) : null)),
      h('div', { class: 'pgroup-body' }, kids));

    return h('form', { class: 'prefs', onsubmit: (e) => { e.preventDefault(); save(); } },
      group('Writer', 'doc', 'The local coding-agent CLI that researches and writes.',
        select('llm', 'provider', 'Provider', [['claude', 'Claude Code'], ['codex', 'Codex'], ...(['claude', 'codex'].includes(c.llm.provider) ? [] : [[c.llm.provider, c.llm.provider]])]),
        text('llm', 'model', 'Model', 'Used for the script. Leave empty for the CLI default.'),
        text('llm', 'fast_model', 'Fast model', 'Used for beat sheets and small calls.'),
        num('llm', 'timeout_seconds', 'Timeout (seconds)', null, { min: 10, max: 1800, step: 10 }),
        select('llm', 'unslop_preset', 'Unslop style', [['crisp', 'Crisp: short, punchy'], ['warm', 'Warm: friendly, human'], ['expert', 'Expert: precise, confident'], ['story', 'Story: narrative, vivid']], 'The rewrite pass that removes AI-sounding phrasing.'),
        h('div', { class: 'field span' }, sw('llm', 'skip_unslop', 'Skip the unslop pass', 'Faster, but scripts sound more generic'))),
      group('Video', 'film', null,
        select('video', 'format', 'Default format', [['vertical', '9:16 vertical (Shorts, Reels, TikTok)'], ['horizontal', '16:9 horizontal'], ['square', '1:1 square (feeds)']]),
        num('video', 'target_seconds', 'Default length (seconds)', null, { min: 10, max: 600, step: 5 }),
        select('video', 'caption_preset', 'Caption style', CAPTION_PRESETS.map(([v, label, desc]) => [v, `${label}: ${desc}`]), 'New projects use this unless you pick another.'),
        select('video', 'fps', 'Frame rate', [[24, '24 fps'], [25, '25 fps'], [30, '30 fps'], [60, '60 fps']]),
        num('video', 'crf', 'Quality (CRF)', 'Lower is better and bigger. 18–23 is a good range.', { min: 10, max: 35, step: 1 }),
        h('div', { class: 'field span' }, sw('video', 'captions', 'Burn in captions', 'Word-by-word animated captions'))),
      group('Voice', 'volume', 'Text-to-speech runs on this machine.',
        select('tts', 'engine', 'Engine', [['kokoro', 'Kokoro (natural)'], ['espeak', 'eSpeak (robotic fallback)'], ['command', 'Custom command']]),
        field('Voice', h('div', { class: 'row gap' }, voiceSel, preview), 'Preview uses the saved engine and speed. Save those changes first. Native word timings are approximate.', voiceId),
        range('tts', 'speed', 'Speed', 0.7, 1.4, 0.05, (v) => `${Number(v).toFixed(2)}×`),
        c.tts.engine === 'command' ? text('tts', 'command', 'Command', 'Shell template with {text_file} and {out_wav}; must write a WAV file.', { attrs: { required: true } }) : null),
      group('Audio mix', 'wave', null,
        range('audio', 'voice_volume', 'Voice', 0, 2, 0.05),
        range('audio', 'sfx_volume', 'Sound effects', 0, 2, 0.05),
        range('audio', 'music_volume', 'Music', 0, 1, 0.05),
        h('div', { class: 'field span row gap wrap' }, sw('audio', 'use_music', 'Background music'), sw('audio', 'duck_music', 'Duck music under voice'))),
      group('Sources', 'globe', null,
        text('search', 'searxng_url', 'SearXNG URL', 'Local meta-search. DuckDuckGo is used when it is down.', { attrs: { type: 'url', placeholder: 'http://127.0.0.1:8888' } }),
        text('youtube', 'cookies_from_browser', 'YouTube cookies from browser', 'Helps when YouTube asks to sign in. e.g. firefox, chrome. Empty = off.', { opt: true, attrs: { placeholder: 'firefox' } }),
        h('div', { class: 'field span row gap wrap' }, sw('search', 'manage_searxng', 'Start SearXNG automatically'), sw('search', 'duckduckgo_fallback', 'DuckDuckGo fallback'))),
      h('div', { class: 'savebar' },
        h('span', null, 'Unsaved changes'),
        h('div', { class: 'row gap' },
          h('button', { type: 'button', class: 'btn ghost sm', onclick: revert }, 'Revert'),
          h('button', { type: 'submit', class: 'btn primary sm' }, icon('check'), 'Save preferences'))));
  }

  return {
    el,
    settingsTab: () => ui.tab,
    hasUnsaved: () => ui.cfgDirty || Object.keys(ui.keyEdits).some(isDirty) || !!ui.yt.client.trim(),
    setTab: (t) => { if (t && t !== ui.tab) setTab(t); },
    update: render,
    unmount() { clearInterval(ui.yt.poll); ui.yt.client = ''; for (const sid of Object.keys(ui.keyEdits)) clearKeyDraft(sid); disposeTree(el); },
  };
}

async function loadKeys() {
  const request = ++keysRequest;
  try {
    const keys = await api('GET', '/keys');
    if (request === keysRequest) state.keys = keys;
  } catch (e) { if (request === keysRequest) state.keys = { error: e.message }; }
  update();
}

// ----------------------------------------------------------- router + boot --

function parseRoute() {
  let parts;
  try { parts = location.hash.replace(/^#\/?/, '').split('/').filter(Boolean).map(decodeURIComponent); } catch { return { name: 'home' }; }
  if (parts[0] === 'p' && parts[1]) return { name: 'project', id: parts[1], tab: TABS.some(([t]) => t === parts[2]) ? parts[2] : null };
  if (parts[0] === 'queue') return { name: 'queue' };
  if (parts[0] === 'settings') return { name: 'settings', tab: SETTINGS_TABS.some(([t]) => t === parts[1]) ? parts[1] : 'setup' };
  return { name: 'home' };
}

let currentRoute = parseRoute();

function mount() {
  const route = parseRoute();
  const prev = currentRoute;
  if (currentView && prev.name === route.name) {
    if (route.name === 'home' || route.name === 'queue') { currentRoute = route; update(); return; }
    if (route.name === 'project' && currentView.projectId === route.id) { currentRoute = route; currentView.setTab(route.tab); update(); return; }
    if (route.name === 'settings') { currentRoute = route; currentView.setTab(route.tab); update(); return; }
  }
  if (currentView?.hasUnsaved?.() && !window.confirm('Discard your unsaved edits and leave this page?')) {
    const old = prev.name === 'project' ? `#/p/${encodeURIComponent(prev.id)}${prev.tab ? `/${prev.tab}` : ''}`
      : prev.name === 'settings' ? `#/settings/${prev.tab}` : prev.name === 'queue' ? '#/queue' : '#/';
    history.replaceState(null, '', old);
    return;
  }
  currentRoute = route;
  currentView?.unmount();
  const app = $('#app');
  currentView = route.name === 'project' ? ProjectView(route.id, route.tab)
    : route.name === 'settings' ? SettingsView(route.tab)
      : route.name === 'queue' ? QueueView()
        : HomeView();
  if (route.name !== 'project') {
    document.title = route.name === 'settings' ? 'Settings · ClipCut' : route.name === 'queue' ? 'Queue · ClipCut' : 'ClipCut';
  }
  app.replaceChildren(currentView.el);
  app.classList.remove('enter');
  void app.offsetWidth;
  app.classList.add('enter');
  window.scrollTo({ top: 0 });
  if (route.name === 'home' || route.name === 'queue') refreshProjects();
  update();
}

window.addEventListener('hashchange', mount);
document.addEventListener('visibilitychange', () => {
  if (document.visibilityState !== 'visible') return;
  refreshProjects();
  if (currentView?.projectId) refreshProject(currentView.projectId);
  refreshStatus();
  refreshQueue();
});
$('.skip').addEventListener('click', (e) => { e.preventDefault(); $('#app').focus(); });
window.addEventListener('beforeunload', (e) => {
  if (!currentView?.hasUnsaved?.()) return;
  e.preventDefault(); e.returnValue = '';
});
setInterval(() => {
  if (document.visibilityState !== 'visible' || state.sse === 'up') return;
  refreshProjects();
  refreshQueue();
  if (currentView?.projectId) refreshProject(currentView.projectId);
}, 8000); // keep cancel/retry status usable while live updates reconnect
setInterval(update, 60_000); // relative times

connectEvents();
refreshStatus();
refreshProjects();
loadConfig();
loadVoices();
refreshQueue();
refreshYoutube();
mount();
