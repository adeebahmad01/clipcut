import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFileSync, spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { gzipSync } from 'node:zlib';
import { test } from 'node:test';

const site = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const root = path.resolve(site, '..');
const out = path.join(site, 'dist');
const read = (rel) => fs.readFileSync(path.join(out, rel), 'utf8');
const pages = fs.readdirSync(out, { recursive: true }).filter((p) => p.endsWith('.html'));
const ids = (html) => [...html.matchAll(/\bid="([^"]+)"/g)].map((m) => m[1]);
const route = (file) => `/${file.replace(/(^|\/)index\.html$/, '').replace(/\.html$/, '').replace(/\/$/, '')}`;
const resolveFile = (url) => {
  const name = decodeURIComponent(url.pathname).replace(/^\//, '');
  return [name, `${name}.html`, `${name ? `${name}/` : ''}index.html`]
    .find((p) => fs.existsSync(path.join(out, p)) && fs.statSync(path.join(out, p)).isFile());
};
const pageIds = new Map(pages.map((p) => [p, new Set(ids(read(p)))]));
const roadmap = fs.readFileSync(path.join(root, 'ROADMAP.md'), 'utf8');
const prompts = fs.readFileSync(path.join(root, 'ROADMAP-PROMPTS.md'), 'utf8');
const features = [...roadmap.matchAll(/^- \[([ xX])\] \*\*(CC-\d+) (.+?)\*\*/gm)];

test('all local links, heading anchors, images, scripts, fonts and search indexes resolve', () => {
  for (const file of pages) {
    const html = read(file);
    assert.doesNotMatch(html, /\{\{(?:head|header|footer|roadmap)\}\}/, file);
    assert.doesNotMatch(html, /(?:href|src)="(?:undefined|null)"/, file);
    assert.equal(new Set(ids(html)).size, ids(html).length, `Duplicate IDs in ${file}`);
    const urls = [...html.matchAll(/\b(?:href|src|data-search)="([^"]+)"/g)].map((m) => m[1]);
    for (const [, srcset] of html.matchAll(/\bsrcset="([^"]+)"/g)) {
      urls.push(...srcset.split(',').map((s) => s.trim().split(/\s+/)[0]));
    }
    for (const href of urls) {
      const url = new URL(href, `https://build.test${route(file)}`);
      if (url.origin !== 'https://build.test') continue;
      const target = resolveFile(url);
      assert.ok(target, `${file}: missing ${href}`);
      if (url.hash) {
        assert.ok(pageIds.get(target)?.has(decodeURIComponent(url.hash.slice(1))), `${file}: missing anchor ${href}`);
      }
    }
  }
  for (const css of fs.readdirSync(path.join(out, 'assets')).filter((p) => p.endsWith('.css'))) {
    for (const [, href] of read(`assets/${css}`).matchAll(/url\(["']?(\/assets\/[^)"']+)/g)) {
      assert.ok(resolveFile(new URL(href, 'https://build.test')), `Missing CSS asset ${href}`);
    }
  }
  const index = fs.readdirSync(path.join(out, 'assets')).find((p) => /^search\..+\.json$/.test(p));
  const entries = JSON.parse(read(`assets/${index}`));
  assert.ok(entries.length > 50, 'Search covers documentation sections');
  for (const entry of entries) {
    assert.ok(entry.t && entry.u && typeof entry.x === 'string', 'Search entry has a title, URL and searchable text');
    const url = new URL(entry.u, 'https://build.test');
    const target = resolveFile(url);
    assert.ok(target, `Search target ${entry.u}`);
    if (url.hash) assert.ok(pageIds.get(target)?.has(decodeURIComponent(url.hash.slice(1))), `Search anchor ${entry.u}`);
  }
  for (const term of ['--disconnect', '--force']) {
    assert.ok(entries.some((e) => e.u.startsWith('/docs/usage') && e.x.includes(term)), `Search includes late-section term ${term}`);
  }
});

test('each docs page highlights precisely its own sidebar link', () => {
  for (const file of pages.filter((p) => p.startsWith('docs/'))) {
    const sidebar = read(file).match(/<nav class="side-nav"[\s\S]*?<\/nav>/)?.[0];
    assert.ok(sidebar, file);
    const current = [...sidebar.matchAll(/<a href="([^"]+)" aria-current="page"/g)];
    assert.equal(current.length, 1, `One current docs page in ${file}`);
    assert.equal(current[0][1], route(file), file);
  }
});

test('docs overview cards lead to all 12 published documents', () => {
  const html = read('docs/index.html');
  const cards = [...html.matchAll(/<div class="cards">([\s\S]*?)<\/div>/g)].map((m) => m[1]).join('');
  const links = [...cards.matchAll(/href="([^"]+)"/g)].map((m) => m[1]);
  assert.equal(links.length, 12);
  assert.equal(new Set(links).size, links.length);
  for (const link of links) assert.match(link, /^\/docs\/[\w-]+$/);
});

test('50+ stable roadmap IDs have exactly one matching, copyable implementation prompt', () => {
  assert.ok(features.length >= 50, `Expected 50+ planned features; got ${features.length}`);
  const headings = [...prompts.matchAll(/^## (CC-\d+) (.+)$/gm)];
  const headingMap = new Map(headings.map((m) => [m[1], m[2].trim()]));
  assert.equal(new Set(features.map((m) => m[2])).size, features.length, 'Duplicate roadmap ID');
  assert.equal(headings.length, features.length, 'One prompt heading per roadmap feature');
  assert.equal(headingMap.size, headings.length, 'Duplicate prompt ID');
  for (const [, , id, title] of features) assert.equal(headingMap.get(id), title, id);
  const blocks = prompts.split(/^## CC-\d+ .+$/m).slice(1);
  for (const [i, block] of blocks.entries()) {
    assert.match(block, /```text\n[\s\S]+?```/, `Copyable prompt for ${headings[i][1]}`);
    assert.match(block, /(?:acceptance|done when|test)/i, `Verification criteria for ${headings[i][1]}`);
  }
});

test('website checklist and landing progress follow ROADMAP.md', () => {
  const html = read('docs/roadmap.html');
  const tasks = [...html.matchAll(/<input [^>]*type="checkbox"[^>]*>/g)].map((m) => m[0]);
  const sourceTasks = [...roadmap.matchAll(/^- \[([ xX])\] /gm)];
  assert.equal(tasks.length, sourceTasks.length);
  assert.equal(tasks.filter((t) => /\bchecked(?:[ =>])/.test(t)).length, sourceTasks.filter((m) => m[1] !== ' ').length);
  for (const task of tasks) {
    assert.match(task, /\bdisabled\b/, 'Status reflects Git, not a misleading browser-only checkbox');
    assert.match(task, /aria-label="[^"]+"/, 'Checkbox accessible name');
  }
  for (const [, , id] of features) assert.ok(pageIds.get('docs/roadmap.html').has(id.toLowerCase()));
  const done = features.filter((m) => m[1] !== ' ').length;
  assert.ok(read('index.html').includes(`${done}<small>/${features.length}</small>`));
  assert.equal((read('index.html').match(/class="rm-card /g) || []).length, (roadmap.match(/^## /gm) || []).length);
});

test('assets are fingerprinted, mobile screenshots are smaller, and page payloads stay small', () => {
  for (const rel of fs.readdirSync(path.join(out, 'assets'), { recursive: true })) {
    const file = path.join(out, 'assets', rel);
    if (fs.statSync(file).isDirectory() || rel.endsWith('.txt')) continue;
    assert.match(rel, /\.[a-f0-9]{10}\.[a-z0-9]+$/, `Unhashed asset ${rel}`);
    assert.doesNotMatch(rel, /\.(png|ttf)$/);
  }
  for (const name of ['home', 'project']) {
    assert.ok(fs.statSync(path.join(site, `assets/screenshot-${name}-640.webp`)).size < fs.statSync(path.join(site, `assets/screenshot-${name}.png`)).size);
  }
  assert.ok(gzipSync(read('index.html')).length < 12_000, 'Landing HTML gzip budget: 12 kB');
  for (const [ext, budget] of [['js', 8_000], ['css', 10_000], ['json', 100_000]]) {
    const files = fs.readdirSync(path.join(out, 'assets')).filter((p) => p.endsWith(`.${ext}`));
    assert.ok(files.length);
    for (const file of files) assert.ok(gzipSync(read(`assets/${file}`)).length < budget, `${file} exceeds gzip budget ${budget}`);
  }
  assert.match(read('index.html'), /srcset="[^"]+640w[^"]+1280w"/);
  assert.match(read('index.html'), /loading="lazy"/);
  assert.doesNotMatch(read('docs/index.html'), /rel="preload"[^>]+as="font"/);
});

test('canonical, social, sitemap and robots URLs use the production address', () => {
  const canonical = (process.env.SITE_URL
    || (process.env.VERCEL_PROJECT_PRODUCTION_URL && `https://${process.env.VERCEL_PROJECT_PRODUCTION_URL}`)
    || 'https://clipcut-sigma.vercel.app').replace(/\/$/, '');
  for (const file of pages) {
    assert.ok(read(file).includes(`<link rel="canonical" href="${canonical}${route(file)}">`), file);
    assert.ok(read(file).includes(`content="${canonical}/og.png"`), file);
  }
  assert.ok(read('robots.txt').includes(`Sitemap: ${canonical}/sitemap.xml`));
  assert.ok(read('sitemap.xml').includes(`${canonical}/docs/roadmap`));
});

test('Vercel detects website changes across an entire push and builds when the base is unavailable', () => {
  const cwd = fs.mkdtempSync(path.join(os.tmpdir(), 'clipcut-deploy-test-'));
  const { ignoreCommand } = JSON.parse(fs.readFileSync(path.join(root, 'vercel.json'), 'utf8'));
  const git = (...args) => execFileSync('git', args, { cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
  const ignored = (base) => spawnSync('sh', ['-c', ignoreCommand], {
    cwd, env: { ...process.env, VERCEL_GIT_PREVIOUS_SHA: base }, stdio: 'pipe',
  }).status;
  const commit = (file, text) => {
    fs.mkdirSync(path.dirname(path.join(cwd, file)), { recursive: true });
    fs.writeFileSync(path.join(cwd, file), text);
    git('add', '.');
    git('commit', '-qm', 'Test fixture');
    return git('rev-parse', 'HEAD');
  };
  try {
    git('init', '-q');
    git('config', 'user.name', 'Website test');
    git('config', 'user.email', 'website-test@example.invalid');
    const initial = commit('docs/Usage.md', 'Initial docs');
    const website = commit('docs/Usage.md', 'Updated docs');
    commit('crates/app/main.rs', 'App-only change after docs change');
    assert.equal(ignored(initial), 1, 'A site change earlier in a push must build');
    assert.equal(ignored(website), 0, 'An app-only change can skip the website build');
    assert.equal(ignored(''), 1, 'First deployment builds');
    assert.equal(ignored('0000000000000000000000000000000000000000'), 1, 'Missing shallow-clone base builds');
  } finally {
    fs.rmSync(cwd, { recursive: true, force: true });
  }
});
