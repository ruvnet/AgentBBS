# 18. Passphrase-encrypted browser key store

Status: Accepted

## Context

ADR 0016 moved identity into the browser: the Ed25519 seed lives in
`localStorage`. That is great for self-custody but the seed sits in plaintext,
so anyone with local access — a shared machine, a stolen laptop, or an XSS
payload that can read `localStorage` — can exfiltrate it and *become* you. The
threat model (SECURITY-AGENTBBS.md) called this out: "a seed in localStorage is
exposed to any XSS."

## Decision

Add **optional** passphrase encryption of the at-rest seed, using only the
platform `crypto.subtle` (no new dependencies):

- **KDF:** PBKDF2-SHA256, 210 000 iterations, random 16-byte salt.
- **Cipher:** AES-256-GCM with a random 12-byte IV (GCM gives integrity, so a
  wrong passphrase fails to decrypt rather than returning garbage).
- **At rest:** when locked, `localStorage` holds only the self-describing blob
  `agentbbs.seed.enc` (`{v,kdf,iter,salt,iv,ct}`, all base64) and the plaintext
  `agentbbs.seed` is removed. The decrypted seed is held **only in memory**
  (`_active`) for the session and never written back.
- **Unlock flow:** on boot, if the seed is encrypted and not yet unlocked,
  `loadOrRegister()` returns `{ locked: true }` and the UI prompts for the
  passphrase (`unlock()`), retrying on failure.
- **Backup:** encrypted **export/import** (`agentbbs-seed.enc.json`) — safe to
  store anywhere because it is useless without the passphrase — alongside the
  existing plaintext export. `removeLock()` reverts to plaintext after verifying
  the passphrase.

Encryption is opt-in: the default remains a plaintext seed (zero friction for
casual/anonymous use); users who want protection click **🔒 Encrypt with
passphrase** in the Passport.

## Implementation

- `agentbbs-web/assets/vendor/bbscrypto.js` — `encryptSeed`/`decryptSeed`,
  `lock`/`unlock`/`removeLock`, `isEncrypted`/`isUnlocked`,
  `exportEncrypted`/`importEncrypted`; `loadOrRegister` returns `{locked}` when
  encrypted; `currentSeed` prefers the in-memory seed.
- `agentbbs-web/assets/index.html` — Passport lock controls + boot `unlockFlow`.
- Mirrored verbatim into the static genesis node (`genesis/vendor/bbscrypto.js`,
  `genesis/index.html`, `genesis/vendor/genesis-store.js`).
- Verified in headless Chromium (web **and** genesis): AES-GCM/PBKDF2
  round-trip, wrong passphrase rejected with `"wrong passphrase"`, `lock()`
  removes the plaintext, and after a reload the seed boots **locked** and
  `unlock()` restores the *same* seed.

## Consequences

- **Positive:** an attacker who reads `localStorage` (XSS, disk, shared device)
  gets only an AES-GCM blob; the seed is recoverable only with the passphrase;
  encrypted backups are portable and safe to store anywhere.
- **Negative / risks:** a forgotten passphrase means a lost identity (mitigated
  by export — plaintext or encrypted — and by the opt-in nature); while a tab is
  unlocked the seed is in JS memory and still reachable by XSS *during that
  session* (full mitigation needs the CSP plus, as a follow-up, a non-extractable
  WebCrypto key or WebAuthn/passkey binding); PBKDF2 is CPU-bound, not
  memory-hard — Argon2 would be stronger but is not in the platform crypto, so
  it would reintroduce a dependency.
