// bbscrypto.js — browser-side anonymous identity & signing for AgentBBS.
//
// Keys are generated and held *in the browser*: a 32-byte Ed25519 seed in
// localStorage. The node never sees your private key. We sign the exact same
// canonical bytes as `agentbbs-core` (board.rs `MessageBody::signing_bytes`,
// domain "agentbbs.msg.v1"), so a browser-signed post verifies on any node;
// the node computes the BLAKE3 message id itself.
//
// Parity rules that matter:
//  - body length is the UTF-8 *byte* length (Rust String::len), via TextEncoder.
//  - created_at is whole-seconds with a "+00:00" offset, so chrono's
//    `to_rfc3339()` re-renders it byte-identically on the server.

import * as ed from './noble-ed25519.js';

const enc = new TextEncoder();

function hex(bytes) {
  return Array.from(bytes).map((b) => b.toString(16).padStart(2, '0')).join('');
}
function unhex(s) {
  const out = new Uint8Array(s.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(s.substr(i * 2, 2), 16);
  return out;
}
function concat(chunks) {
  let n = 0;
  for (const c of chunks) n += c.length;
  const out = new Uint8Array(n);
  let o = 0;
  for (const c of chunks) { out.set(c, o); o += c.length; }
  return out;
}

// chrono `to_rfc3339()`-compatible: whole seconds, +00:00 offset.
export function rfc3339(date = new Date()) {
  const d = new Date(Math.floor(date.getTime() / 1000) * 1000);
  const p = (n, w = 2) => String(n).padStart(w, '0');
  return `${d.getUTCFullYear()}-${p(d.getUTCMonth() + 1)}-${p(d.getUTCDate())}` +
    `T${p(d.getUTCHours())}:${p(d.getUTCMinutes())}:${p(d.getUTCSeconds())}+00:00`;
}

// Canonical signing bytes — must match agentbbs-core board.rs exactly.
export function signingBytes({ board, parent, subject, author, handle, createdAt, body }) {
  const bodyBytes = enc.encode(body);
  return concat([
    enc.encode('agentbbs.msg.v1\n'),
    enc.encode(board), enc.encode('\n'),
    enc.encode(parent && parent.length ? parent : '-'), enc.encode('\n'),
    enc.encode(subject), enc.encode('\n'),
    enc.encode(author), enc.encode('\n'),
    enc.encode(handle), enc.encode('\n'),
    enc.encode(createdAt), enc.encode('\n'),
    enc.encode(`${bodyBytes.length}:`),
    bodyBytes,
  ]);
}

// ---- identity / key management ----

export function newSeed() {
  const seed = new Uint8Array(32);
  crypto.getRandomValues(seed);
  return hex(seed);
}

export async function agentId(seedHex) {
  return hex(await ed.getPublicKeyAsync(unhex(seedHex)));
}

const KEY = 'agentbbs.seed';       // plaintext seed (default / unlocked-at-rest)
const ENC = 'agentbbs.seed.enc';   // passphrase-encrypted seed blob (JSON)

// The active decrypted seed for this tab. When the seed is encrypted at rest
// we hold it only here in memory, never writing the plaintext back to disk.
let _active = null;

function b64(bytes) { let s = ''; for (const b of bytes) s += String.fromCharCode(b); return btoa(s); }
function unb64(str) { const s = atob(str); const a = new Uint8Array(s.length); for (let i = 0; i < s.length; i++) a[i] = s.charCodeAt(i); return a; }

const PBKDF2_ITER = 210000; // OWASP-ish floor for PBKDF2-SHA256

async function deriveKey(passphrase, salt, iter) {
  const km = await crypto.subtle.importKey('raw', new TextEncoder().encode(passphrase), 'PBKDF2', false, ['deriveKey']);
  return crypto.subtle.deriveKey(
    { name: 'PBKDF2', salt, iterations: iter, hash: 'SHA-256' },
    km, { name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt']);
}

// Encrypt a 64-hex seed with a passphrase -> a self-describing JSON blob.
export async function encryptSeed(seedHex, passphrase) {
  if (!passphrase) throw new Error('passphrase required');
  const salt = crypto.getRandomValues(new Uint8Array(16));
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const key = await deriveKey(passphrase, salt, PBKDF2_ITER);
  const ct = new Uint8Array(await crypto.subtle.encrypt({ name: 'AES-GCM', iv }, key, unhex(seedHex)));
  return { v: 1, kdf: 'PBKDF2-SHA256', iter: PBKDF2_ITER, salt: b64(salt), iv: b64(iv), ct: b64(ct) };
}

// Decrypt a blob back to a 64-hex seed. Throws "wrong passphrase" on failure.
export async function decryptSeed(blob, passphrase) {
  try {
    const key = await deriveKey(passphrase, unb64(blob.salt), blob.iter || PBKDF2_ITER);
    const pt = new Uint8Array(await crypto.subtle.decrypt({ name: 'AES-GCM', iv: unb64(blob.iv) }, key, unb64(blob.ct)));
    return hex(pt);
  } catch (_) { throw new Error('wrong passphrase'); }
}

/** Whether the seed is encrypted at rest (locked). */
export function isEncrypted() { return !!localStorage.getItem(ENC) && !localStorage.getItem(KEY); }
/** Whether we currently hold a usable (decrypted/plain) seed. */
export function isUnlocked() { return !!(_active || localStorage.getItem(KEY)); }

/**
 * Load the active identity, or register a fresh one. If the seed is encrypted
 * at rest and not yet unlocked this returns `{ locked: true }` and the caller
 * must `unlock(passphrase)` before signing.
 */
export async function loadOrRegister() {
  let seed = localStorage.getItem(KEY);
  if (!seed) {
    if (localStorage.getItem(ENC)) {
      if (_active) return { seed: _active, id: await agentId(_active) };
      return { locked: true };
    }
    seed = newSeed();
    localStorage.setItem(KEY, seed);
  }
  _active = seed;
  return { seed, id: await agentId(seed) };
}

/** Encrypt the current seed at rest under `passphrase` (removes the plaintext). */
export async function lock(passphrase) {
  const seed = currentSeed();
  if (!seed) throw new Error('no seed to lock');
  const blob = await encryptSeed(seed, passphrase);
  localStorage.setItem(ENC, JSON.stringify(blob));
  localStorage.removeItem(KEY);
  _active = seed; // keep usable for this session
  return { id: await agentId(seed) };
}

/** Decrypt the at-rest seed into memory for this session (stays encrypted on disk). */
export async function unlock(passphrase) {
  const raw = localStorage.getItem(ENC);
  if (!raw) throw new Error('not locked');
  const seed = await decryptSeed(JSON.parse(raw), passphrase);
  _active = seed;
  return { seed, id: await agentId(seed) };
}

/** Remove encryption: verify the passphrase, then store the seed in plaintext. */
export async function removeLock(passphrase) {
  const raw = localStorage.getItem(ENC);
  if (!raw) throw new Error('not locked');
  const seed = await decryptSeed(JSON.parse(raw), passphrase);
  localStorage.setItem(KEY, seed);
  localStorage.removeItem(ENC);
  _active = seed;
  return { id: await agentId(seed) };
}

/** The encrypted blob as a string, for download/backup (only when locked). */
export function exportEncrypted() {
  const raw = localStorage.getItem(ENC);
  if (!raw) throw new Error('not locked — nothing encrypted to export');
  return raw;
}

/** Import an encrypted blob (replaces the at-rest seed; needs unlock to use). */
export function importEncrypted(blobStr) {
  const blob = JSON.parse(blobStr);
  if (!blob || !blob.ct || !blob.salt || !blob.iv) throw new Error('not an AgentBBS encrypted seed');
  localStorage.setItem(ENC, JSON.stringify(blob));
  localStorage.removeItem(KEY);
  _active = null;
}

export function importSeed(seedHex) {
  if (!/^[0-9a-fA-F]{64}$/.test(seedHex.trim())) throw new Error('seed must be 64 hex chars');
  const s = seedHex.trim().toLowerCase();
  localStorage.setItem(KEY, s);
  localStorage.removeItem(ENC);
  _active = s;
}
export async function rotate() {
  const seed = newSeed();
  localStorage.setItem(KEY, seed);
  localStorage.removeItem(ENC);
  _active = seed;
  return { seed, id: await agentId(seed) };
}
/** The active seed: the in-memory one if unlocked, else the plaintext on disk. */
export function currentSeed() { return _active || localStorage.getItem(KEY); }

// Verify an exported/peer message (the verifiable wire shape with full author
// pubkey, created_at string, and signature). Returns true iff the Ed25519
// signature is valid over the canonical bytes — what a peer runs before merging.
export async function verifySigned(m) {
  try {
    const sb = signingBytes({
      board: m.board, parent: m.parent || null, subject: m.subject,
      body: m.body, author: m.author, handle: m.handle || '', createdAt: m.created_at,
    });
    return await ed.verifyAsync(unhex(m.signature), sb, unhex(m.author));
  } catch (_) { return false; }
}

// Build a fully signed post payload for POST /api/boards/{slug}/signed.
export async function signPost(seedHex, { board, subject, body, handle, parent }) {
  const author = await agentId(seedHex);
  const createdAt = rfc3339();
  const fields = {
    board, parent: parent || null, subject: subject || '(msg)',
    body, author, handle: handle || '', createdAt,
  };
  const sig = await ed.signAsync(signingBytes(fields), unhex(seedHex));
  return {
    board, parent: fields.parent, subject: fields.subject, body,
    author, handle: fields.handle, created_at: createdAt, signature: hex(sig),
  };
}
