#!/bin/sh
# Fake reviewer: fails with a diagnostic on stderr and a non-zero exit
# code (per-reviewer outcome `failed`, exit code 3).
#
# POSIX sh builtins only; runs under env_clear with an empty environment.
printf '%s\n' 'reviewer exploded: unable to parse the review target scope' >&2
exit 3
