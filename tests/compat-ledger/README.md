# `tests/compat-ledger/` — the compatibility evidence ledger

This tree records, one row per migrated upstream Git test scenario, what that
scenario proves about Libra's Git compatibility. It is the deliverable of
CT-01 in [`plan-long.md`](../../docs/development/plan/plan-long.md); the
schema and the migration process are defined by
[`plan-20260729.md`](../../docs/development/plan/plan-20260729.md).

## Layout

```
tests/compat-ledger/
  <family>/<stem>.toml      evidence: one file per upstream source file
  _example/                 the worked example — not evidence
  _invalid/                 one file per rejection the guard promises
```

A directory whose name starts with `_` is fixture space. The guard skips it
when walking the ledger, so nothing under `_example/` or `_invalid/` can ever
be counted as evidence.

Each file carries one `[[scenario]]` table per upstream test:

```toml
[[scenario]]
id = "<stem>::<upstream-test-slug>"
category = "direct"
command_status = "partial"
surface_compatibility = "git-compatible"
surface_evidence = "docs/commands/diff.md#description"
reason = "…"
owner = "libra-compat"
review_date = "2026-08-08"
upstream_revision = "<40-hex pinned grit SHA>"
upstream_file = "t4001-diff-format.sh"
libra_command = "diff"
libra_surface = "--numstat"
libra_tests = ["command::diff_test::…"]     # filled in by CT3-02
```

**The field set is defined once**, in ADR-CT-03 of `plan-20260729.md`. Neither
this page nor the guard restates it — a second definition is how the field set
drifts. What follows is only what a reader needs in order to add a row.

## What the guard will not let you write

`tests/compat/compat_ledger_schema.rs` (Cargo target `compat_ledger_schema`)
enforces the schema. Three of its rules are worth knowing before you write a
row, because they are the ones that make the ledger evidence rather than
paperwork:

- **The command tier is recomputed, not read.** `command_status` must equal the
  tier `COMPATIBILITY.md`'s top-level table gives that command. You cannot
  claim a command is better supported than the matrix says it is. If the
  command is `intentionally-different`, or absent from the matrix entirely,
  the only admissible `category` is `declined`.
- **`surface_evidence` must actually mention the surface.** It resolves to real
  text — a `COMPATIBILITY.md:<line>`, a `docs/commands/<cmd>.md#<section>`, or
  a `_compatibility.md#D<n>` section — and that text must contain both the
  `libra_command` and the `libra_surface` literally. A citation that does not
  discuss the flag is not a citation.
- **The field set is closed.** An unknown key is an error, not a note. Put
  narrative in `reason` (≤ 200 characters) or in a TOML comment.

`declined` rows must name a resolvable `decision_id` (a `### D<n>` heading in
`docs/development/commands/_compatibility.md`); `blocked` rows must carry a
non-empty `blocked_by`. No row may contain an absolute path from someone's
machine.

## Running the guard

```bash
source .env.test && LIBRA_SKIP_WEB_BUILD=1 cargo test --test compat_ledger_schema
```

An empty ledger passes: the tree is introduced before the evidence, and having
no rows yet is not a failure.

## Machine-readable dumps

Five tests print the ledger in line-oriented form for later cards to consume
(`cargo test --test compat_ledger_schema -- --nocapture`), each line prefixed
by its kind: `SCENARIO_ID`, `DIRECT_ID`, `LIBRA_TEST`, `EVIDENCE`,
`SCENARIO_TEST`. They exist so downstream gates parse a TOML-parsed dump
instead of grepping the files, which is how a "`grep '^id = '`" style check
silently misses rows.
