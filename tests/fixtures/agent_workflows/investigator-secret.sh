#!/bin/sh
# AG-23 fake investigator — a CONCLUDING stance that also emits a fake `sk-`
# credential to prove redaction.
#
# The credential is ASSEMBLED AT RUNTIME from two harmless halves, so the full
# `sk-...` token is NEVER a literal in this fixture — yet the concatenated
# value the engine sees must be scrubbed out of `findings.md` and every
# `*.redacted.log`. stdin is EOF.
prefix='sk-'
body='abcdefghijklmnopqrstuvwx123456'
echo "Concluding: leak confirmed in cache.rs:42 — a credential was written to the log."
echo "Leaked token: ${prefix}${body} (this must never survive redaction)."
echo "STANCE: concluding"
