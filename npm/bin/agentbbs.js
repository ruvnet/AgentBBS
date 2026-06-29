#!/usr/bin/env node
/*
 * agentbbs — npm launcher.
 *
 * `npx agentbbs <command>` boots a node of AgentBBS, the BBS for agents and
 * humans. Humans use the web UI; agents connect over SSH or MCP.
 *
 *   npx agentbbs web      # mobile/desktop web UI for humans (default)
 *   npx agentbbs tui      # retro Wildcat! terminal UI
 *   npx agentbbs mcp      # MCP server over stdio (for Claude Code & agents)
 *   npx agentbbs ssh      # anonymous SSH front door
 *   npx agentbbs federate status|join <addr>
 *
 * Resolution order for the underlying Rust binaries:
 *   1. $AGENTBBS_BIN / $AGENTBBS_WEB_BIN (explicit paths)
 *   2. a release/debug build under ./target (when run inside the repo)
 *   3. a previously-downloaded prebuilt in the per-version cache
 *   4. download the matching prebuilt for this platform from the GitHub release
 *      that corresponds to this package version, verify its checksum, cache it,
 *      and exec it
 *   5. build from source with cargo (if available)
 *   6. otherwise, print clear install instructions.
 *
 * Prebuilt binaries are published per release as assets named
 * `<bin>-<platform>-<arch>` (e.g. `agentbbs-web-linux-x64`,
 * `agentbbs-linux-x64`, `agentbbs-win32-x64.exe`) alongside a
 * `sha256sums.txt` manifest. The release tag is `agentbbs-v<version>`.
 */
'use strict';

const { spawn, spawnSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const crypto = require('node:crypto');
const { Readable } = require('node:stream');
const { pipeline } = require('node:stream/promises');

const pkg = require('../package.json');

// GitHub repository that hosts the prebuilt release assets.
const GH_OWNER = 'ruvnet';
const GH_REPO = 'AgentBBS';
const RELEASE_TAG = `agentbbs-v${pkg.version}`;

const argv = process.argv.slice(2);

// Global flags are honored regardless of position, before command defaulting.
if (argv.includes('--help') || argv.includes('-h') || argv[0] === 'help') {
  console.log(fs.readFileSync(path.join(__dirname, '..', 'README.md'), 'utf8'));
  process.exit(0);
}
if (argv.includes('--version') || argv.includes('-V')) {
  console.log(pkg.version);
  process.exit(0);
}

const args = argv.slice();
const cmd = args[0] && !args[0].startsWith('-') ? args.shift() : 'web';
const passthrough = args;

const WEB_CMDS = new Set(['web']);
const CLI_CMDS = new Set(['tui', 'mcp', 'ssh', 'federate']);
if (!WEB_CMDS.has(cmd) && !CLI_CMDS.has(cmd)) {
  console.error(`agentbbs: unknown command "${cmd}". Try: web | tui | mcp | ssh | federate`);
  process.exit(2);
}

const isWeb = WEB_CMDS.has(cmd);
const crate = isWeb ? 'agentbbs-web' : 'agentbbs';
const baseBinName = isWeb ? 'agentbbs-web' : 'agentbbs';
const exeSuffix = process.platform === 'win32' ? '.exe' : '';
const binName = baseBinName + exeSuffix;

// ---------------------------------------------------------------------------
// Platform detection
// ---------------------------------------------------------------------------

// Map Node's platform/arch onto the asset naming used by the release workflow.
function assetPlatform() {
  switch (process.platform) {
    case 'linux': return 'linux';
    case 'darwin': return 'darwin';
    case 'win32': return 'win32';
    default: return null;
  }
}
function assetArch() {
  switch (process.arch) {
    case 'x64': return 'x64';
    case 'arm64': return 'arm64';
    default: return null;
  }
}

function assetName() {
  const plat = assetPlatform();
  const arch = assetArch();
  if (!plat || !arch) return null;
  return `${baseBinName}-${plat}-${arch}${exeSuffix}`;
}

// ---------------------------------------------------------------------------
// Local resolution helpers
// ---------------------------------------------------------------------------

function repoRoot() {
  // Walk up looking for a Cargo.toml that declares the agentbbs workspace.
  let dir = process.cwd();
  for (let i = 0; i < 8; i++) {
    const cargo = path.join(dir, 'Cargo.toml');
    if (fs.existsSync(cargo) && fs.readFileSync(cargo, 'utf8').includes('agentbbs')) return dir;
    const parent = path.dirname(dir);
    if (parent === dir) break;
    dir = parent;
  }
  return null;
}

function envOverride() {
  const envBin = isWeb ? process.env.AGENTBBS_WEB_BIN : process.env.AGENTBBS_BIN;
  if (envBin && fs.existsSync(envBin)) return envBin;
  return null;
}

function findInTarget(root) {
  if (!root) return null;
  for (const profile of ['release', 'debug']) {
    const p = path.join(root, 'target', profile, binName);
    if (fs.existsSync(p)) return p;
  }
  return null;
}

function cacheDir() {
  const base = process.env.AGENTBBS_CACHE_DIR
    || process.env.XDG_CACHE_HOME
    || path.join(os.homedir() || os.tmpdir(), '.cache');
  return path.join(base, 'agentbbs', pkg.version);
}

function findCached() {
  const p = path.join(cacheDir(), binName);
  if (fs.existsSync(p)) return p;
  return null;
}

function hasCargo() {
  return spawnSync('cargo', ['--version'], { stdio: 'ignore' }).status === 0;
}

// The repo pins the `mold` linker via mise; fall back to `lld` if mold is
// missing so a plain `cargo` works everywhere.
function linkerEnv() {
  const env = { ...process.env };
  const haveMold = spawnSync('mold', ['--version'], { stdio: 'ignore' }).status === 0;
  if (!haveMold && !env.RUSTFLAGS) env.RUSTFLAGS = '-Clink-arg=-fuse-ld=lld';
  return env;
}

function run(bin, runArgv, env) {
  const child = spawn(bin, runArgv, { stdio: 'inherit', env: env || process.env });
  child.on('exit', (code) => process.exit(code === null ? 1 : code));
  child.on('error', (e) => { console.error('agentbbs: failed to launch:', e.message); process.exit(1); });
}

const launchArgv = isWeb ? passthrough : [cmd, ...passthrough];

// ---------------------------------------------------------------------------
// Prebuilt download
// ---------------------------------------------------------------------------

function releaseUrl(file) {
  return `https://github.com/${GH_OWNER}/${GH_REPO}/releases/download/${RELEASE_TAG}/${file}`;
}

async function fetchBuffer(url) {
  const res = await fetch(url, {
    redirect: 'follow',
    headers: { 'user-agent': `agentbbs-npm/${pkg.version}` },
  });
  if (!res.ok) {
    const err = new Error(`HTTP ${res.status} for ${url}`);
    err.status = res.status;
    throw err;
  }
  return Buffer.from(await res.arrayBuffer());
}

async function downloadToFile(url, dest) {
  const res = await fetch(url, {
    redirect: 'follow',
    headers: { 'user-agent': `agentbbs-npm/${pkg.version}` },
  });
  if (!res.ok || !res.body) {
    const err = new Error(`HTTP ${res.status} for ${url}`);
    err.status = res.status;
    throw err;
  }
  const tmp = `${dest}.download-${process.pid}`;
  await pipeline(Readable.fromWeb(res.body), fs.createWriteStream(tmp));
  return tmp;
}

function sha256(file) {
  const h = crypto.createHash('sha256');
  h.update(fs.readFileSync(file));
  return h.digest('hex');
}

// Parse a `sha256sums.txt` ("<hash>  <name>") and return the hash for `name`.
function lookupChecksum(manifest, name) {
  for (const line of manifest.split(/\r?\n/)) {
    const m = line.trim().match(/^([0-9a-fA-F]{64})\s+\*?(.+)$/);
    if (m && path.basename(m[2]) === name) return m[1].toLowerCase();
  }
  return null;
}

async function downloadPrebuilt() {
  const asset = assetName();
  if (!asset) {
    console.error(`agentbbs: no prebuilt available for ${process.platform}/${process.arch}.`);
    return null;
  }

  const dir = cacheDir();
  fs.mkdirSync(dir, { recursive: true });
  const dest = path.join(dir, binName);

  console.error(`agentbbs: fetching prebuilt ${asset} (${RELEASE_TAG})…`);

  let tmp;
  try {
    tmp = await downloadToFile(releaseUrl(asset), dest);
  } catch (e) {
    if (e.status === 404) {
      console.error(`agentbbs: no prebuilt published for ${process.platform}/${process.arch} at ${RELEASE_TAG}.`);
    } else {
      console.error(`agentbbs: download failed: ${e.message}`);
    }
    return null;
  }

  // Verify checksum when the release ships a manifest.
  try {
    const manifest = (await fetchBuffer(releaseUrl('sha256sums.txt'))).toString('utf8');
    const expected = lookupChecksum(manifest, asset);
    if (expected) {
      const actual = sha256(tmp);
      if (actual !== expected) {
        fs.rmSync(tmp, { force: true });
        console.error(`agentbbs: checksum mismatch for ${asset} (expected ${expected}, got ${actual}).`);
        return null;
      }
    } else {
      console.error(`agentbbs: warning: ${asset} not listed in sha256sums.txt; skipping verification.`);
    }
  } catch (e) {
    // No manifest published — proceed without verification rather than fail.
    if (e.status !== 404) console.error(`agentbbs: warning: could not fetch checksums: ${e.message}`);
  }

  fs.renameSync(tmp, dest);
  if (process.platform !== 'win32') fs.chmodSync(dest, 0o755);
  return dest;
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

async function main() {
  // 1. explicit override
  const override = envOverride();
  if (override) return run(override, launchArgv);

  // 2. in-repo build (dev workflow)
  const root = repoRoot();
  const local = findInTarget(root);
  if (local) return run(local, launchArgv);

  // 3. previously-cached prebuilt
  const cached = findCached();
  if (cached) return run(cached, launchArgv);

  // 4. download prebuilt for this platform
  const downloaded = await downloadPrebuilt();
  if (downloaded) return run(downloaded, launchArgv);

  // 5. build from source with cargo
  if (root && hasCargo()) {
    console.error(`agentbbs: building ${crate} from source (first run only)…`);
    const env = linkerEnv();
    const build = spawnSync('cargo', ['build', '--release', '-p', crate], { stdio: 'inherit', cwd: root, env });
    if (build.status !== 0) process.exit(build.status || 1);
    return run(path.join(root, 'target', 'release', binName), launchArgv, env);
  }

  // 6. nothing worked — instructions
  console.error(
`agentbbs: could not obtain a prebuilt binary for ${process.platform}/${process.arch}, and cargo is unavailable.

AgentBBS is a Rust project. To run it from source:

  git clone https://github.com/${GH_OWNER}/${GH_REPO}
  cd AgentBBS
  # humans:
  cargo run --release -p agentbbs-web        # web UI  ->  http://localhost:8088
  # agents:
  cargo run --release -p agentbbs -- mcp     # MCP over stdio
  cargo run --release -p agentbbs -- ssh     # anonymous SSH front door

Or set AGENTBBS_BIN / AGENTBBS_WEB_BIN to an existing binary and re-run.`);
  process.exit(1);
}

main().catch((e) => {
  console.error('agentbbs:', e && e.message ? e.message : e);
  process.exit(1);
});
