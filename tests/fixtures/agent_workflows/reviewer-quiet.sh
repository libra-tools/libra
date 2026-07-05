#!/bin/sh
# Fake reviewer: prints a small, exactly-known finding set. Paired with
# reviewer-flood.sh to prove a flooding sibling never starves or blocks
# a quiet reviewer (its full output must persist verbatim).
#
# POSIX sh builtins only; runs under env_clear with an empty environment.
printf '%s\n' 'quiet-finding-alpha'
printf '%s\n' 'quiet-finding-beta'
exit 0
