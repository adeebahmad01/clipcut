#!/usr/bin/env node
// Explicit, local-only yt-dlp fixture. It never contacts YouTube/downloads.
// Config: [tools] "yt-dlp" = "/absolute/path/to/this/file"
// Requires CLIPCUT_SOURCES_MOCK_BASE for every operation except --version.
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';

const args = process.argv.slice(2);
if (args.includes('--version')) {
  console.log('clipcut-mock-yt-dlp-v1 (synthetic fixture; NOT real yt-dlp)');
  process.exit(0);
}
const base = new URL(process.env.CLIPCUT_SOURCES_MOCK_BASE || 'about:blank');
if (base.protocol !== 'http:' || !/^(127\.|10\.|192\.168\.|172\.(1[6-9]|2\d|3[01])\.)/.test(base.hostname)) {
  throw new Error('mock yt-dlp requires an explicit numeric private HTTP CLIPCUT_SOURCES_MOCK_BASE');
}
if (args.includes('--cookies') || args.includes('--cookies-from-browser')) {
  throw new Error('mock yt-dlp refuses browser cookies');
}
const audit = process.env.CLIPCUT_SOURCES_MOCK_AUDIT || '/tmp/clipcut-sources-smoke/yt-dlp-argv.jsonl';
if (!path.resolve(audit).startsWith('/tmp/clipcut-')) throw new Error('fixture audit must stay under /tmp/clipcut-*');
fs.mkdirSync(path.dirname(audit), { recursive: true });
fs.appendFileSync(audit, JSON.stringify({ mocked: true, args }) + '\n');
const flag = (name) => args[args.indexOf(name) + 1];
const videoUrl = args.find((a) => a.startsWith('https://www.youtube.com/watch?'));
const id = videoUrl ? new URL(videoUrl).searchParams.get('v') : 'fixtureA001';
if (args.includes('--flat-playlist')) {
  const query = args.find((a) => /^ytsearch\d+:/.test(a)) || '';
  if (query.includes('mock-fail')) throw new Error('deterministic YouTube search failure');
  if (query.includes('mock-empty')) process.exit(0);
  const entry = (id, extra = {}) => ({
    id, title: 'Synthetic coffee cherry video', channel: 'Fixture Creator', channel_id: 'fixture-channel',
    duration: 30, live_status: 'not_live', thumbnails: [{ url: `${base.origin}/sources-mock/media/image.png`, width: 640 }],
    ...extra,
  });
  for (const e of [
    entry('fixtureA001'), entry('fixtureB002'),
    entry('fixtureL003', { is_live: true }), entry('fixtureU004', { duration: null }),
    entry('fixtureX005', { duration: 100000 }), entry('fixtureC006', { channel: 'Fixture Blocked' }),
    entry('../invalid'),
  ]) console.log(JSON.stringify(e));
  console.log('{malformed json');
} else if (args.includes('--skip-download')) {
  const file = flag('-o').replaceAll('%(id)s', id).replaceAll('%(ext)s', 'en.vtt');
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, `WEBVTT

00:00:00.000 --> 00:00:02.000
The coffee cherry is a drupe.

00:00:02.000 --> 00:00:04.000
Its seeds are called coffee beans.

00:00:04.000 --> 00:00:07.000
The telescope sees the early universe in infrared.

00:00:07.000 --> 00:00:10.000
This is a synthetic caption fixture, not a live transcript.
`);
} else {
  if (!args.includes('--download-sections')) throw new Error('fixture forbids whole-video downloads');
  const section = /^\*(\d+(?:\.\d+)?)-(\d+(?:\.\d+)?)$/.exec(flag('--download-sections'));
  if (!section) throw new Error('invalid section');
  const length = Number(section[2]) - Number(section[1]);
  if (!(length >= 1 && length <= 3)) throw new Error('fixture sections must be 1-3 seconds');
  const file = path.resolve(flag('-o').replaceAll('%(id)s', id).replaceAll('%(ext)s', 'mp4'));
  fs.mkdirSync(path.dirname(file), { recursive: true });
  execFileSync(args.includes('--ffmpeg-location') ? flag('--ffmpeg-location') : 'ffmpeg', [
    '-nostdin', '-y', '-v', 'error', '-f', 'lavfi', '-i', 'testsrc2=s=240x360:r=30',
    '-t', String(id === 'fullvid0001' ? 5 : length), '-an', '-c:v', 'libx264', '-preset', 'veryfast',
    '-crf', '30', '-threads', '2', '-pix_fmt', 'yuv420p', '-movflags', '+faststart', file,
  ], { stdio: 'inherit', timeout: 15_000 });
  console.log(file);
}
