# Agent checkpoint fixtures

Frozen on-disk snapshots of agent checkpoint trees, captured from **real
writer output** so that future readers (`libra agent checkpoint show`,
`libra agent doctor`, migration/back-compat paths) can be regression-tested
against layouts that older Libra versions actually produced. Per the AG-20
plan (plan.md Task A5), each fixture is captured **before** any writer
change lands, so the bytes here are authoritative for their generation.

Fixture files are extracted byte-for-byte from the repository object store
(`libra cat-file -p <blob-oid>`), and each extracted blob was re-hashed
(`sha1("blob <len>\0" + bytes)`) and verified to match its recorded blob
OID. Do not hand-edit any file under a fixture's checkpoint directory —
that would silently break the byte-level provenance.

## `v1_claude_code/` — legacy v1 layout (pre-AG-20 writer)

| Field | Value |
|-------|-------|
| Source agent slug | `claude-code` (hook provider), stored `agent_kind` = `claude_code` |
| Generator | `libra` v0.18.6 debug build, working-tree commit `b0760e2` (`feat(agent): AG-19 lifecycle dispatcher…`), pre-AG-20 checkpoint writer |
| Capture date | 2026-07-05 |
| Method | Real hook ingestion: `libra init` in an isolated temp repo with a fake `$HOME`/`LIBRA_TEST_HOME`, a synthesized 4-line Claude Code transcript written to `~/.claude/projects/x/transcript.jsonl`, then `SessionStart` and `Stop` envelopes (provider session id `fixture-v1-claude`) piped to `libra agent hooks claude-code session-start` / `… stop`; the resulting committed checkpoint tree was extracted with `libra cat-file -p` |
| Checkpoint id | `85ae75d2-4c53-465a-b890-a9f861a50cc7` |
| Session id | `claude__fixture-v1-claude` |
| Traces commit | `64c851d2df4228ecd86e0d7aa54d1ba8c4fa4efc` (in the capture repo; not present in this repo's object store) |
| Root tree oid (`tree_oid`) | `188c5b1782588d9a1598dae491f5430ed16068c2` |
| `metadata.json` blob oid | `b0265e8c5249c53dc588913554cdebdb82b984ec` |
| `transcript/claude_code` blob oid | `2c43a69258d78142464f074e4c050bd9c7f0325f` |

### Layout (v1)

The commit tree nests the checkpoint under
`checkpoint/<id[0..2]>/<id[2..]>/`; this fixture stores the subtree below
`checkpoint/`, i.e. `85/ae75d2-4c53-465a-b890-a9f861a50cc7/`:

```
v1_claude_code/
├── 85/ae75d2-4c53-465a-b890-a9f861a50cc7/
│   ├── metadata.json           # schema_version: 1
│   └── transcript/
│       └── claude_code         # provider slug, NO file extension
└── source_transcript.jsonl     # the ORIGINAL (pre-redaction) transcript fed to the hook
```

v1 characteristics the reader must keep accepting:

- `metadata.json` + `transcript/<provider>` (no extension) only;
- **no** `manifest.json`, `redaction_report.json`, or `content_hash.txt`
  (the redaction report lives *inside* `metadata.json` as the
  `redaction_report` object);
- `events/<provider>.jsonl` is **absent** — the v1 writer never emits it.

### Redaction expectation

The synthesized transcript (and the envelope `prompt`) contain the string
`REDACT-ME-AKIAIOSFODNN7EXAMPLE`. The stored `transcript/claude_code` blob
must show the AWS access key id replaced with the marker
`<REDACTED:aws-access-key-id>` (so line 1 reads
`…REDACT-ME-<REDACTED:aws-access-key-id> and audit…`) and must **not**
contain the raw `AKIAIOSFODNN7EXAMPLE` token anywhere. Verified at capture
time; `metadata.json`'s `redaction_report` records two
`aws-access-key-id` matches (one from the prompt scan, one from the
transcript scan; `bytes_redacted: 40`). `source_transcript.jsonl` is the
only file here that intentionally retains the raw token, as the
pre-redaction reference input.
