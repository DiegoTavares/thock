#!/usr/bin/env node
// Seals the download gate: encrypts the release-manifest URL under an invite
// code and writes public/gate.json. The page at /download derives the same
// key from the code the tester types and decrypts it in the browser, so the
// site stays static — no backend holds the code.
//
//   node seal.mjs                 # generates a fresh invite code, prints it
//   node seal.mjs THOCK-XXXX-...  # re-seals under an existing code
//
// MANIFEST_URL overrides what gets sealed (default: the stable channel).
// Rotating the code is: run this, redeploy, email the new code.

import { webcrypto as crypto } from "node:crypto";
import { writeFileSync } from "node:fs";

const MANIFEST_URL =
  process.env.MANIFEST_URL ??
  "https://storage.googleapis.com/thock-releases/channels/stable.json";
// PBKDF2 rounds. gate.json is public, so the code has to survive an offline
// guess; 600k rounds is OWASP's 2023 floor for SHA-256 and takes ~0.3 s in a
// browser — felt once, on submit.
const ITERATIONS = 600_000;
// Base32 without the look-alikes (0/O, 1/I/L), 16 characters → 80 bits.
const ALPHABET = "23456789ABCDEFGHJKMNPQRSTUVWXYZ";

function generateCode() {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  const chars = Array.from(bytes, (b) => ALPHABET[b % ALPHABET.length]);
  return "THOCK-" + chars.join("").replace(/(.{4})(?=.)/g, "$1-");
}

// Must match normalizeCode() in public/download.html.
function normalizeCode(code) {
  return code.replace(/[\s-]+/g, "").toUpperCase();
}

const base64 = (bytes) => Buffer.from(bytes).toString("base64");

async function seal(code, plaintext) {
  const salt = crypto.getRandomValues(new Uint8Array(16));
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const material = await crypto.subtle.importKey(
    "raw", new TextEncoder().encode(normalizeCode(code)), "PBKDF2", false, ["deriveKey"],
  );
  const key = await crypto.subtle.deriveKey(
    { name: "PBKDF2", hash: "SHA-256", salt, iterations: ITERATIONS },
    material, { name: "AES-GCM", length: 256 }, false, ["encrypt"],
  );
  const ciphertext = await crypto.subtle.encrypt(
    { name: "AES-GCM", iv }, key, new TextEncoder().encode(plaintext),
  );
  return {
    kdf: "PBKDF2-SHA256", iterations: ITERATIONS,
    salt: base64(salt), iv: base64(iv), ciphertext: base64(new Uint8Array(ciphertext)),
  };
}

const code = process.argv[2] ?? generateCode();
const blob = await seal(code, JSON.stringify({ manifest: MANIFEST_URL }));
const target = new URL("./public/gate.json", import.meta.url);
writeFileSync(target, JSON.stringify(blob, null, 2) + "\n");

console.log(`Sealed ${MANIFEST_URL}`);
console.log(`   into ${target.pathname}`);
console.log(`Invite code: ${code}`);
console.log("Redeploy the site, then send the code to approved testers.");
