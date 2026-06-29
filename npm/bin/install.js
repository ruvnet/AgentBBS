#!/usr/bin/env node
/*
 * postinstall: best-effort fetch of a prebuilt AgentBBS binary for this
 * platform from the GitHub release (or $AGENTBBS_RELEASE_BASE). This is purely
 * an optimisation — if no prebuilt is available, the launcher (agentbbs.js)
 * builds from source on first run. So this NEVER fails the install (exit 0).
 */
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const https = require('node:https');

const pkg = require('../package.json');
const VERSION = pkg.version;
const BASE =
  process.env.AGENTBBS_RELEASE_BASE ||
  `https://github.com/ruvnet/agentbbs/releases/download/v${VERSION}`;

// Map Node's platform/arch to a Rust target triple used in release asset names.
function target() {
  const p = process.platform;
  const a = process.arch;
  if (p === 'linux' && a === 'x64') return 'x86_64-unknown-linux-gnu';
  if (p === 'linux' && a === 'arm64') return 'aarch64-unknown-linux-gnu';
  if (p === 'darwin' && a === 'x64') return 'x86_64-apple-darwin';
  if (p === 'darwin' && a === 'arm64') return 'aarch64-apple-darwin';
  if (p === 'win32' && a === 'x64') return 'x86_64-pc-windows-msvc';
  return null;
}

function download(url, dest, redirects = 0) {
  return new Promise((resolve, reject) => {
    if (redirects > 5) return reject(new Error('too many redirects'));
    https
      .get(url, { headers: { 'user-agent': 'agentbbs-npm' } }, (res) => {
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          res.resume();
          return resolve(download(res.headers.location, dest, redirects + 1));
        }
        if (res.statusCode !== 200) {
          res.resume();
          return reject(new Error(`HTTP ${res.statusCode}`));
        }
        const tmp = dest + '.partial';
        const out = fs.createWriteStream(tmp);
        res.pipe(out);
        out.on('finish', () => out.close(() => { fs.renameSync(tmp, dest); resolve(); }));
        out.on('error', reject);
      })
      .on('error', reject);
  });
}

async function main() {
  const t = target();
  if (!t) {
    console.error(`agentbbs: no prebuilt for ${process.platform}/${process.arch}; will build from source on first run.`);
    return;
  }
  const ext = process.platform === 'win32' ? '.exe' : '';
  const dir = path.join(__dirname, '..', 'binaries');
  fs.mkdirSync(dir, { recursive: true });
  let got = 0;
  for (const name of ['agentbbs', 'agentbbs-web']) {
    const asset = `${name}-${t}${ext}`;
    const dest = path.join(dir, `${name}${ext}`);
    try {
      await download(`${BASE}/${asset}`, dest);
      if (process.platform !== 'win32') fs.chmodSync(dest, 0o755);
      got++;
    } catch (e) {
      console.error(`agentbbs: prebuilt ${asset} unavailable (${e.message}).`);
    }
  }
  if (got === 0) {
    console.error('agentbbs: no prebuilt binaries fetched; the CLI will build from source on first run.');
  } else {
    console.error(`agentbbs: installed ${got} prebuilt binar${got === 1 ? 'y' : 'ies'} for ${t}.`);
  }
}

main().catch((e) => {
  console.error('agentbbs: postinstall skipped:', e.message);
}); // never throw — install must not fail
