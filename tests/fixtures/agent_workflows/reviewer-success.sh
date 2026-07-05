#!/bin/sh
# Fake reviewer: succeeds and prints markdown findings on stdout.
#
# The credential below is a FAKE value assembled at runtime (never a
# token-shaped literal in this file) so secret scanners stay quiet; the
# test asserts the assembled value never survives the redaction pipeline
# into findings.md or the redacted reviewer logs.
#
# POSIX sh builtins only; runs under env_clear with an empty environment.
printf '%s\n' '## findings'
printf '%s\n' '- looks-good: no blocking issues found'
printf -- '- fake credential for redaction proof: sk-%s\n' 'abcdefghijklmnopqrstuvwx123456'
printf -- '- ansi smuggle attempt: \033[31mnot-really-red\033[0m\n'
exit 0
