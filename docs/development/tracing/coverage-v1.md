# Coverage v1 — canonical turn form and digest protocol

> plan-20260713 ADR-DR-08 / ADR-DR-12 / DR-05c-0. `coverage_schema_version = 1`.
> Any change to the rules below (turn grouping, canonical JSON form, field
> allowlist) requires bumping the schema version; claims recorded under an
> older version are treated as **not covered** for the newer version, never
> silently reused.

Coverage v1 defines, for every captured external-agent session, how a
transcript is split into **logical turns**, which semantic content enters the
per-turn **coverage digest**, and the exact **canonical byte form** hashed.
Live hook writers, import writers (M4), and the OpenCode export bridge (M3)
MUST all produce identical `(logical_turn_key, coverage_digest)` values for
the same content, or cross-path idempotence breaks.

## 1. Pipeline position (ADR-DR-12, fixed order)

```
bounded raw read / export pipe
  → untrusted structural parse (bounded memory)
  → typed normalized turns            (this spec, §2–3)
  → typed-field redaction
  → canonical serialize + sha256      (this spec, §4–5)
  → coverage claim                    (agent_coverage_claim gate)
  → persist
```

Raw bytes are parsed but never persisted un-redacted; **redaction runs before
canonicalization**, so the digest is computed over the redacted content — the
same secret-free bytes every path can reproduce.

Persistence scope note (plan ADR-DR-04/DR-12): the allowlist-only re-encoding
rule governs (a) coverage digests/claims on every path, and (b) the per-turn
projections persisted by the import (M4) and export (M3) writers. The LIVE
checkpoint blob keeps its pre-existing baseline — the whole transcript passed
through the generic redactor (unknown fields retained in redacted form) — an
explicitly granted compatibility exception, accounted in the plan's risk
register; removing it is out of scope for this plan.

## 2. Turn grouping and `logical_turn_key`

- A **logical turn** starts at a user (human) message and extends up to, but
  not including, the next user message: user prompt, the assistant response
  text, and every tool call / tool result / tool error the assistant produced
  in between.
- `logical_turn_key` is chosen in this order:
  1. A **stable provider turn/message ID** (format-validated; e.g. a UUID the
     provider stamps on the user message). Preferred whenever present.
  2. Otherwise the **normalized turn ordinal**: the 0-based index of the turn
     in the normalized turn sequence, rendered as `ordinal:<n>`.
- **Never** raw JSONL line numbers — inserted metadata/system lines shift them
  wholesale.
- Ordinal mode MUST verify that previously covered turns' canonical prefix is
  unchanged (same digests, same order). A reorder/rewrite of already-covered
  turns moves the affected claims to `conflicted`; it is never treated as new
  turns.

## 3. Semantic records (digest allowlist)

Each turn normalizes to an ordered list of **semantic records**, preserving
source order. Exactly four record shapes exist in v1:

| shape | fields (all required unless `\|null`) |
|---|---|
| user | `{"role":"user","text":<string>}` |
| assistant | `{"role":"assistant","text":<string>}` |
| tool call | `{"call_id":<string\|null>,"input":<value>,"name":<string>,"role":"tool_call"}` |
| tool result / error | `{"call_id":<string\|null>,"content":<string>,"is_error":<bool>,"role":"tool_result"}` |

- Multiple text blocks in one message concatenate with a single `\n` in source
  order.
- Tool errors are `tool_result` records with `is_error: true`.
- **Provenance fields stay out of the digest**: model, token usage, timestamps,
  file paths, session/agent identifiers, cwd. Different sources expose these
  with different fidelity; including them would create false conflicts
  (ADR-DR-12).
- **Unknown fields are dropped**, both from the digest and from persistence.
  No "raw fragment" passthrough.

## 4. Canonical JSON form (JCS-equivalent for the v1 value domain)

Canonical bytes are the serialization of the **JSON array of the turn's
semantic records** under these rules (matching RFC 8785 for this value
domain):

1. **UTF-8** encoding; non-ASCII characters are emitted as raw UTF-8 bytes,
   not `\uXXXX` escapes.
2. **Object keys sorted** lexicographically by Unicode code point, recursively.
3. **No insignificant whitespace** anywhere.
4. **String escaping is minimal**: only `"` (`\"`), `\` (`\\`), and control
   characters below U+0020 are escaped; the two-character forms `\b \t \n \f
   \r` are used where defined, `\u00XX` (lowercase hex) otherwise.
5. **Numbers are integers only** (i64 range) in v1. Fractional or
   out-of-range numbers in semantic positions make the turn `incomplete`
   (fail-closed for digest purposes); they never enter a digest.
6. **Duplicate keys** in the source object are a parse rejection: the turn is
   marked `incomplete` (or the input fails closed, per the caller). Never
   "last one wins".
7. **Missing vs null**: an absent optional field and an explicit `null` are
   both serialized as `null` for the nullable fields in §3 (so the two
   normalize identically); non-nullable fields must be present.
8. Booleans and `null` are the literals `true` / `false` / `null`.

`coverage_digest` = lowercase-hex SHA-256 of the canonical bytes (64 chars, no
prefix).

## 5. Golden vectors

These are normative. All producers (live / import / export, any provider)
must reproduce them byte-for-byte and digest-for-digest.

### Vector 1 — user + assistant

Input (any equivalent source form):
user says `hi`, assistant replies `hello`.

Canonical bytes (65 bytes):

```
[{"role":"user","text":"hi"},{"role":"assistant","text":"hello"}]
```

sha256: `df991cd9a1ac5c12c32b8cdf0254c3dfbbf26485b505a5afc83a90d1128ebc54`

### Vector 2 — tool call with unsorted input keys + tool result

Source tool input arrives as `{"b":2,"a":"x"}` (unsorted); call id `c1`;
result content `2 matches`, not an error.

Canonical bytes (152 bytes):

```
[{"call_id":"c1","input":{"a":"x","b":2},"name":"grep","role":"tool_call"},{"call_id":"c1","content":"2 matches","is_error":false,"role":"tool_result"}]
```

sha256: `af085109b1584fd77bb3b752c1141c1c0f0f12b51a163e94b0f91ddd329460a4`

### Vector 3 — escapes and non-ASCII

User text is the single line-broken string:
`line1`, newline, `line2 "quoted" \ 中文`.

Canonical bytes (60 bytes; `中文` as raw UTF-8):

```
[{"role":"user","text":"line1\nline2 \"quoted\" \\ 中文"}]
```

sha256: `f1e76bf75df5d6b0f67a46806abb256cd6de30eaea30e45254a8a66cf5183356`

## 6. Completeness

- `complete`: the turn's semantic records were fully parsed from a
  well-formed source region.
- `incomplete`: flush timeout, truncated/corrupt tail line, export byte-cap
  truncation, duplicate-key rejection, or non-integer number — anything that
  prevented a faithful full parse. A later `complete` parse of the same
  logical turn upgrades the claim via a new `agent_coverage_revision`
  (ADR-DR-16); `complete → different complete` never auto-overwrites, it goes
  to `conflicted` / doctor.

## 7. Storage mapping

- Gate table: `agent_coverage_claim` — UNIQUE
  `(session_id, logical_turn_key, coverage_schema_version)`; `coverage_digest`
  is the content version and is **not** part of the unique key.
- History table: `agent_coverage_revision` — append-only, one row per
  committed revision; supersede relations live here, never on
  `agent_checkpoint` (a whole-transcript checkpoint may back many turns).
- Source identity persisted alongside imports is provider-root-relative
  (never an absolute home path).
