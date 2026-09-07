// Regenerate the install-smoke fixtures (plan-20260821 A1-05).
//
//   node tests/data/install-smoke/generate-fixtures.mjs
//
// Fixtures are signed with the SAME committed test-only Ed25519 keypair as
// the UP-01 contract vectors (tests/data/up01-transition-vectors-v1.json).
// The private half is intentionally public: it protects nothing, it exists
// so the smoke harness can exercise the real verification code paths.
// Deterministic: fixed timestamps, fixed contents, RFC 8032 deterministic
// signatures — rerunning must be byte-stable.

import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { webcrypto as crypto, createHash } from "node:crypto";

const TEST_PRIVATE_KEY_PKCS8_B64 =
  "MC4CAQAwBQYDK2VwBCIEIPHTtu4stYxMs50qKjn1e04Hei8LuAiJ5LorBvbK0ici";
const KEY_ID = "libra-release-test-1";
const DOMAIN = "libra-upgrade-manifest-v1\0";
const PLATFORMS = ["linux-amd64", "linux-arm64", "darwin-arm64", "windows-amd64"];
const VERSION = "9.9.9";

const root = dirname(fileURLToPath(import.meta.url));

function sha256Hex(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

async function sign(payloadBytes) {
  const key = await crypto.subtle.importKey(
    "pkcs8",
    Buffer.from(TEST_PRIVATE_KEY_PKCS8_B64, "base64"),
    { name: "Ed25519" },
    false,
    ["sign"],
  );
  const message = Buffer.concat([Buffer.from(DOMAIN, "binary"), payloadBytes]);
  return Buffer.from(await crypto.subtle.sign("Ed25519", key, message));
}

async function envelopeFromBytes(payloadBytes, { pretty = false } = {}) {
  const signature = await sign(payloadBytes);
  const doc = {
    schema_version: 1,
    payload: payloadBytes.toString("base64"),
    signatures: [{ key_id: KEY_ID, signature: signature.toString("base64") }],
  };
  // The PAYLOAD is always the canonical compact serialization (it is
  // signature-bound); the ENVELOPE spelling is free — pretty-printed
  // envelopes must verify identically.
  return `${JSON.stringify(doc, null, pretty ? 2 : undefined)}\n`;
}

async function envelope(payload, opts = {}) {
  return envelopeFromBytes(Buffer.from(JSON.stringify(payload)), opts);
}

function payloadFor(binaryByPlatform, mutate = {}) {
  const version = mutate.version ?? VERSION;
  return {
    channel: "stable",
    version,
    control_revision: 3,
    published_at: mutate.published_at ?? "2026-01-01T00:00:00.000Z",
    expires_at: mutate.expires_at ?? "2027-12-31T00:00:00.000Z",
    min_key_generation: mutate.min_key_generation ?? 1,
    paused: mutate.paused ?? false,
    revoked_versions: mutate.revoked_versions ?? [],
    artifacts: PLATFORMS.map((platform) => ({
      platform,
      url: `https://download.libra.tools/libra/releases/v${version}/libra-${platform}`,
      sha256: mutate.sha256 ?? sha256Hex(binaryByPlatform[platform]),
      size:
        mutate.size ??
        binaryByPlatform[platform].length + (mutate.sizeDelta ?? 0),
    })),
  };
}

function writeFixture(path, content) {
  mkdirSync(dirname(join(root, path)), { recursive: true });
  writeFileSync(join(root, path), content);
  console.log(`wrote ${path}`);
}

const binaries = Object.fromEntries(
  PLATFORMS.map((platform) => [
    platform,
    Buffer.from(`libra install-smoke fixture binary ${VERSION} ${platform}\n`),
  ]),
);

// Artifact tree shared by the signed scenarios.
for (const platform of PLATFORMS) {
  writeFixture(
    `fixtures/tree/libra/releases/v${VERSION}/libra-${platform}`,
    binaries[platform],
  );
}

// Legacy-fallback tree (v9.9.8): binary + .sha256 sidecar, no manifest.
// The Windows installer's legacy path downloads the `.exe` asset name.
const legacy = Buffer.from("libra install-smoke legacy binary 9.9.8\n");
for (const platform of PLATFORMS) {
  writeFixture(`fixtures/tree/libra/releases/v9.9.8/libra-${platform}`, legacy);
  writeFixture(
    `fixtures/tree/libra/releases/v9.9.8/libra-${platform}.sha256`,
    `${sha256Hex(legacy)}  libra-${platform}\n`,
  );
}
writeFixture(
  "fixtures/tree/libra/releases/v9.9.8/libra-windows-amd64.exe",
  legacy,
);
// Mirrors the GitHub API's pretty-printed shape: install.sh's legacy-path
// extraction expects `"tag_name": "..."` with a space after the colon.
writeFixture(
  "fixtures/tree/repos/libra-tools/libra/releases/latest",
  `${JSON.stringify({ tag_name: "v9.9.8" }, null, 2)}\n`,
);

// Scenario manifests.
writeFixture("fixtures/manifest-valid.json", await envelope(payloadFor(binaries)));

const badSignature = JSON.parse(await envelope(payloadFor(binaries)));
const sigBytes = Buffer.from(badSignature.signatures[0].signature, "base64");
sigBytes[0] ^= 0xff;
badSignature.signatures[0].signature = sigBytes.toString("base64");
writeFixture(
  "fixtures/manifest-bad-signature.json",
  `${JSON.stringify(badSignature)}\n`,
);

writeFixture(
  "fixtures/manifest-sha-mismatch.json",
  await envelope(payloadFor(binaries, { sha256: "f".repeat(64) })),
);
// Signed size LARGER than the artifact (a too-small signed size is cut off
// by the bounded download instead — that is the undersized scenario).
writeFixture(
  "fixtures/manifest-size-mismatch.json",
  await envelope(payloadFor(binaries, { sizeDelta: 4096 })),
);

// Policy branches: each properly SIGNED, so only the policy check can refuse.
// Expired: published < expires, both inside the test key window, but the
// expiry itself is in the past — only the expiry check can refuse it.
writeFixture(
  "fixtures/manifest-expired.json",
  await envelope(payloadFor(binaries, { expires_at: "2026-02-01T00:00:00.000Z" })),
);
writeFixture(
  "fixtures/manifest-paused.json",
  await envelope(payloadFor(binaries, { paused: true })),
);
writeFixture(
  "fixtures/manifest-revoked.json",
  await envelope(payloadFor(binaries, { revoked_versions: [VERSION] })),
);
// Signed and unexpired but older than the installer's pinned baseline
// (the harness rewrites DEFAULT_VERSION to v9.9.8): the anti-replay floor.
writeFixture(
  "fixtures/manifest-stale-replay.json",
  await envelope(payloadFor(binaries, { version: "1.0.0" })),
);

// Round-2 policy branches (all properly signed with the test key):
// artifact size 0 must be refused at parse time, before any download.
writeFixture(
  "fixtures/manifest-zero-size.json",
  await envelope(payloadFor(binaries, { size: 0 })),
);
// min_key_generation above the installer's pinned key generation (1).
writeFixture(
  "fixtures/manifest-future-min-key.json",
  await envelope(payloadFor(binaries, { min_key_generation: 2 })),
);
// Signed lifetime beyond the test key's not_after (2028-01-01): the pinned
// key window check must refuse it even though it is not yet expired.
writeFixture(
  "fixtures/manifest-key-window.json",
  await envelope(payloadFor(binaries, { expires_at: "2029-01-01T00:00:00.000Z" })),
);
// Properly signed but NOT the canonical top-level serialization (an unknown
// field is injected before "artifacts"): the structural grammar gate must
// refuse it before trusting any extracted scalar.
{
  const canonical = payloadFor(binaries);
  const injected = {
    channel: canonical.channel,
    version: canonical.version,
    control_revision: canonical.control_revision,
    published_at: canonical.published_at,
    expires_at: canonical.expires_at,
    min_key_generation: canonical.min_key_generation,
    paused: canonical.paused,
    revoked_versions: canonical.revoked_versions,
    metadata: { version: "0.0.1" },
    artifacts: canonical.artifacts,
  };
  writeFixture("fixtures/manifest-noncanonical.json", await envelope(injected));
}

// Round-3 branches (all signed with the test key unless noted):
// impossible calendar date — field ranges pass, the calendar check must not.
writeFixture(
  "fixtures/manifest-bad-calendar.json",
  await envelope(
    payloadFor(binaries, { published_at: "2026-09-31T00:00:00.000Z" }),
  ),
);
// min_key_generation wider than the bounded nine-digit numeric grammar: the
// structural gate must refuse it before any shell integer comparison.
writeFixture(
  "fixtures/manifest-huge-min-key.json",
  await envelope(payloadFor(binaries, { min_key_generation: 10000000000 })),
);
// A field TRAILING the artifacts array shaped like an artifact row: the
// full-payload grammar's end anchor must refuse it.
writeFixture(
  "fixtures/manifest-trailing-artifact.json",
  await envelope({
    ...payloadFor(binaries),
    // Spread preserves insertion order, so this serializes AFTER artifacts.
    metadata: {
      platform: "linux-amd64",
      url: "https://download.libra.tools/libra/releases/v0.0.1/libra-linux-amd64",
      sha256: "0".repeat(64),
      size: 1,
    },
  }),
);
// Pretty-printed ENVELOPE around the canonical compact payload: must verify
// and install identically (the envelope spelling is not signature-bound).
writeFixture(
  "fixtures/manifest-pretty-envelope.json",
  await envelope(payloadFor(binaries), { pretty: true }),
);
// Signed size one byte SMALLER than the served artifact: the bounded
// download must cut off / refuse instead of accepting extra bytes.
writeFixture(
  "fixtures/manifest-undersized.json",
  await envelope(payloadFor(binaries, { sizeDelta: -1 })),
);

// Round-4 branches:
// SIGNED multi-line payload — a fully canonical first line plus a trailing
// artifact row on a second line. Line-oriented tools would see a valid first
// line; the printable-ASCII single-line gate must refuse the whole payload.
writeFixture(
  "fixtures/manifest-multiline-payload.json",
  await envelopeFromBytes(
    Buffer.from(
      `${JSON.stringify(payloadFor(binaries))}\n` +
        `{"platform":"linux-arm64","url":"https://download.libra.tools/libra/releases/v9.9.9/libra-linux-arm64","sha256":"${"0".repeat(64)}","size":1}`,
    ),
  ),
);
// SemVer component wider than the bounded nine-digit grammar: shell integer
// comparison could overflow, so the canonical check must refuse it first.
writeFixture(
  "fixtures/manifest-huge-semver.json",
  await envelope(payloadFor(binaries, { version: "99999999999.0.0" })),
);

// Valid signature over the ORIGINAL payload, but the payload was swapped
// afterwards: byte-level tamper distinct from the flipped-signature case.
const tampered = JSON.parse(await envelope(payloadFor(binaries)));
const tamperedPayload = JSON.parse(
  Buffer.from(tampered.payload, "base64").toString(),
);
tamperedPayload.paused = false;
tamperedPayload.version = "9.9.10";
tampered.payload = Buffer.from(JSON.stringify(tamperedPayload)).toString("base64");
writeFixture(
  "fixtures/manifest-tampered-payload.json",
  `${JSON.stringify(tampered)}\n`,
);
