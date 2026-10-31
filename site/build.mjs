// Builds the ClipCut website into site/dist: the landing page, the docs
// rendered from the repo's Markdown files, the roadmap, a search index, a
// sitemap and a 404 page. Run `npm run build` in site/.
//
// The Markdown in docs/ and the repo root stays the source of truth, so it
// reads the same on GitHub. Links between those files are rewritten to site
// URLs, and a link to a heading that doesn't exist fails the build.

import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { Marked, Renderer } from 'marked';

const SITE = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(SITE, '..');
const OUT = path.join(SITE, 'dist');
const REPO = 'adeebahmad01/clipcut';
const GH = `https://github.com/${REPO}`;
// The canonical address for links in the sitemap and social cards. Vercel
// sets VERCEL_PROJECT_PRODUCTION_URL during its builds.
const SITE_URL = (process.env.SITE_URL
  || (process.env.VERCEL_PROJECT_PRODUCTION_URL && `https://${process.env.VERCEL_PROJECT_PRODUCTION_URL}`)
  || 'https://clipcut-sigma.vercel.app').replace(/\/$/, '');

// The docs, in sidebar order. `file` is relative to the repo root.
const GROUPS = [
  ['Getting started', [
    { slug: '', title: 'Overview', blurb: 'What ClipCut is and where to start' },
    { slug: 'installation', file: 'docs/Installation.md', title: 'Installation', blurb: 'Every OS, every dependency, building from source' },
    { slug: 'usage', file: 'docs/Usage.md', title: 'Usage', blurb: 'The app, the CLI, the queue, clip mode and publishing' },
  ]],
  ['Reference', [
    { slug: 'configuration', file: 'docs/Configuration.md', title: 'Configuration', blurb: 'Every setting in config.toml' },
    { slug: 'api', file: 'docs/API.md', title: 'HTTP API', blurb: 'The JSON API the web UI uses' },
    { slug: 'troubleshooting', file: 'docs/Troubleshooting.md', title: 'Troubleshooting', blurb: 'Fixes for common problems' },
  ]],
  ['Internals', [
    { slug: 'architecture', file: 'docs/Architecture.md', title: 'Architecture', blurb: 'How the crates fit together' },
    { slug: 'releasing', file: 'docs/Releasing.md', title: 'Releasing', blurb: 'Cutting a release and deploying the site' },
  ]],
  ['Project', [
    { slug: 'roadmap', file: 'ROADMAP.md', title: 'Roadmap', blurb: 'What ships next, as a checklist' },
    { slug: 'roadmap-prompts', file: 'ROADMAP-PROMPTS.md', title: 'Roadmap prompts', blurb: 'A ready prompt for an AI agent per roadmap item' },
    { slug: 'changelog', file: 'CHANGELOG.md', title: 'Changelog', blurb: 'Every change, release by release' },
    { slug: 'contributing', file: 'CONTRIBUTING.md', title: 'Contributing', blurb: 'Build it, test it, send a pull request' },
    { slug: 'security', file: 'SECURITY.md', title: 'Security', blurb: 'Reporting a vulnerability' },
  ]],
];
const docsUrl = (slug) => slug ? `/docs/${slug}` : '/docs';
const PAGES = GROUPS.flatMap(([group, pages]) => pages.map((p) => ({ ...p, group, url: docsUrl(p.slug) })));
const PAGE_BY_FILE = new Map(PAGES.filter((p) => p.file).map((p) => [p.file, p]));

const escapeHtml = (s) => String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);
const stripTags = (html) => html.replace(/<[^>]+>/g, '').replace(/&(amp|lt|gt|quot|#39);/g, (_, e) => ({ amp: '&', lt: '<', gt: '>', quot: '"', '#39': "'" })[e]);
const read = (rel) => fs.readFileSync(path.join(ROOT, rel), 'utf8');
const hash = (buf) => crypto.createHash('sha256').update(buf).digest('hex').slice(0, 10);

// GitHub's heading anchors: lowercase, drop punctuation, spaces to hyphens,
// and number repeats (`foo`, `foo-1`, `foo-2`).
function slugger() {
  const seen = new Map();
  return (text) => {
    const base = text.toLowerCase().trim().replace(/[^\p{L}\p{N}\s_-]/gu, '').replace(/\s/g, '-');
    let slug = base;
    if (seen.has(base)) {
      let n = seen.get(base);
      do slug = `${base}-${++n}`; while (seen.has(slug));
      seen.set(base, n);
    }
    seen.set(slug, 0);
    return slug;
  };
}

// --- Roadmap -----------------------------------------------------------------

function parseRoadmap(md) {
  const milestones = [];
  const slug = slugger();
  let cur = null;
  for (const line of md.split('\n')) {
    const h = line.match(/^## (.+)$/);
    if (h) {
      const title = h[1].trim();
      const [version, name] = title.includes(': ') ? title.split(/: (.*)/s) : ['', title];
      cur = { title, version, name, anchor: slug(title), status: '', goal: '', items: [] };
      milestones.push(cur);
      continue;
    }
    if (/^# /.test(line)) slug(line.slice(2));
    if (!cur) continue;
    const s = line.match(/^Status:\s*(\w+)/);
    const it = line.match(/^- \[([ xX])\] \*\*(?:(CC-\d+) )?(.+?)\*\*:?\s*(.*)$/);
    if (s) cur.status = s[1];
    else if (it) cur.items.push({ done: it[1] !== ' ', id: it[2] || '', title: it[3], text: it[4].replace(/\s*\(\[prompt\]\([^)]*\)\)\s*$/, '') });
    else if (line.trim() && !cur.goal && !cur.items.length) cur.goal = line.trim();
  }
  return milestones;
}

const count = (items) => ({ done: items.filter((i) => i.done).length, total: items.length });
const pct = ({ done, total }) => (total ? Math.round((done / total) * 100) : 0);
const bar = (c, label) => `<div class="bar" role="progressbar" aria-label="${escapeHtml(label)}" aria-valuemin="0" aria-valuemax="${c.total}" aria-valuenow="${c.done}"><i style="width:${pct(c)}%"></i></div>`;
const statusClass = (s) => `s-${s.toLowerCase()}`;

// The roadmap section on the landing page.
function roadmapSection(milestones) {
  const planned = milestones.flatMap((m) => m.items.filter((i) => i.id));
  const total = count(planned);
  const cards = milestones.map((m) => {
    const c = count(m.items);
    const shown = m.items.slice(0, 5);
    const more = m.items.length - shown.length;
    return `<li class="rm-card ${statusClass(m.status)}">
  <div class="rm-top"><span class="pill">${escapeHtml(m.status)}</span>${m.version ? `<span class="ver">${escapeHtml(m.version)}</span>` : ''}</div>
  <h3>${escapeHtml(m.name)}</h3>
  <p class="goal">${escapeHtml(m.goal)}</p>
  <div class="rm-count"><span>${c.done} of ${c.total} done</span>${bar(c, `${m.name}: ${c.done} of ${c.total} done`)}</div>
  <ul>${shown.map((i) => `<li class="${i.done ? 'done' : ''}"><span class="tick" aria-hidden="true"></span><span><span class="sr">${i.done ? 'Done: ' : 'To do: '}</span>${i.id ? `<code>${i.id}</code> ` : ''}${escapeHtml(i.title)}</span></li>`).join('')}</ul>
  <a class="rm-more" href="/docs/roadmap#${m.anchor}">${more > 0 ? `${more} more in ${escapeHtml(m.version || m.name)}` : 'See details'} <span aria-hidden="true">→</span></a>
</li>`;
  }).join('\n');
  return `<section id="roadmap" class="roadmap" aria-labelledby="roadmap-h">
  <div class="wrap">
    <div class="rm-head">
      <div>
        <h2 id="roadmap-h">Roadmap</h2>
        <p class="sub">What has shipped, what is being built now and what comes after. Every item is a checkbox in
          <a href="/docs/roadmap">ROADMAP.md</a>, ticked in the pull request that ships it.</p>
      </div>
      <div class="rm-total">
        <b>${total.done}<small>/${total.total}</small></b>
        <span>planned features shipped</span>
        ${bar(total, `${total.done} of ${total.total} planned features shipped`)}
      </div>
    </div>
    <ol class="rm-track" role="list">
${cards}
    </ol>
    <p class="fine rm-foot">Want to build one? Each item has a ready prompt for an AI coding agent in
      <a href="/docs/roadmap-prompts">the roadmap prompts</a>. Ideas and votes go in
      <a href="${GH}/issues/new?template=feature_request.yml">a feature request</a>.</p>
  </div>
</section>`;
}

// Puts a status pill and progress bar where each milestone's `Status:` line is.
function decorateRoadmap(md, milestones) {
  let i = -1;
  const planned = count(milestones.flatMap((m) => m.items.filter((it) => it.id)));
  const summary = `<div class="rm-summary"><b>${planned.done} of ${planned.total}</b> planned features shipped${bar(planned, 'Planned features shipped')}</div>\n\n`;
  return md
    .replace(/^## /m, `${summary}## `)
    .replace(/^## .+$|^Status:\s*(\w+).*$/gm, (line, status) => {
      if (line.startsWith('## ')) { i++; return line; }
      const c = count(milestones[i].items);
      return `<div class="ms-meta"><span class="pill ${statusClass(status)}">${escapeHtml(status)}</span><span>${c.done} of ${c.total} done</span>${bar(c, `${c.done} of ${c.total} done`)}</div>\n`;
    });
}

// --- Markdown ------------------------------------------------------------------

const anchorsByUrl = new Map(); // url -> Set of heading ids, for link checks
const pendingLinks = []; // { from, url, hash }

function rewriteHref(href, fromFile, text) {
  if (/^[a-z][a-z0-9+.-]*:|^\/\//i.test(href)) return href;
  const fromPage = PAGE_BY_FILE.get(fromFile);
  if (href.startsWith('#')) {
    pendingLinks.push({ from: fromFile, url: fromPage.url, hash: href.slice(1) });
    return href;
  }
  const [p, h = ''] = href.split('#');
  const rel = path.posix.normalize(path.posix.join(path.posix.dirname(fromFile), p));
  const page = PAGE_BY_FILE.get(rel);
  if (page) {
    let anchor = h;
    // `[CC-012](ROADMAP.md)` from the prompts points at the item itself.
    if (!anchor && page.slug === 'roadmap' && /^CC-\d+$/.test(text)) anchor = text.toLowerCase();
    if (anchor) pendingLinks.push({ from: fromFile, url: page.url, hash: anchor });
    return page.url + (anchor ? `#${anchor}` : '');
  }
  if (rel === 'README.md') return '/';
  const abs = path.join(ROOT, rel);
  const isDir = fs.existsSync(abs) ? fs.statSync(abs).isDirectory() : p.endsWith('/');
  return `${GH}/${isDir ? 'tree' : 'blob'}/main/${rel}${h ? `#${h}` : ''}`;
}

function renderMarkdown(md, file) {
  const slug = slugger();
  const toc = [];
  const sections = []; // for search: { anchor, title, text }
  let title = '';

  const marked = new Marked({
    gfm: true,
    renderer: {
      heading({ tokens, depth }) {
        const inner = this.parser.parseInline(tokens);
        const text = stripTags(inner);
        const id = slug(text);
        if (depth === 1 && !title) title = text;
        if (depth === 2 || depth === 3) toc.push({ depth, id, text });
        if (depth <= 3) sections.push({ anchor: depth === 1 ? '' : id, title: text, text: '' });
        if (depth === 1) return `<h1 id="${id}">${inner}</h1>\n`;
        return `<h${depth} id="${id}"><a class="anchor" href="#${id}" aria-hidden="true" tabindex="-1">#</a>${inner}</h${depth}>\n`;
      },
      code({ text, lang }) {
        const l = (lang || '').trim().split(/\s/)[0];
        return `<div class="code"${l ? ` data-lang="${escapeHtml(l)}"` : ''}><pre><code>${escapeHtml(text)}</code></pre></div>\n`;
      },
      table(token) {
        return `<div class="table">${Renderer.prototype.table.call(this, token)}</div>\n`;
      },
      listitem(item) {
        let html = Renderer.prototype.listitem.call(this, item);
        if (item.task) {
          const label = item.text.match(/^\*\*(.+?)\*\*/)?.[1] || item.text;
          html = html.replace('<input ', `<input aria-label="${escapeHtml(label)}" `);
        }
        // Give each roadmap item an anchor, so `#cc-012` lands on it.
        const id = item.task && item.text.match(/^\*\*(CC-\d+) /);
        return id ? html.replace(/^<li>/, `<li id="${id[1].toLowerCase()}" class="task">`) : item.task ? html.replace(/^<li>/, '<li class="task">') : html;
      },
    },
  });

  const tokens = marked.lexer(md);
  marked.walkTokens(tokens, (token) => {
    if (token.type === 'link' || token.type === 'image') token.href = rewriteHref(token.href, file, token.text);
  });
  // Drop a hand-written table of contents right under the title; the page
  // has its own.
  const first = tokens.findIndex((t) => t.type !== 'space' && t.type !== 'heading');
  if (first > -1 && tokens[first].type === 'list' && tokens[first].items.every((i) => /^\[[^\]]+\]\(#[^)]+\)$/.test(i.text.trim()))) {
    tokens.splice(first, 1);
  }
  const html = marked.parser(tokens);

  // Search text per section, taken from the raw Markdown between headings.
  let si = -1;
  for (const t of tokens) {
    if (t.type === 'heading' && t.depth <= 3) { si++; continue; }
    if (si < 0 || t.type === 'space' || t.type === 'html') continue;
    sections[si].text += ' ' + t.raw;
  }
  for (const s of sections) {
    s.text = s.text
      .replace(/```[^\n]*\n?/g, ' ')
      .replace(/!?\[([^\]]*)\]\([^)]*\)/g, '$1')
      .replace(/<[^>]+>/g, ' ')
      .replace(/[*_`|>#]+/g, ' ')
      .replace(/-{3,}/g, ' ')
      .replace(/\s+/g, ' ')
      .trim();
  }

  return { html, toc, title, sections };
}

// --- Templates -----------------------------------------------------------------

let ASSET = {}; // 'assets/x.svg' -> 'assets/x.<hash>.svg', filled in by build()

function head({ title, description, url, image = '/og.png' }) {
  return `<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<title>${escapeHtml(title)}</title>
<meta name="description" content="${escapeHtml(description)}">
<link rel="canonical" href="${SITE_URL}${url}">
<meta property="og:title" content="${escapeHtml(title)}">
<meta property="og:description" content="${escapeHtml(description)}">
<meta property="og:image" content="${SITE_URL}${image}">
<meta property="og:url" content="${SITE_URL}${url}">
<meta property="og:type" content="website">
<meta name="twitter:card" content="summary_large_image">
<meta name="theme-color" content="#0a0a0b">
<meta name="color-scheme" content="dark">
<link rel="icon" href="/favicon.svg" type="image/svg+xml">
${url.startsWith('/docs') ? '' : '<link rel="preload" href="/assets/fonts/anton-latin.woff2" as="font" type="font/woff2" crossorigin>'}
<link rel="stylesheet" href="/assets/style.css">
<script src="/assets/site.js" defer></script>
<noscript><style>
.menu-btn,.side-btn,.side-close,.search,.copy{display:none!important}
@media(max-width:860px){.sidebar{position:static;transform:none;visibility:visible;width:100%;border:0}.side-nav ul{display:flex;flex-wrap:wrap;gap:4px}}
@media(max-width:720px){.top{position:static}.top-in{height:auto;min-height:60px;flex-wrap:wrap;padding-top:12px;padding-bottom:12px}#main-nav{position:static;display:flex;flex-direction:row;flex-wrap:wrap;padding:0;border:0;box-shadow:none}#main-nav .gh{margin:0}}
</style></noscript>`;
}

function header(active) {
  const link = (href, label, key) => `<a href="${href}"${active === key ? ' aria-current="page"' : ''}>${label}</a>`;
  return `<a class="skip" href="#main">Skip to content</a>
<header class="top">
  <div class="wrap top-in">
    <a class="brand" href="/">
      <img src="/assets/logo.svg" width="26" height="26" alt="">
      <span>ClipCut</span>
    </a>
    <button class="menu-btn" type="button" aria-expanded="false" aria-controls="main-nav" aria-label="Menu">
      <svg width="22" height="22" viewBox="0 0 24 24" aria-hidden="true"><path d="M4 7h16M4 12h16M4 17h16" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>
    </button>
    <nav id="main-nav" aria-label="Main">
      ${link('/#download', 'Download', 'download')}
      ${link('/docs', 'Docs', 'docs')}
      ${link('/#roadmap', 'Roadmap', 'roadmap')}
      ${link('/docs/changelog', 'Changelog', 'changelog')}
      <a class="gh" href="${GH}">GitHub</a>
    </nav>
  </div>
</header>`;
}

function footer() {
  return `<footer>
  <div class="wrap foot">
    <span>ClipCut · MIT license · fonts under the SIL OFL</span>
    <span><a href="/docs">Docs</a> · <a href="/docs/roadmap">Roadmap</a> · <a href="${GH}">GitHub</a> · <a href="${GH}/releases">Releases</a> · <a href="${GH}/issues">Issues</a></span>
  </div>
</footer>`;
}

function sidebar(current) {
  return `<aside class="sidebar" id="sidebar" aria-label="Documentation">
  <button class="side-close" type="button">Close docs menu <span aria-hidden="true">×</span></button>
  <div class="search" role="search">
    <label class="sr" for="q">Search the docs</label>
    <input id="q" type="search" role="combobox" aria-autocomplete="list" aria-haspopup="listbox" placeholder="Search docs" autocomplete="off" spellcheck="false" aria-controls="q-results" aria-expanded="false">
    <kbd aria-hidden="true">/</kbd>
    <div class="results" id="q-results" role="listbox" aria-label="Search results" hidden></div>
    <span class="sr" id="q-status" role="status"></span>
  </div>
  <nav class="side-nav" aria-label="Docs">
${GROUPS.map(([group, pages]) => `    <p>${group}</p>
    <ul>${pages.map((p) => `<li><a href="${docsUrl(p.slug)}"${p.slug === current.slug ? ' aria-current="page"' : ''}>${p.title}</a></li>`).join('')}</ul>`).join('\n')}
  </nav>
</aside>`;
}

function tocHtml(toc) {
  if (toc.length < 2) return '';
  return `<ul>${toc.map((t) => `<li class="d${t.depth}"><a href="#${t.id}">${escapeHtml(t.text)}</a></li>`).join('')}</ul>`;
}

function docsPage(page, body, toc) {
  const i = PAGES.indexOf(page);
  const prev = PAGES[i - 1];
  const next = PAGES[i + 1];
  const tocList = tocHtml(toc);
  const title = page.slug ? `${page.title} · ClipCut docs` : 'ClipCut docs';
  return `<!doctype html>
<html lang="en">
<head>
${head({ title, description: `${page.blurb}. ClipCut documentation.`, url: page.url })}
</head>
<body class="docs-body" data-search="/assets/search.json">
${header('docs')}
<div class="docs-shell">
${sidebar(page)}
  <div class="scrim" hidden></div>
  <main id="main" class="doc">
    <div class="doc-bar">
      <button class="side-btn" type="button" aria-expanded="false" aria-controls="sidebar">
        <svg width="18" height="18" viewBox="0 0 24 24" aria-hidden="true"><path d="M4 6h16M4 12h10M4 18h16" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>
        Docs menu
      </button>
      <span class="crumb">${escapeHtml(page.group)} <span aria-hidden="true">/</span> ${escapeHtml(page.title)}</span>
    </div>
${tocList ? `    <details class="toc-inline"><summary>On this page</summary>${tocList}</details>` : ''}
    <article class="prose">
${body}
    </article>
    <div class="doc-meta">${page.file ? `<a href="${GH}/edit/main/${page.file}">Edit this page on GitHub</a>` : ''}</div>
    <nav class="pager" aria-label="Previous and next page">
      ${prev ? `<a class="prev" href="${prev.url}"><small>Previous</small><b>${prev.title}</b></a>` : '<span></span>'}
      ${next ? `<a class="next" href="${next.url}"><small>Next</small><b>${next.title}</b></a>` : '<span></span>'}
    </nav>
  </main>
${tocList ? `  <aside class="toc" aria-label="On this page"><p>On this page</p>${tocList}</aside>` : '  <aside class="toc"></aside>'}
</div>
${footer()}
</body>
</html>
`;
}

function overviewBody() {
  return `<h1 id="clipcut-docs">ClipCut docs</h1>
<p class="lead">ClipCut turns a topic into a fast-cut short video made from YouTube moments, article screenshots,
memes and GIFs, with word-by-word captions and a local AI voiceover. It runs on your machine as one binary with a
built-in web UI and a command line.</p>
<div class="code"><pre><code>curl -fsSL https://github.com/${REPO}/releases/latest/download/install.sh | sh
clipcut</code></pre></div>
<p>New here? Start with <a href="/docs/installation">Installation</a>, then <a href="/docs/usage">Usage</a>.
If something breaks, <a href="/docs/troubleshooting">Troubleshooting</a> covers the common problems, and
<code>clipcut doctor</code> checks every dependency.</p>
${GROUPS.map(([group, pages]) => {
    const list = pages.filter((p) => p.slug);
    return `<h2 id="${group.toLowerCase().replace(/\s/g, '-')}">${group}</h2>
<div class="cards">${list.map((p) => `<a href="${docsUrl(p.slug)}"><b>${p.title}</b><span>${p.blurb}</span></a>`).join('')}</div>`;
  }).join('\n')}`;
}

function notFoundPage() {
  return `<!doctype html>
<html lang="en">
<head>
${head({ title: 'Page not found · ClipCut', description: 'This page does not exist.', url: '/404' })}
<meta name="robots" content="noindex">
</head>
<body>
${header('')}
<main id="main" class="nf">
  <div class="wrap">
    <p class="eyebrow">404</p>
    <h1>Cut.</h1>
    <p class="lede">This page is not in the edit. It may have moved when the docs came to the website.</p>
    <div class="cta"><a class="btn primary" href="/">Home</a><a class="btn" href="/docs">Docs</a></div>
  </div>
</main>
${footer()}
</body>
</html>
`;
}

// --- Assets ----------------------------------------------------------------------

const minifyCss = (css) => css
  .replace(/\/\*[\s\S]*?\*\//g, '')
  .replace(/\s+/g, ' ')
  .replace(/\s*([{};,>])\s*/g, '$1')
  .replace(/:\s+/g, ':')
  .replace(/;}/g, '}')
  .trim();

// Swaps every `/assets/...` reference for its hashed name.
const hashRefs = (text) => text.replace(/\/assets\/[\w./-]+/g, (ref) => (ASSET[ref] ? ASSET[ref] : ref));

function emitHashed(rel, buf) {
  const ext = path.extname(rel);
  const out = `${rel.slice(0, -ext.length)}.${hash(buf)}${ext}`;
  fs.mkdirSync(path.dirname(path.join(OUT, out)), { recursive: true });
  fs.writeFileSync(path.join(OUT, out), buf);
  ASSET[`/${rel}`] = `/${out}`;
}

function copyAssets(dir = 'assets') {
  for (const entry of fs.readdirSync(path.join(SITE, dir), { withFileTypes: true })) {
    const rel = `${dir}/${entry.name}`;
    if (entry.isDirectory()) { copyAssets(rel); continue; }
    // PNGs are the README's screenshots; the site serves WebP.
    if (entry.name.endsWith('.png') || entry.name.endsWith('.ttf')) continue;
    const buf = fs.readFileSync(path.join(SITE, rel));
    if (entry.name.endsWith('.txt')) {
      fs.mkdirSync(path.dirname(path.join(OUT, rel)), { recursive: true });
      fs.writeFileSync(path.join(OUT, rel), buf);
    } else {
      emitHashed(rel, buf);
    }
  }
}

function write(rel, text) {
  const file = path.join(OUT, rel);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, text);
}

// --- Build ---------------------------------------------------------------------

function build() {
  const started = Date.now();
  fs.rmSync(OUT, { recursive: true, force: true });
  fs.mkdirSync(OUT, { recursive: true });
  ASSET = {};
  anchorsByUrl.clear();
  pendingLinks.length = 0;

  copyAssets();
  fs.copyFileSync(path.join(SITE, 'favicon.svg'), path.join(OUT, 'favicon.svg'));
  fs.copyFileSync(path.join(SITE, 'assets/screenshot-home.png'), path.join(OUT, 'og.png'));

  // Docs pages.
  const search = [];
  const roadmapMd = read('ROADMAP.md');
  const milestones = parseRoadmap(roadmapMd);
  for (const page of PAGES) {
    let body;
    let toc = [];
    if (page.file) {
      const md = page.slug === 'roadmap' ? decorateRoadmap(roadmapMd, milestones) : read(page.file);
      const r = renderMarkdown(md, page.file);
      body = r.html;
      toc = r.toc;
      anchorsByUrl.set(page.url, new Set([...body.matchAll(/ id="([^"]+)"/g)].map((m) => m[1])));
      for (const s of r.sections) {
        if (!s.text && !s.anchor) continue;
        const text = page.slug === 'roadmap-prompts' && s.text.length > 1800 ? s.text.slice(0, 1800) : s.text;
        search.push({ t: s.anchor ? `${page.title} › ${s.title}` : page.title, u: page.url + (s.anchor ? `#${s.anchor}` : ''), x: text });
      }
    } else {
      body = overviewBody();
    }
    write(page.slug ? `docs/${page.slug}.html` : 'docs/index.html', docsPage(page, body, toc));
  }

  const broken = pendingLinks.filter((l) => !anchorsByUrl.get(l.url)?.has(l.hash));
  if (broken.length) {
    for (const l of broken) console.error(`Broken link in ${l.from}: ${l.url}#${l.hash}`);
    process.exit(1);
  }
  emitHashed('assets/search.json', Buffer.from(JSON.stringify(search)));

  // Landing page.
  const landing = fs.readFileSync(path.join(SITE, 'index.html'), 'utf8')
    .replace('{{head}}', head({
      title: 'ClipCut: topic in, short video out',
      description: 'ClipCut turns a topic into a fast-cut short video made from YouTube moments, article screenshots, memes and GIFs, with a local AI voiceover. Free and open source for Linux and macOS.',
      url: '/',
    }))
    .replace('{{header}}', header('home'))
    .replace('{{roadmap}}', roadmapSection(milestones))
    .replace('{{footer}}', footer());
  write('index.html', landing);
  write('404.html', notFoundPage());

  // CSS and JS last, so their own asset references are already hashed.
  emitHashed('assets/style.css', Buffer.from(minifyCss(hashRefs(fs.readFileSync(path.join(SITE, 'style.css'), 'utf8')))));
  emitHashed('assets/site.js', fs.readFileSync(path.join(SITE, 'site.js')));
  for (const file of fs.readdirSync(OUT, { recursive: true })) {
    if (!file.endsWith('.html')) continue;
    const p = path.join(OUT, file);
    fs.writeFileSync(p, hashRefs(fs.readFileSync(p, 'utf8')));
  }

  const urls = ['/', ...PAGES.map((p) => p.url)];
  write('sitemap.xml', `<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n${urls.map((u) => `  <url><loc>${SITE_URL}${u === '/' ? '' : u}</loc></url>`).join('\n')}\n</urlset>\n`);
  write('robots.txt', `User-agent: *\nAllow: /\nSitemap: ${SITE_URL}/sitemap.xml\n`);

  console.log(`Built ${PAGES.length + 2} pages and ${search.length} search entries into site/dist in ${Date.now() - started} ms`);
}

build();
