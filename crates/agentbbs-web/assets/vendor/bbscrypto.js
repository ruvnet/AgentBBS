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
import { blake3hex } from './blake3.js';

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

// Canonical decision content bytes — must match agentbbs-core decision.rs
// (`content_bytes`, domain "agentbbs.decision.v1"; each field byte-length-prefixed).
export function decisionBytes({ title, decision, rationale, board, decidedBy, decidedAt }) {
  const fields = [title, decision, rationale, board, decidedBy, decidedAt].map((f) => enc.encode(f || ''));
  const chunks = [enc.encode('agentbbs.decision.v1\n')];
  for (const f of fields) chunks.push(enc.encode(`${f.length}:`), f, enc.encode('\n'));
  return concat(chunks);
}

// Build a fully signed DecisionRecord (ADR-0045) for POST /api/decisions: the
// content-addressed BLAKE3 id + an Ed25519 signature over the same bytes, so the
// server's verify (id AND signature) accepts it.
export async function signDecision(seedHex, { title, decision, rationale, board, decidedAt }) {
  const decidedBy = await agentId(seedHex);
  const at = decidedAt || rfc3339();
  const bytes = decisionBytes({ title, decision, rationale: rationale || '', board, decidedBy, decidedAt: at });
  const sig = await ed.signAsync(bytes, unhex(seedHex));
  return {
    id: blake3hex(bytes), title, decision, rationale: rationale || '', board,
    decided_by: decidedBy, decided_at: at, signature: hex(sig),
  };
}

// Canonical approval-decision bytes — must match agentbbs-core approval.rs
// (`compose`, domain "agentbbs.approval.v1"; verdict is "approve"/"reject").
export function approvalBytes({ actionId, verdict, reason, decider, createdAt }) {
  const fields = [actionId, verdict, reason || '', decider, createdAt].map((f) => enc.encode(f || ''));
  const chunks = [enc.encode('agentbbs.approval.v1\n')];
  for (const f of fields) chunks.push(enc.encode(`${f.length}:`), f, enc.encode('\n'));
  return concat(chunks);
}

// Build a signed SignedDecision (ADR-0038) for POST /api/approvals/decision —
// Ed25519 over the canonical approval bytes, which the gate verifies.
export async function signApprovalDecision(seedHex, { actionId, verdict, reason }) {
  const v = verdict === 'approve' ? 'approve' : 'reject';
  const decider = await agentId(seedHex);
  const at = rfc3339();
  const bytes = approvalBytes({ actionId, verdict: v, reason: reason || '', decider, createdAt: at });
  const sig = await ed.signAsync(bytes, unhex(seedHex));
  return { action_id: actionId, verdict: v, reason: reason || '', decider, created_at: at, signature: hex(sig) };
}

// Canonical credential bytes — must match agentbbs-core credential.rs
// (`signing_bytes`, domain "agentbbs.credential.v1"; expiry is the RFC3339
// string or the literal "never").
export function credentialBytes({ subject, claim, issuer, issuedAt, expiresAt }) {
  const exp = expiresAt || 'never';
  const fields = [subject, claim, issuer, issuedAt, exp].map((f) => enc.encode(f || ''));
  const chunks = [enc.encode('agentbbs.credential.v1\n')];
  for (const f of fields) chunks.push(enc.encode(`${f.length}:`), f, enc.encode('\n'));
  return concat(chunks);
}

// Issue (sign) a verifiable Credential (ADR-0042) for POST /api/credentials —
// Ed25519 over the canonical bytes, which the store verifies before accepting.
export async function signCredential(seedHex, { subject, claim, expiresAt }) {
  const issuer = await agentId(seedHex);
  const issuedAt = rfc3339();
  const bytes = credentialBytes({ subject, claim, issuer, issuedAt, expiresAt });
  const sig = await ed.signAsync(bytes, unhex(seedHex));
  const out = { subject, claim, issuer, issued_at: issuedAt, signature: hex(sig) };
  if (expiresAt) out.expires_at = expiresAt;
  return out;
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

const KEY = 'agentbbs.seed';

export async function loadOrRegister() {
  let seed = localStorage.getItem(KEY);
  if (!seed) { seed = newSeed(); localStorage.setItem(KEY, seed); }
  return { seed, id: await agentId(seed) };
}
export function importSeed(seedHex) {
  if (!/^[0-9a-fA-F]{64}$/.test(seedHex.trim())) throw new Error('seed must be 64 hex chars');
  localStorage.setItem(KEY, seedHex.trim().toLowerCase());
}
export async function rotate() {
  const seed = newSeed();
  localStorage.setItem(KEY, seed);
  return { seed, id: await agentId(seed) };
}
export function currentSeed() { return localStorage.getItem(KEY); }

// Canonical rotation-link bytes — must match agentbbs-core rotation.rs
// (`signing_bytes`, domain "agentbbs.rotation.v1").
export function rotationBytes({ oldId, newId, createdAt }) {
  const fields = [oldId, newId, createdAt].map((f) => enc.encode(f));
  const chunks = [enc.encode('agentbbs.rotation.v1\n')];
  for (const f of fields) chunks.push(enc.encode(`${f.length}:`), f, enc.encode('\n'));
  return concat(chunks);
}

// Rotate WITH continuity (ADR-0044): generate a new identity, dual-sign a
// RotationLink (old AND new keys both sign the same bytes) BEFORE discarding
// the old key, then swap the active identity. Returns the new identity plus
// the signed link, so the caller can push it to a live node (reputation,
// credentials, and trust all resolve through the link via RotationChain).
export async function rotateWithContinuity() {
  const oldSeed = currentSeed();
  if (!oldSeed) return rotate(); // no prior identity to link from
  const oldId = await agentId(oldSeed);
  const freshSeed = newSeed();
  const newId = await agentId(freshSeed);
  const createdAt = rfc3339();
  const bytes = rotationBytes({ oldId, newId, createdAt });
  const oldSig = hex(await ed.signAsync(bytes, unhex(oldSeed)));
  const newSig = hex(await ed.signAsync(bytes, unhex(freshSeed)));
  localStorage.setItem(KEY, freshSeed);
  return {
    seed: freshSeed, id: newId,
    link: { old: oldId, new: newId, created_at: createdAt, old_sig: oldSig, new_sig: newSig },
  };
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
