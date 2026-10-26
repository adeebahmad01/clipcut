#!/usr/bin/env node
// ClipCut mock API server for UI development. Zero dependencies (Node 24).
//
//   node tools/mock-server.mjs [port]
//
// Serves crates/app/web/ at / and a fake, in-memory implementation of
// docs/API.md under /api. Placeholder media is generated once with ffmpeg in
// /tmp/clipcut-mock/. One project runs a fake pipeline that emits SSE.

import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const WEB = path.resolve(HERE, '../crates/app/web');
const MEDIA = '/tmp/clipcut-mock';
const PORT = Number(process.argv[2] || process.env.PORT || 7879);
const SOURCE_MEDIA = '/tmp/clipcut-sources-smoke/fixtures';
const SOURCES_ONLY = process.env.CLIPCUT_MOCK_SOURCES_ONLY === '1';

// ---------------------------------------------------------------- media ----

function ffmpeg(out, args) {
  const file = path.join(MEDIA, out);
  if (fs.existsSync(file)) return;
  execFileSync('ffmpeg', ['-y', '-loglevel', 'error', ...args, file], { stdio: 'inherit' });
}

function makeMedia() {
  fs.mkdirSync(MEDIA, { recursive: true });
  const v = (name, src, d = 3) =>
    ffmpeg(name, ['-f', 'lavfi', '-i', src, '-t', String(d), '-pix_fmt', 'yuv420p', '-c:v', 'libx264', '-preset', 'veryfast', '-crf', '30', '-movflags', '+faststart']);
  v('clip-youtube.mp4', 'testsrc2=s=540x960:r=25');
  v('clip-gif.mp4', 'mandelbrot=s=540x960:r=25');
  v('clip-stock.mp4', 'life=s=540x960:mold=10:r=25:ratio=0.1:death_color=#202040:life_color=#c8ff3e');
  v('clip-upload.mp4', 'rgbtestsrc=s=540x960:r=25');
  const png = (name, src) => ffmpeg(name, ['-f', 'lavfi', '-i', src, '-frames:v', '1']);
  png('img-image.png', 'gradients=s=720x1280:c0=#ff3e8a:c1=#3e6bff:x0=0:y0=0:x1=720:y1=1280:seed=3');
  png('img-meme.png', 'gradients=s=720x720:c0=#ffd23e:c1=#ff6a3e:seed=7');
  png('img-article.png',
    'color=c=#f4f4f0:s=720x1280,drawbox=x=60:y=120:w=600:h=40:c=#222222:t=fill,drawbox=x=60:y=200:w=520:h=14:c=#999999:t=fill,' +
    'drawbox=x=60:y=230:w=560:h=14:c=#999999:t=fill,drawbox=x=50:y=280:w=620:h=60:c=#fff3a0:t=fill,drawbox=x=60:y=300:w=540:h=14:c=#555555:t=fill,' +
    'drawbox=x=60:y=380:w=600:h=320:c=#99aabb:t=fill,drawbox=x=60:y=740:w=560:h=14:c=#999999:t=fill,drawbox=x=60:y=770:w=480:h=14:c=#999999:t=fill');
  for (let i = 1; i <= 6; i++) {
    png(`cand-${i}.png`, `gradients=s=320x320:seed=${i * 11}:speed=0.01`);
  }
  ffmpeg('final.mp4', ['-f', 'lavfi', '-i', 'testsrc2=s=540x960:r=25', '-f', 'lavfi', '-i', 'sine=f=220:d=20',
    '-t', '20', '-pix_fmt', 'yuv420p', '-c:v', 'libx264', '-preset', 'veryfast', '-crf', '32', '-c:a', 'aac', '-shortest', '-movflags', '+faststart']);
  ffmpeg('final-h.mp4', ['-f', 'lavfi', '-i', 'testsrc2=s=960x540:r=25', '-f', 'lavfi', '-i', 'sine=f=260:d=12',
    '-t', '12', '-pix_fmt', 'yuv420p', '-c:v', 'libx264', '-preset', 'veryfast', '-crf', '32', '-c:a', 'aac', '-shortest', '-movflags', '+faststart']);
  ffmpeg('thumb.png', ['-ss', '1', '-i', path.join(MEDIA, 'final.mp4'), '-frames:v', '1']);
  ffmpeg('thumb-h.png', ['-ss', '1', '-i', path.join(MEDIA, 'final-h.mp4'), '-frames:v', '1']);
  ffmpeg('voice.wav', ['-f', 'lavfi', '-i', 'sine=f=180:d=20:sample_rate=24000', '-af', 'volume=0.2']);
  ffmpeg('preview.wav', ['-f', 'lavfi', '-i', 'sine=f=330:d=1.2:sample_rate=24000', '-af', 'volume=0.3']);
}

// ------------------------------------------ isolated native-source fixtures -
// CLIPCUT_SOURCES_MOCK_BASE=http://<numeric-private-IP>:<port>
// rewrites provider HTTP calls to /sources-mock/<service><original-path>.
// This router never contacts a provider or reads any ClipCut home/credentials.
// YouTube is a subprocess: use crates/sources/fixtures/mock-yt-dlp.mjs instead.
// Keep /api and /mock (the existing UI demo) unchanged.
function makeSourceMedia() {
  fs.mkdirSync(SOURCE_MEDIA, { recursive: true });
  const make = (name, args) => {
    const file = path.join(SOURCE_MEDIA, name);
    if (!fs.existsSync(file)) execFileSync('ffmpeg', [
      '-nostdin', '-y', '-loglevel', 'error', ...args, '-threads', '2', file,
    ], { stdio: 'inherit', timeout: 20_000 });
  };
  make('video.mp4', ['-f', 'lavfi', '-i', 'testsrc2=s=320x480:r=30', '-t', '3',
    '-an', '-c:v', 'libx264', '-preset', 'veryfast', '-crf', '30', '-pix_fmt', 'yuv420p', '-movflags', '+faststart']);
  make('image.png', ['-f', 'lavfi', '-i', 'gradients=s=640x960:seed=3', '-frames:v', '1']);
  make('meme.png', ['-f', 'lavfi', '-i', 'gradients=s=640x640:seed=7', '-frames:v', '1']);
  make('audio.wav', ['-f', 'lavfi', '-i', 'sine=f=220:d=1:sample_rate=16000']);
}

const sourceStats = { total: 0, routes: {}, keyed: {} };
const htmlEscape = (s) => String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);
const sourceHtml = (res, html, status = 200) => {
  res.writeHead(status, { 'content-type': 'text/html; charset=utf-8', 'cache-control': 'no-store' });
  res.end(html);
};
const sourceSvg = '<svg xmlns="http://www.w3.org/2000/svg" width="1em" height="1em" viewBox="0 0 32 32"><path fill="#f90" d="M16 2 30 28H2Z"/><circle fill="#ffe" cx="16" cy="20" r="4"/></svg>';

async function sourceMock(req, res, url) {
  const p = url.pathname.slice('/sources-mock'.length);
  if (p === '/stats') {
    if (req.method === 'DELETE') {
      sourceStats.total = 0; sourceStats.routes = {}; sourceStats.keyed = {};
    }
    return json(res, 200, { ...sourceStats, schema_version: 1, mocked: true });
  }
  const service = p.split('/')[1];
  sourceStats.total++;
  sourceStats.routes[p] = (sourceStats.routes[p] || 0) + 1;
  // Do not echo request headers, key values, cookies or complete request URLs.
  const base = `http://${LAN}:${PORT}`;
  const fixture = (f) => `${base}/sources-mock/media/${f}`;
  const article = `${base}/sources-mock/article/coffee`;
  let q = url.searchParams.get('q') || url.searchParams.get('query') || url.searchParams.get('gsrsearch') || '';
  if (req.method === 'POST') {
    let bytes = 0; const chunks = [];
    for await (const chunk of req) {
      bytes += chunk.length;
      if (bytes > 8192) return err(res, 413, 'source fixture request too large');
      chunks.push(chunk);
    }
    const text = Buffer.concat(chunks).toString();
    q = req.headers['content-type']?.includes('application/json')
      ? JSON.parse(text || '{}').query || '' : new URLSearchParams(text).get('q') || '';
  }
  if (service === 'tenor-web') q = p;
  const has = (scenario) => q.toLowerCase().includes(`mock-${scenario}`);
  const keyed = ['brave', 'tavily', 'giphy', 'tenor', 'pexels', 'pixabay', 'unsplash'].includes(service);
  if (keyed) {
    sourceStats.keyed[service] = (sourceStats.keyed[service] || 0) + 1;
    const token = url.searchParams.get('api_key') || url.searchParams.get('key')
      || req.headers['x-subscription-token']
      || String(req.headers.authorization || '').replace(/^(Bearer|Client-ID)\s+/i, '');
    if (!String(token).startsWith('clipcut-mock-')) return err(res, 401, 'only clipcut-mock- fixture tokens are accepted');
  }
  if (has('fail') || (has('ddg') && service === 'searxng')
    || ((has('keyed') || has('tavily')) && ['searxng', 'duckduckgo'].includes(service))
    || (has('tavily') && service === 'brave')
    || (has('partial') && ['wikimedia', 'openverse'].includes(service))) {
    return err(res, 503, 'deterministic source fixture failure');
  }
  if (has('malformed')) {
    res.writeHead(200, { 'content-type': 'application/json' }); return res.end('{bad json');
  }
  if (has('slow')) {
    const timer = setTimeout(() => json(res, 200, { results: [] }), 15_000);
    res.on('close', () => clearTimeout(timer)); return;
  }
  const empty = has('empty') || (has('stock-keyed') && service === 'wikimedia');
  const image = fixture('image.png'), video = fixture('video.mp4');
  const hit = (i = 0) => ({
    title: `Synthetic coffee cherry fixture ${i + 1}`, url: `${article}?item=${i}`,
    content: 'The coffee cherry is a drupe. Deterministic synthetic test evidence, not live research.',
    img_src: `${has('gif') ? video : image}?item=${i}`, thumbnail_src: image,
    resolution: '640x960', publishedDate: '2026-01-01',
  });
  if (p === '/searxng/search') return json(res, 200, { results: empty ? [] : [hit(0), hit(1), { ...hit(2), url: 'javascript:alert(1)' }] });
  if (service === 'duckduckgo') {
    if (p.endsWith('/html/')) return sourceHtml(res, empty ? '<html></html>' :
      [0, 1].map((i) => `<div class="result"><a class="result__a" href="${htmlEscape(hit(i).url)}">${hit(i).title}</a><div class="result__snippet">${hit(i).content}</div></div>`).join(''));
    if (p.endsWith('/i.js')) return json(res, 200, { results: empty ? [] : [0, 1].map((i) => ({ title: hit(i).title,
      url: hit(i).url, image: hit(i).img_src, thumbnail: image, width: 640, height: 960 })) });
    if (p.endsWith('/news.js')) return json(res, 200, { results: empty ? [] : [{ ...hit(), excerpt: hit().content, image, date: 1767225600 }] });
    if (p.endsWith('/v.js')) return json(res, 200, { results: empty ? [] : [{ title: hit().title, content: article, description: hit().content, images: { medium: image } }] });
    if (p === '/duckduckgo/') return sourceHtml(res, '<script>var vqd="1234-5678";</script>');
  }
  if (p === '/wikimedia/w/api.php') {
    const isVideo = url.searchParams.get('prop') === 'videoinfo';
    const meta = (license, licenseUrl) => ({
      Artist: { value: '<a href="https://example.com/fixture-creator">Fixture Creator</a>' },
      LicenseShortName: { value: license }, LicenseUrl: { value: licenseUrl },
      Attribution: { value: 'Synthetic fixture — no real Commons content' }, AttributionRequired: { value: 'true' },
    });
    const page = (i, license, licenseUrl) => ({
      index: i, title: `File:Coffee fixture ${i}`,
      [isVideo ? 'videoinfo' : 'imageinfo']: [{
        url: isVideo ? video : image, descriptionurl: `${article}?commons=${i}`,
        thumburl: image, width: isVideo ? 320 : 640, height: isVideo ? 480 : 960,
        thumbwidth: 640, thumbheight: 960, duration: 3, size: 300000,
        mime: isVideo ? 'video/mp4' : 'image/png', extmetadata: meta(license, licenseUrl),
        derivatives: [{ type: 'video/mp4', src: video, width: 320, height: 480 }],
      }],
    });
    return json(res, 200, { query: { pages: empty ? [] : [
      page(0, 'CC BY 4.0', 'https://creativecommons.org/licenses/by/4.0/'),
      page(1, 'CC BY-NC 4.0', 'https://creativecommons.org/licenses/by-nc/4.0/'),
      page(2, 'Unknown', ''), page(3, 'CC BY 4.0', 'https://evil.invalid/licenses/by/4.0/'),
    ] } });
  }
  if (p === '/openverse/v1/images/') return json(res, 200, { results: empty ? [] : ['by', 'by-nc', 'by-nd', 'unknown'].map((license, i) => ({
    title: `Openverse synthetic fixture ${i}`, url: `${image}?openverse=${i}`, thumbnail: image,
    foreign_landing_url: `${article}?openverse=${i}`, width: 640, height: 960,
    creator: 'Fixture Creator', creator_url: 'https://example.com/fixture-creator', license,
    license_version: '4.0', license_url: `https://creativecommons.org/licenses/${license}/4.0/`,
    attribution: 'Fixture Creator — synthetic, not live evidence', source: 'source-mock', filetype: 'png',
  })) });
  if (p === '/imgflip/get_memes') return json(res, 200, { success: true, data: { memes: empty ? [] : [
    { id: '1', name: 'Expanding Brain', url: fixture('meme.png'), width: 640, height: 640, box_count: 4 },
    { id: '2', name: 'Surprised Coffee', url: `${fixture('meme.png')}?template=2`, width: 640, height: 640, box_count: 2 },
  ] } });
  if (service === 'tenor-web') return sourceHtml(res, empty ? '<html></html>' :
    `<a href="${base}/sources-mock/article/gif/view/fixture"><img src="${video}" alt="Synthetic coffee GIF"/></a>`);
  if (p === '/iconify/search') return json(res, 200, { icons: empty || /zzzz|nonsense/.test(q) ? [] : ['../unsafe', 'mdi:fire', 'fluent-emoji-flat:fire'] });
  if (p === '/iconify/collection') return json(res, 200, { info: {
    name: 'Synthetic fixture icons', author: { name: 'ClipCut fixture' },
    license: { title: 'MIT (fixture only)', spdx: 'MIT', url: 'https://opensource.org/license/mit/' },
  } });
  if (service === 'iconify' && p.endsWith('.svg')) {
    if (p.includes('missing')) return err(res, 404, 'no such fixture icon');
    return sendBuf(req, res, Buffer.from(sourceSvg), 'image/svg+xml');
  }
  if (service === 'brave') {
    const results = empty ? [] : [{ ...hit(), description: hit().content, properties: { url: image }, thumbnail: { src: image } }];
    return json(res, 200, p.includes('/web/') ? { web: { results } } : { results });
  }
  if (service === 'tavily') return json(res, 200, { results: empty ? [] : [hit()], images: empty ? [] : [image] });
  if (service === 'giphy') return json(res, 200, { data: empty ? [] : [{
    id: 'fixture', title: 'Synthetic GIF', url: `${article}?giphy=1`,
    images: { downsized_small: { mp4: video, width: '320', height: '480' }, fixed_height_still: { url: image } },
  }] });
  if (service === 'tenor') return json(res, 200, { results: empty ? [] : [{
    id: 'fixture', content_description: 'Synthetic GIF', itemurl: `${article}?tenor=1`,
    media_formats: { mp4: { url: video, dims: [320, 480], duration: 3 }, tinygif: { url: image } },
  }] });
  if (service === 'pexels') return json(res, 200, p.includes('/videos/') ? { videos: empty ? [] : [{
    url: `${article}?pexels=video`, duration: 3, image, user: { name: 'Fixture Creator' },
    video_files: [{ file_type: 'video/mp4', width: 320, height: 480, link: video }],
  }] } : { photos: empty ? [] : [{
    url: `${article}?pexels=photo`, alt: 'Fixture photo', photographer: 'Fixture Creator', width: 640, height: 960,
    src: { large2x: image, original: image, medium: image },
  }] });
  if (service === 'pixabay') return json(res, 200, { hits: empty ? [] : [{
    tags: 'Synthetic coffee', pageURL: `${article}?pixabay=1`, user: 'Fixture Creator', duration: 3,
    videos: { small: { url: video, width: 320, height: 480 } },
  }] });
  if (p === '/unsplash/search/photos') return json(res, 200, { results: empty ? [] : [{
    alt_description: 'Fixture photo', width: 640, height: 960, urls: { regular: image, small: image },
    user: { name: 'Fixture Creator' }, links: { html: `${article}?unsplash=1`, download_location: 'https://api.unsplash.com/photos/fixture/download' },
  }] });
  if (p === '/unsplash/photos/fixture/download') return json(res, 200, { url: image });
  if (service === 'media') {
    const name = p.split('/').pop();
    if (['image.png', 'meme.png', 'video.mp4', 'audio.wav'].includes(name)) return sendFile(req, res, path.join(SOURCE_MEDIA, name));
    if (name === 'html.png' || name === 'html.mp4') return sourceHtml(res, '<html><body>This is HTML, not media.</body></html>');
    if (name === 'corrupt.png' || name === 'corrupt.mp4') return sendBuf(req, res, Buffer.from('not media'), 'application/octet-stream');
    if (name === 'empty.mp4') return sendBuf(req, res, Buffer.alloc(0), 'video/mp4');
    if (name === 'unsafe.svg') return sendBuf(req, res, Buffer.from('<svg width="32" height="32"><script>alert(1)</script></svg>'), 'image/svg+xml');
  }
  if (service === 'article') {
    if (p === '/article/redirect') { res.writeHead(302, { location: article }); return res.end(); }
    if (p === '/article/error') return sourceHtml(res, '<title>Fixture error</title><p>Not available</p>', 503);
    if (p === '/article/blocked') return sourceHtml(res, '<title>Verify you are human</title><p>Access denied.</p>');
    return sourceHtml(res, `<!doctype html><html><head><title>Synthetic coffee cherry evidence</title>
      <meta property="og:site_name" content="ClipCut deterministic fixtures"/>
      <style>body{font:20px/1.65 sans-serif;max-width:470px;margin:32px auto;color:#18202c}h1{line-height:1.2}.note{color:#667}</style>
      </head><body><h1>Coffee cherries: fixture article</h1><p class="note">Synthetic local test page. This is not live provider evidence.</p>
      <p>The coffee cherry is a drupe. Its seeds are often called coffee beans. This specific sentence is used to verify highlighted browser screenshots.</p>
      ${Array.from({ length: 12 }, (_, i) => `<p>Fixture paragraph ${i + 1}: this local article contains enough text to exercise layout, scrolling to a quotation, and a real viewport screenshot. No paywall, authentication, or consent is bypassed.</p>`).join('')}
      </body></html>`);
  }
  return err(res, 404, 'unknown deterministic source fixture route');
}

// ---------------------------------------------------------------- data -----

const now = () => new Date().toISOString();
const ago = (min) => new Date(Date.now() - min * 60_000).toISOString();
const STAGES = ['research', 'hooks', 'script', 'beats', 'assets', 'voice', 'render'];
const PAUSE_AFTER = ['hooks', 'script', 'assets'];
const KINDS = ['youtube', 'article', 'meme', 'gif', 'image', 'stock', 'text'];

const freshStages = () => STAGES.map((stage) => ({
  stage, status: 'pending', message: '', progress: 0, error: null, started_at: null, finished_at: null,
}));
const doneStages = (upto = 'render') => freshStages().map((s) =>
  STAGES.indexOf(s.stage) <= STAGES.indexOf(upto)
    ? { ...s, status: 'done', progress: 1, message: 'Done', started_at: ago(30), finished_at: ago(29) }
    : s);

function research(topic) {
  return {
    summary: `${topic} has a stranger history than most people think. Early accounts disagree on the details, `
      + 'but primary sources agree on the core facts. The story spread widely after a 2010s viral thread, '
      + 'and experts still argue about what it means today.',
    facts: [
      { text: 'Coffee "beans" are the seeds of a cherry-like fruit called the coffee cherry.', source_url: 'https://en.wikipedia.org/wiki/Coffee_bean', source_title: 'Wikipedia — Coffee bean' },
      { text: 'Botanically the coffee cherry is a drupe, like peaches and plums.', source_url: 'https://www.britannica.com/plant/coffee-plant', source_title: 'Britannica' },
      { text: 'Some cafés now sell cascara, a tea made from dried coffee cherry husks.', source_url: 'https://www.ncausa.org/', source_title: 'National Coffee Association' },
      { text: 'Each cherry usually holds two seeds; about 5% hold one, called a peaberry.', source_url: null, source_title: null },
    ],
    sources: [
      { title: 'Coffee bean — Wikipedia', url: 'https://en.wikipedia.org/wiki/Coffee_bean', snippet: 'A coffee bean is a seed of the Coffea plant…' },
      { title: 'Coffee plant | Britannica', url: 'https://www.britannica.com/plant/coffee-plant', snippet: 'Coffee plant, any of various shrubs…' },
    ],
    angles: ['Your morning drink is fruit juice, technically', 'Peaberries are rare mutants'],
    keywords: ['coffee cherry', 'peaberry', 'cascara', 'drupe'],
  };
}

function hooks() {
  return [
    { text: 'Your morning coffee is technically a fruit smoothie.', angle: 'contrarian', score: 8.7, why: 'Flips a daily habit into a surprise; instantly relatable.' },
    { text: 'Nobody told you coffee beans aren\'t beans.', angle: 'myth-busting', score: 8.1, why: 'Implies hidden knowledge; strong curiosity gap.' },
    { text: 'There\'s a fruit you eat every day without knowing it.', angle: 'mystery', score: 7.4, why: 'Open loop, but slightly vague for the first second.' },
    { text: 'Baristas hate this one botanical fact.', angle: 'meme', score: 6.2, why: 'Playful, but the clickbait pattern is worn out.' },
  ];
}

function script(topic) {
  const text = 'Your morning coffee is technically a fruit smoothie. Coffee beans are not beans at all. '
    + 'They are the seeds of a bright red cherry. Botanists call it a drupe, the same family as peaches. '
    + 'Some cafés even brew the dried skins into a tea called cascara. And one in twenty cherries hides a single round seed. '
    + 'Roasters call it a peaberry and charge extra for it. So next time someone says they skipped fruit today, hand them a latte.';
  return {
    text,
    sentences: text.match(/[^.!?]+[.!?]+/g).map((s) => s.trim()),
    draft: 'Did you know that your morning cup of coffee is actually made from a fruit? It\'s true! In today\'s fast-paced world, '
      + 'many people don\'t realize that coffee beans are not really beans at all. Let\'s dive into the fascinating world of coffee cherries…',
    title: topic.length < 60 ? `${topic}` : topic,
    description: 'Coffee beans aren\'t beans. They\'re seeds from a cherry — and some cafés brew the skins too.',
    hashtags: ['#coffee', '#funfacts', '#science', '#shorts'],
  };
}

const BEAT_PLAN = [
  ['Your morning coffee', 'youtube', 'barista pouring latte slow motion', 'MORNING COFFEE'],
  ['is technically a fruit smoothie.', 'meme', 'surprised pikachu', 'A FRUIT??'],
  ['Coffee beans are not beans at all.', 'text', 'not beans', 'NOT BEANS'],
  ['They are the seeds', 'image', 'coffee cherry seeds macro', null],
  ['of a bright red cherry.', 'stock', 'red coffee cherries on branch', 'RED CHERRY'],
  ['Botanists call it a drupe,', 'article', 'coffee cherry drupe botany', 'DRUPE'],
  ['the same family as peaches.', 'gif', 'peach emoji dance', null],
  ['Some cafés even brew the dried skins', 'youtube', 'cascara tea brewing', 'CASCARA'],
  ['into a tea called cascara.', 'image', 'cascara tea cup', null],
  ['And one in twenty cherries hides a single round seed.', 'stock', 'coffee sorting hands', '1 IN 20'],
  ['Roasters call it a peaberry and charge extra for it.', 'gif', 'money rain', '$$$'],
  ['So next time someone says they skipped fruit today, hand them a latte.', 'text', 'latte', 'HAND THEM A LATTE'],
];

const PROVIDER = { youtube: 'youtube', article: 'article', meme: 'imgflip', gif: 'giphy', image: 'wikimedia', stock: 'pexels', text: 'text' };

function assetFor(kind, id, provider = PROVIDER[kind]) {
  const video = ['youtube', 'gif', 'stock'].includes(kind);
  const ext = video ? 'mp4' : kind === 'text' ? 'svg' : 'png';
  return {
    kind: video ? 'video' : kind === 'text' ? 'svg' : 'image',
    path: `assets/${id}.${ext}`,
    provider,
    source_url: kind === 'text' ? '' : `https://example.com/${provider}/${id}`,
    credit: kind === 'text' ? '' : `${provider}: "${id}" via example.com`,
    width: 540, height: 960, duration_s: video ? 3 : null, trim_start_s: 0, focus: null,
  };
}

function candidates(kind, query) {
  return Array.from({ length: kind === 'text' ? 1 : 6 }, (_, i) => ({
    kind,
    provider: PROVIDER[kind],
    title: `${query || kind} — result ${i + 1}`,
    page_url: `https://example.com/${PROVIDER[kind]}/${encodeURIComponent(query || kind)}/${i}`,
    media_url: null,
    thumbnail_url: `/mock/cand-${(i % 6) + 1}.png`,
    duration_s: ['youtube', 'gif', 'stock'].includes(kind) ? 2 + (i % 3) * 0.5 : null,
    width: 540, height: 960,
    start_s: kind === 'youtube' ? 12.5 + i * 7 : null,
    extra: {},
  }));
}

function beats({ withAssets = true } = {}) {
  let t = 0;
  return BEAT_PLAN.map(([text, kind, query, ost], i) => {
    const id = `b${String(i + 1).padStart(2, '0')}`;
    const dur = Math.max(1.1, Math.min(3, text.split(' ').length / 2.6));
    const fallback = withAssets && i === 9;
    const failed = withAssets && i === 6 && false;
    const b = {
      id, text,
      visual: {
        kind, query, alt_queries: [], on_screen_text: ost, icon: kind === 'text' ? 'mdi:coffee' : null,
        meme: kind === 'meme' ? { template: 'Surprised Pikachu', caption: 'me learning coffee is fruit', top_text: '', bottom_text: '' } : null,
        quote: kind === 'article' ? 'the coffee cherry is a drupe' : null, sfx: null,
      },
      candidates: withAssets ? candidates(fallback ? 'image' : kind, query) : [],
      chosen: withAssets ? 0 : null,
      asset: withAssets ? assetFor(fallback ? 'image' : kind, id) : null,
      status: withAssets ? (fallback ? 'fallback' : failed ? 'failed' : 'ready') : 'pending',
      error: fallback ? 'pexels: no results for "coffee sorting hands"; used image' : null,
      locked: withAssets && i === 3,
      start_s: withAssets ? t : 0,
      end_s: withAssets ? t + dur : 0,
    };
    t += dur;
    return b;
  });
}

function voiceTrack(bs) {
  const words = [];
  for (const b of bs) {
    const ws = b.text.split(' ');
    const step = (b.end_s - b.start_s) / ws.length;
    ws.forEach((w, i) => words.push({ text: w, start_s: b.start_s + i * step, end_s: b.start_s + (i + 0.85) * step }));
  }
  const dur = bs.at(-1)?.end_s || 20;
  return { path: 'voice/voice.wav', duration_s: dur, sample_rate: 24000, engine: 'kokoro', voice: 'am_michael', words, sentences: [] };
}

function renderOut(p) {
  const v = p.format === 'vertical';
  return { path: 'render/final.mp4', credits_path: 'render/credits.txt', duration_s: p.voice_track?.duration_s || 20,
    width: v ? 1080 : 1920, height: v ? 1920 : 1080, rendered_at: now() };
}

function baseProject(id, topic, extra = {}) {
  return {
    id, created_at: ago(120), updated_at: ago(60), topic, notes: '', format: 'vertical', target_seconds: 45,
    voice: null, autopilot: true, stages: freshStages(), research: null, hooks: [], chosen_hook: null,
    script: null, beats: [], voice_track: null, render: null, ...extra,
  };
}

function seed() {
  const projects = new Map();
  const done = baseProject('20261008-091200-a1b2c3', 'Why coffee is technically a fruit', {
    created_at: ago(300), updated_at: ago(240), notes: 'Keep it playful. Mention cascara.',
    stages: doneStages(), research: research('Coffee'), hooks: hooks(), chosen_hook: 0, script: script('Your coffee is a fruit'),
  });
  done.beats = beats();
  done.voice_track = voiceTrack(done.beats);
  done.render = renderOut(done);
  projects.set(done.id, done);

  const paused = baseProject('20261008-101500-d4e5f6', 'The 1977 Wow! signal from deep space', {
    created_at: ago(90), updated_at: ago(80), autopilot: false, stages: doneStages('hooks'),
    research: research('The Wow! signal'),
    hooks: [
      { text: 'In 1977 a telescope heard something for exactly 72 seconds.', angle: 'mystery', score: 9.1, why: 'Specific number + mystery = strong stop.' },
      { text: 'An astronomer circled six characters and wrote "Wow!"', angle: 'story', score: 8.4, why: 'Human moment, visual hook.' },
      { text: 'We never heard it again. Ever.', angle: 'cliffhanger', score: 7.0, why: 'Punchy, but lacks context in the first second.' },
    ],
  });
  projects.set(paused.id, paused);

  const failed = baseProject('20261007-183000-0f9e8d', 'Quantum computers explained in 45 seconds', {
    created_at: ago(1500), updated_at: ago(1490), format: 'horizontal', stages: doneStages('assets'),
    research: research('Quantum computing'), hooks: hooks(), chosen_hook: 1, script: script('Quantum computers, explained'),
  });
  failed.beats = beats();
  const vs = failed.stages.find((s) => s.stage === 'voice');
  Object.assign(vs, { status: 'failed', message: 'Kokoro failed', error: 'kokoro: model file kokoro-v1.0.onnx not found (run setup → kokoro)', started_at: ago(1490) });
  projects.set(failed.id, failed);

  const running = baseProject('20261008-114500-777aaa', 'How Australia lost the Great Emu War', { created_at: ago(1), updated_at: ago(1) });
  projects.set(running.id, running);
  return { projects, runningId: running.id };
}

// ---------------------------------------------------------------- state ----

const { projects, runningId } = seed();
const tasks = new Map(); // project id -> { cancelled }
const uploads = new Map(); // `${id}/${path}` -> { buf, type }
const clients = new Set();
let config = {
  server: { host: '0.0.0.0', port: 7878, open_window: true },
  llm: { provider: 'claude', model: 'opus', fast_model: 'sonnet', timeout_seconds: 600, unslop_preset: 'crisp', skip_unslop: false },
  video: { format: 'vertical', fps: 30, target_seconds: 45, min_clip_seconds: 1, max_clip_seconds: 3, captions: true, crf: 20 },
  search: { searxng_url: 'http://127.0.0.1:8888', manage_searxng: true, duckduckgo_fallback: true },
  youtube: { max_results: 8, max_video_minutes: 40, cookies_from_browser: null, channel_blocklist: [] },
  tts: { engine: 'kokoro', voice: 'am_michael', speed: 1.1, command: '' },
  audio: { voice_volume: 1, sfx_volume: 0.55, music_volume: 0.14, duck_music: true, use_music: true },
  publish: { privacy: 'private', category_id: '22', made_for_kids: false },
  tools: {},
};
const SERVICES = [
  ['giphy', 'GIPHY — GIF search (developers.giphy.com)'],
  ['tenor', 'Tenor — GIF search fallback (Google Cloud API key)'],
  ['pexels', 'Pexels — stock video/photos (pexels.com/api)'],
  ['pixabay', 'Pixabay — stock video fallback (pixabay.com/api/docs)'],
  ['unsplash', 'Unsplash — photos (optional)'],
  ['freesound', 'Freesound — extra SFX (optional)'],
  ['tavily', 'Tavily — web search fallback (optional)'],
  ['brave', 'Brave Search — web/image search fallback (optional)'],
];
const keys = { giphy: ['dc6zaTOxFJmzC1234', 'abcdabcdabcd9f2e'], pexels: ['563492ad6f9170000100000177aa'] };
const keyState = { 'giphy/1': 'cooling_down' };
const checks = [
  { name: 'ffmpeg', ok: true, detail: '7.1.1', fix: null },
  { name: 'ffprobe', ok: true, detail: '7.1.1', fix: null },
  { name: 'yt-dlp', ok: false, detail: 'not found on PATH', fix: 'Install with setup target yt-dlp (downloads the standalone binary)' },
  { name: 'chromium', ok: true, detail: '/usr/bin/chromium-browser', fix: null },
  { name: 'tts: kokoro', ok: false, detail: 'model files missing', fix: 'Run setup target kokoro (≈330 MB download)' },
  { name: 'llm: claude', ok: true, detail: 'Claude Code 2.4.1', fix: null },
  { name: 'search: searxng', ok: false, detail: 'http://127.0.0.1:8888 unreachable (DuckDuckGo fallback active)', fix: 'Run setup target searxng (needs podman)' },
  { name: 'keys', ok: true, detail: '2 of 8 services have keys', fix: null },
];
const VOICES = [
  ['am_michael', 'Michael (US, male)'], ['af_heart', 'Heart (US, female)'], ['af_bella', 'Bella (US, female)'],
  ['am_adam', 'Adam (US, male)'], ['bm_george', 'George (UK, male)'], ['bf_emma', 'Emma (UK, female)'],
].map(([id, label]) => ({ id, label, engine: 'kokoro' })).concat([{ id: 'en-us', label: 'eSpeak (robotic fallback)', engine: 'espeak' }]);

// ---------------------------------------------------------------- events ---

function send(event, data) {
  const line = `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`;
  for (const res of clients) res.write(line);
}
function progress(project_id, stage, message, { beat_id = null, progress = null, level = 'info' } = {}) {
  send('progress', { project_id, stage, beat_id, progress, message, level });
}
function touch(p) {
  p.updated_at = now();
  send('project', { id: p.id, updated_at: p.updated_at, running: tasks.has(p.id) });
}
setInterval(() => send('ping', {}), 15_000);

// ---------------------------------------------------------------- pipeline -

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const stageOf = (p, s) => p.stages.find((x) => x.stage === s);

function resetFrom(p, stage) {
  const i = STAGES.indexOf(stage);
  for (const s of p.stages) {
    if (STAGES.indexOf(s.stage) >= i) Object.assign(s, { status: 'pending', message: '', progress: 0, error: null, started_at: null, finished_at: null });
  }
  if (i <= 0) p.research = null;
  if (i <= 1) { p.hooks = []; p.chosen_hook = null; }
  if (i <= 2) p.script = null;
  if (i <= 3) p.beats = [];
  if (i <= 4) for (const b of p.beats) if (!b.locked) Object.assign(b, { asset: null, candidates: [], chosen: null, status: 'pending', error: null });
  if (i <= 5) p.voice_track = null;
  p.render = null;
}

const MESSAGES = {
  research: ['Searching the web (SearXNG → DuckDuckGo)', 'Reading 6 sources', 'Extracting facts', 'Writing the briefing'],
  hooks: ['Brainstorming 12 hooks', 'Scoring scroll-stopping power', 'Keeping the best 4'],
  script: ['Drafting the script', 'Unslop pass (crisp)', 'Splitting into sentences'],
  beats: ['Planning 1–3 s beats', 'Choosing a visual kind per beat', 'Writing search queries'],
  voice: ['Synthesizing with Kokoro (am_michael)', 'Aligning word timings', 'Normalizing loudness'],
  render: ['Building the timeline', 'Encoding frames', 'Mixing voice, SFX and music', 'Burning captions', 'Writing credits.txt'],
};

async function runStage(p, stage, task) {
  const st = stageOf(p, stage);
  Object.assign(st, { status: 'running', progress: 0, message: 'Starting…', started_at: now(), error: null });
  touch(p);
  if (stage === 'assets') {
    if (!p.beats.length) p.beats = beats({ withAssets: false });
    const full = beats();
    for (let i = 0; i < p.beats.length; i++) {
      if (task.cancelled) return false;
      const b = p.beats[i];
      if (b.locked && b.asset) continue;
      b.status = 'searching';
      touch(p);
      progress(p.id, stage, `${b.id}: searching ${b.visual.kind} for "${b.visual.query}"`, { beat_id: b.id });
      await sleep(700);
      const f = full[i];
      Object.assign(b, { candidates: f.candidates, chosen: 0, asset: f.asset, status: f.status, error: f.error });
      if (f.status === 'fallback') progress(p.id, stage, `${b.id}: ${f.error}`, { beat_id: b.id, level: 'warn' });
      st.progress = (i + 1) / p.beats.length;
      st.message = `${i + 1}/${p.beats.length} visuals ready`;
      progress(p.id, stage, st.message, { progress: st.progress });
      touch(p);
    }
  } else {
    const msgs = MESSAGES[stage];
    for (let i = 0; i < msgs.length; i++) {
      if (task.cancelled) return false;
      st.progress = i / msgs.length;
      st.message = msgs[i];
      progress(p.id, stage, msgs[i], { progress: st.progress });
      if (i === 1 && stage === 'research') progress(p.id, stage, 'SearXNG unreachable, using DuckDuckGo', { level: 'warn' });
      await sleep(stage === 'render' ? 1100 : 900);
    }
    if (task.cancelled) return false;
    if (stage === 'research') p.research = research(p.topic);
    if (stage === 'hooks') p.hooks = hooks();
    if (stage === 'script') {
      if (p.chosen_hook == null) p.chosen_hook = 0;
      p.script = script(p.topic);
    }
    if (stage === 'beats') p.beats = beats({ withAssets: false });
    if (stage === 'voice') {
      const full = beats();
      p.beats.forEach((b, i) => { b.start_s = full[i]?.start_s ?? 0; b.end_s = full[i]?.end_s ?? 0; });
      p.voice_track = voiceTrack(p.beats);
    }
    if (stage === 'render') p.render = renderOut(p);
  }
  Object.assign(st, { status: 'done', progress: 1, message: 'Done', finished_at: now() });
  progress(p.id, stage, `${stage} done`, { progress: 1 });
  touch(p);
  return true;
}

async function run(p, from, only) {
  if (tasks.has(p.id)) return;
  let start = from ?? STAGES.find((s) => stageOf(p, s).status !== 'done');
  if (!start) return;
  resetFrom(p, start);
  const task = { cancelled: false };
  tasks.set(p.id, task);
  touch(p);
  try {
    for (let i = STAGES.indexOf(start); i < STAGES.length; i++) {
      const stage = STAGES[i];
      if (!(await runStage(p, stage, task))) {
        const st = stageOf(p, stage);
        Object.assign(st, { status: 'failed', error: 'Cancelled', message: 'Cancelled' });
        progress(p.id, stage, 'Cancelled by user', { level: 'warn' });
        break;
      }
      if (only) break;
      if (!p.autopilot && PAUSE_AFTER.includes(stage)) {
        progress(p.id, stage, `Paused after ${stage} for review`);
        break;
      }
    }
  } finally {
    tasks.delete(p.id);
    touch(p);
  }
}

// ---------------------------------------------------------------- http -----

const summary = (p) => ({
  id: p.id, topic: p.topic, created_at: p.created_at, updated_at: p.updated_at, format: p.format,
  running: tasks.has(p.id), stages: p.stages, title: p.script?.title || null,
  duration_s: p.render?.duration_s ?? null,
  video_url: p.render ? `/api/projects/${p.id}/files/render/final.mp4` : null,
  thumb_url: p.render ? `/api/projects/${p.id}/still?t=1` : null,
});
const keysView = () => ({
  services: SERVICES.map(([id, label]) => ({
    id, label,
    slots: (keys[id] || []).map((k, slot) => ({ slot, hint: k.slice(-4), state: keyState[`${id}/${slot}`] || 'ready' })),
  })),
});
const statusView = () => ({
  version: '0.1.0-mock', home: '/home/me/.local/share/clipcut', network_url: `http://${lanIp()}:${PORT}`,
  tools: { ffmpeg: '/usr/bin/ffmpeg', ffprobe: '/usr/bin/ffprobe', yt_dlp: checks[2].ok ? '/home/me/.local/share/clipcut/bin/yt-dlp' : null, chromium: '/usr/bin/chromium-browser' },
  checks, ready: checks.every((c) => c.ok),
});

const TYPES = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.css': 'text/css; charset=utf-8',
  '.svg': 'image/svg+xml', '.png': 'image/png', '.mp4': 'video/mp4', '.wav': 'audio/wav', '.txt': 'text/plain; charset=utf-8',
  '.json': 'application/json', '.ico': 'image/x-icon', '.webm': 'video/webm', '.jpg': 'image/jpeg', '.gif': 'image/gif' };

function json(res, status, body) {
  res.writeHead(status, { 'content-type': 'application/json' });
  res.end(body === undefined ? '' : JSON.stringify(body));
}
const err = (res, status, message) => json(res, status, { error: message });

function sendBuf(req, res, buf, type, extra = {}) {
  const total = buf.length;
  const range = req.headers.range && /bytes=(\d*)-(\d*)/.exec(req.headers.range);
  const head = { 'content-type': type, 'accept-ranges': 'bytes', 'cache-control': 'no-cache', ...extra };
  if (range) {
    let start = range[1] === '' ? total - Number(range[2]) : Number(range[1]);
    let end = range[1] === '' || range[2] === '' ? total - 1 : Math.min(Number(range[2]), total - 1);
    if (start >= total || start < 0) { res.writeHead(416, { 'content-range': `bytes */${total}` }); return res.end(); }
    res.writeHead(206, { ...head, 'content-range': `bytes ${start}-${end}/${total}`, 'content-length': end - start + 1 });
    return res.end(req.method === 'HEAD' ? undefined : buf.subarray(start, end + 1));
  }
  res.writeHead(200, { ...head, 'content-length': total });
  res.end(req.method === 'HEAD' ? undefined : buf);
}
const sendFile = (req, res, file, extra) =>
  sendBuf(req, res, fs.readFileSync(file), TYPES[path.extname(file)] || 'application/octet-stream', extra);

function textSvg(text) {
  const t = String(text || '').replace(/[<&>]/g, '');
  return Buffer.from(`<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 540 960"><rect width="540" height="960" fill="#111"/>`
    + `<circle cx="270" cy="380" r="90" fill="#c8ff3e"/><text x="270" y="580" fill="#fff" font-family="system-ui,sans-serif" font-weight="900" font-size="54" text-anchor="middle">${t}</text></svg>`);
}

function projectFile(req, res, p, rel) {
  const up = uploads.get(`${p.id}/${rel}`);
  if (up) return sendBuf(req, res, up.buf, up.type);
  const h = p.format === 'horizontal';
  if (rel === 'render/final.mp4') return p.render ? sendFile(req, res, path.join(MEDIA, h ? 'final-h.mp4' : 'final.mp4')) : err(res, 404, 'not rendered');
  if (rel === 'voice/voice.wav') return p.voice_track ? sendFile(req, res, path.join(MEDIA, 'voice.wav')) : err(res, 404, 'no voice');
  if (rel === 'render/credits.txt') {
    const lines = p.beats.filter((b) => b.asset?.credit).map((b) => `${b.id}  ${b.asset.credit}  ${b.asset.source_url}`);
    return sendBuf(req, res, Buffer.from(`ClipCut credits — ${p.topic}\n\n${lines.join('\n')}\n`), TYPES['.txt']);
  }
  const beat = p.beats.find((b) => b.asset?.path === rel);
  if (!beat) return err(res, 404, `no such file: ${rel}`);
  const k = beat.asset.provider === 'upload' ? 'upload' : beat.status === 'fallback' ? 'image' : beat.visual.kind;
  if (rel.endsWith('.svg')) return sendBuf(req, res, textSvg(beat.visual.on_screen_text || beat.text.split(' ').slice(0, 3).join(' ')), TYPES['.svg']);
  if (rel.endsWith('.mp4')) return sendFile(req, res, path.join(MEDIA, `clip-${['youtube', 'gif', 'stock', 'upload'].includes(k) ? k : 'youtube'}.mp4`));
  return sendFile(req, res, path.join(MEDIA, `img-${['meme', 'article'].includes(k) ? k : 'image'}.png`));
}

async function body(req) {
  const chunks = [];
  for await (const c of req) chunks.push(c);
  return Buffer.concat(chunks);
}
async function jsonBody(req) {
  const b = await body(req);
  if (!b.length) return {};
  return JSON.parse(b.toString('utf8'));
}

function parseMultipart(buf, ctype) {
  const m = /boundary=(?:"([^"]+)"|([^;]+))/.exec(ctype || '');
  if (!m) return null;
  const boundary = Buffer.from(`--${m[1] || m[2]}`);
  let pos = buf.indexOf(boundary);
  while (pos !== -1) {
    const next = buf.indexOf(boundary, pos + boundary.length);
    if (next === -1) break;
    const part = buf.subarray(pos + boundary.length + 2, next - 2);
    const sep = part.indexOf('\r\n\r\n');
    const headers = part.subarray(0, sep).toString();
    if (/name="file"/.test(headers)) {
      return {
        filename: /filename="([^"]*)"/.exec(headers)?.[1] || 'upload.bin',
        type: /content-type:\s*([^\r\n]+)/i.exec(headers)?.[1] || 'application/octet-stream',
        data: part.subarray(sep + 4),
      };
    }
    pos = next;
  }
  return null;
}

const PATCHABLE = ['topic', 'notes', 'voice', 'autopilot', 'format', 'target_seconds'];

async function api(req, res, url) {
  const parts = url.pathname.split('/').filter(Boolean).slice(1); // drop "api"
  const M = req.method;
  const [a, id, b, beatId, op] = parts;

  if (a === 'events' && M === 'GET') {
    res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache', connection: 'keep-alive' });
    res.write(': hello\n\n');
    clients.add(res);
    req.on('close', () => clients.delete(res));
    return;
  }
  // The real server draws a QR code of the Network URL; this is a stand-in with its corner squares.
  if (a === 'status' && id === 'qr.svg') {
    const finder = (x, y) => `<path d="M${x} ${y}h7v7h-7zM${x + 1} ${y + 1}v5h5v-5zM${x + 2} ${y + 2}h3v3h-3z" fill-rule="evenodd"/>`;
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 33 33" shape-rendering="crispEdges"><rect width="33" height="33" fill="#fff"/><g fill="#000">${finder(4, 4)}${finder(22, 4)}${finder(4, 22)}<text x="16.5" y="19" font-size="4" text-anchor="middle" font-family="monospace">mock</text></g></svg>`;
    return sendBuf(req, res, Buffer.from(svg), 'image/svg+xml', { 'cache-control': 'no-store' });
  }
  if (a === 'status') return json(res, 200, statusView());
  if (a === 'config') {
    if (M === 'GET') return json(res, 200, config);
    if (M === 'PUT') { config = await jsonBody(req); return json(res, 200, config); }
  }
  if (a === 'keys') {
    if (M === 'GET' && !id) return json(res, 200, keysView());
    if (M === 'PUT' && id) {
      if (!SERVICES.some(([s]) => s === id)) return err(res, 404, `unknown service ${id}`);
      const { keys: incoming = [] } = await jsonBody(req);
      if (incoming.length > 5) return err(res, 400, 'at most 5 keys');
      const old = keys[id] || [];
      const out = [];
      for (let i = 0; i < Math.max(old.length, incoming.length); i++) {
        const v = incoming[i] ?? '';
        if (v === '__remove__') continue;
        if (v === '') { if (old[i]) out.push(old[i]); } else out.push(v.trim());
      }
      keys[id] = out;
      for (const k of Object.keys(keyState)) if (k.startsWith(`${id}/`)) delete keyState[k];
      const kc = checks.find((c) => c.name === 'keys');
      kc.detail = `${Object.values(keys).filter((v) => v.length).length} of 8 services have keys`;
      return json(res, 200, keysView());
    }
  }
  if (a === 'voices') {
    if (M === 'GET' && !id) return json(res, 200, VOICES);
    if (M === 'POST' && id === 'preview') { await body(req); await sleep(400); return sendFile(req, res, path.join(MEDIA, 'preview.wav')); }
  }
  if (a === 'setup' && M === 'POST') {
    const { what } = await jsonBody(req);
    const targets = what === 'all' ? ['yt-dlp', 'kokoro', 'searxng'] : [what];
    json(res, 202, { ok: true });
    (async () => {
      for (const t of targets) {
        for (let i = 0; i <= 5; i++) {
          await sleep(500);
          progress('_setup', null, i < 5 ? `${t}: ${['resolving', 'downloading', 'downloading', 'verifying', 'installing'][i]}…` : `${t}: installed`, { progress: i / 5 });
        }
        if (t === 'searxng' && !process.env.MOCK_PODMAN) {
          progress('_setup', null, 'searxng: podman not found — install podman, then retry', { level: 'error' });
          continue;
        }
        const c = checks.find((c) => c.name.includes(t === 'kokoro' ? 'kokoro' : t));
        if (c) Object.assign(c, { ok: true, detail: 'installed', fix: null });
      }
      progress('_setup', null, 'Setup finished');
    })();
    return;
  }

  if (a !== 'projects') return err(res, 404, 'not found');
  if (!id) {
    if (M === 'GET') return json(res, 200, [...projects.values()].sort((x, y) => y.created_at.localeCompare(x.created_at)).map(summary));
    if (M === 'POST') {
      const q = await jsonBody(req);
      if (!q.topic?.trim()) return err(res, 400, 'topic is required');
      const d = new Date();
      const pid = `${d.toISOString().slice(0, 19).replace(/[-:]/g, '').replace('T', '-')}-${Math.random().toString(16).slice(2, 8)}`;
      const p = baseProject(pid, q.topic.trim(), {
        created_at: now(), updated_at: now(), notes: q.notes || '', format: q.format || config.video.format,
        target_seconds: q.target_seconds || config.video.target_seconds, voice: q.voice || null, autopilot: q.autopilot ?? true,
      });
      projects.set(pid, p);
      run(p);
      return json(res, 200, p);
    }
  }
  const p = projects.get(id);
  if (!p) return err(res, 404, `project ${id} not found`);

  if (!b) {
    if (M === 'GET') return json(res, 200, p);
    if (M === 'DELETE') { tasks.get(id) && (tasks.get(id).cancelled = true); projects.delete(id); res.writeHead(204); return res.end(); }
    if (M === 'PATCH') {
      if (tasks.has(id)) return err(res, 409, 'project is running; cancel it first');
      const q = await jsonBody(req);
      for (const k of PATCHABLE) if (k in q) p[k] = q[k];
      if ('chosen_hook' in q) {
        if (q.chosen_hook != null && !p.hooks[q.chosen_hook]) return err(res, 400, 'no such hook');
        p.chosen_hook = q.chosen_hook;
        if (stageOf(p, 'script').status !== 'pending') resetFrom(p, 'script');
      }
      if ('script_text' in q) {
        if (!p.script) return err(res, 400, 'no script yet');
        p.script.text = q.script_text;
        p.script.sentences = (q.script_text.match(/[^.!?]+[.!?]*/g) || []).map((s) => s.trim()).filter(Boolean);
        resetFrom(p, 'beats');
      }
      for (const k of ['title', 'description', 'hashtags']) if (k in q && p.script) p.script[k] = q[k];
      touch(p);
      return json(res, 200, p);
    }
  }
  if (b === 'run' && M === 'POST') {
    if (tasks.has(id)) return err(res, 409, 'already running');
    const q = await jsonBody(req);
    if (q.from && !STAGES.includes(q.from)) return err(res, 400, `bad stage ${q.from}`);
    run(p, q.from, q.only);
    return json(res, 202, p);
  }
  if (b === 'cancel' && M === 'POST') { const t = tasks.get(id); if (t) t.cancelled = true; return json(res, 202, p); }
  if (b === 'duplicate' && M === 'POST') {
    const c = structuredClone(p);
    c.id = `${id.slice(0, 15)}-${Math.random().toString(16).slice(2, 8)}`;
    c.created_at = c.updated_at = now();
    resetFrom(c, 'beats');
    for (const s of ['research', 'hooks', 'script']) if (stageOf(c, s).status !== 'done') resetFrom(c, s);
    projects.set(c.id, c);
    touch(c);
    return json(res, 200, c);
  }
  if (b === 'still') return sendFile(req, res, path.join(MEDIA, p.format === 'horizontal' ? 'thumb-h.png' : 'thumb.png'));
  if (b === 'download') return p.render ? sendFile(req, res, path.join(MEDIA, p.format === 'horizontal' ? 'final-h.mp4' : 'final.mp4'), { 'content-disposition': `attachment; filename="${id}.mp4"` }) : err(res, 404, 'not rendered');
  if (b === 'files') return projectFile(req, res, p, decodeURIComponent(parts.slice(3).join('/')));

  if (b === 'beats' && beatId) {
    const beat = p.beats.find((x) => x.id === beatId);
    if (!beat) return err(res, 404, `beat ${beatId} not found`);
    if (!op && M === 'PATCH') {
      const q = await jsonBody(req);
      if ('text' in q) beat.text = q.text;
      if ('locked' in q) beat.locked = !!q.locked;
      if (q.visual) beat.visual = { ...beat.visual, ...q.visual };
      touch(p);
      return json(res, 200, beat);
    }
    if (M !== 'POST') return err(res, 405, 'method not allowed');
    if (op === 'search') {
      const q = await jsonBody(req);
      const kind = q.kind || beat.visual.kind;
      progress(p.id, 'assets', `${beat.id}: searching ${kind} for "${q.query || beat.visual.query}"`, { beat_id: beat.id });
      await sleep(1200);
      if ((q.query || '').includes('fail')) return err(res, 502, `${PROVIDER[kind]}: no results for "${q.query}"`);
      beat.candidates = candidates(kind, q.query || beat.visual.query);
      beat.chosen = null;
      touch(p);
      return json(res, 200, beat);
    }
    if (op === 'choose') {
      const { index } = await jsonBody(req);
      const c = beat.candidates[index];
      if (!c) return err(res, 400, 'no such candidate');
      progress(p.id, 'assets', `${beat.id}: downloading ${c.provider} asset`, { beat_id: beat.id });
      await sleep(1500);
      uploads.delete(`${p.id}/${beat.asset?.path}`);
      beat.visual.kind = c.kind;
      Object.assign(beat, { chosen: index, asset: assetFor(c.kind, `${beat.id}-${index}`, c.provider), status: 'ready', error: null, locked: true });
      touch(p);
      return json(res, 200, beat);
    }
    if (op === 'auto') {
      progress(p.id, 'assets', `${beat.id}: re-running automatic pick`, { beat_id: beat.id });
      await sleep(1800);
      beat.candidates = candidates(beat.visual.kind, beat.visual.query);
      Object.assign(beat, { chosen: 0, asset: assetFor(beat.visual.kind, `${beat.id}-auto`), status: 'ready', error: null, locked: false });
      touch(p);
      return json(res, 200, beat);
    }
    if (op === 'upload') {
      const f = parseMultipart(await body(req), req.headers['content-type']);
      if (!f) return err(res, 400, 'multipart field "file" is required');
      const isVideo = f.type.startsWith('video/');
      const ext = path.extname(f.filename) || (isVideo ? '.mp4' : '.png');
      const rel = `assets/${beat.id}-upload${ext}`;
      uploads.set(`${p.id}/${rel}`, { buf: f.data, type: f.type });
      Object.assign(beat, {
        asset: { kind: isVideo ? 'video' : 'image', path: rel, provider: 'upload', source_url: '', credit: `Uploaded: ${f.filename}`, width: 0, height: 0, duration_s: null, trim_start_s: 0, focus: null },
        status: 'ready', error: null, locked: true, chosen: null,
      });
      touch(p);
      return json(res, 200, beat);
    }
  }
  return err(res, 404, 'not found');
}

function lanIp() {
  try { return execFileSync('sh', ['-c', "hostname -I | awk '{print $1}'"]).toString().trim() || '127.0.0.1'; } catch {}
  for (const list of Object.values(os.networkInterfaces())) for (const i of list || []) if (i.family === 'IPv4' && !i.internal) return i.address;
  return '127.0.0.1';
}

if (!SOURCES_ONLY) makeMedia();
makeSourceMedia();
const LAN = lanIp();

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, 'http://x');
  try {
    if (url.pathname.startsWith('/sources-mock/')) return await sourceMock(req, res, url);
    if (url.pathname.startsWith('/api/')) return await api(req, res, url);
    if (url.pathname.startsWith('/mock/')) return sendFile(req, res, path.join(MEDIA, path.basename(url.pathname)));
    const rel = url.pathname === '/' ? 'index.html' : url.pathname.slice(1);
    const file = path.join(WEB, path.normalize(rel));
    if (!file.startsWith(WEB) || !fs.existsSync(file) || fs.statSync(file).isDirectory()) return sendFile(req, res, path.join(WEB, 'index.html'));
    return sendFile(req, res, file);
  } catch (e) {
    console.error(e);
    if (!res.headersSent) err(res, 500, String(e.message || e));
  }
});
server.listen(PORT, '0.0.0.0', () => {
  console.log(`ClipCut mock server\n  Local:   http://localhost:${PORT}\n  Network: http://${LAN}:${PORT}`);
  console.log(`  Sources: http://${LAN}:${PORT}/sources-mock/ (synthetic metadata + tiny real media)`);
  if (!SOURCES_ONLY) setTimeout(() => run(projects.get(runningId)), 1500);
});
