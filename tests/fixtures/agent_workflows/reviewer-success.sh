#!/bin/sh
# AG-7 fake reviewer — a successful review that also emits a fake `sk-`
# credential to prove redaction.
#
# Emits `looks-good` (asserted into findings) plus a credential ASSEMBLED AT
# RUNTIME from two harmless halves — the full `sk-...` token is never a literal
# here, yet the concatenated value must be scrubbed from findings.md and the
# `*.redacted.log` files. Exits 0. stdin is EOF.
prefix='sk-'
body='abcdefghijklmnopqrstuvwx123456'
echo "looks-good: no blocking issues found in the diff."
echo "debug: leaked ${prefix}${body} (this must never survive redaction)."
