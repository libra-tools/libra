#!/bin/sh
# Fake reviewer: floods stdout with $1 lines (default 16384) of 64
# payload characters each — the default is ~1.06 MiB, far past the
# 64 KiB per-sink cap — then exits 0. Generated at run time so no
# megabyte blob lives in the repository (agent.md fixture size rule).
#
# POSIX sh builtins only; runs under env_clear with an empty environment.
count="${1:-16384}"
i=0
while [ "$i" -lt "$count" ]; do
    printf '%s\n' 'flood-0123456789abcdef0123456789abcdef0123456789abcdef0123456789'
    i=$((i+1))
done
exit 0
