#!/bin/sh
# Fake reviewer: records its own PID to the file named by $1, then
# `exec`s /bin/sleep so the recorded PID *is* the long-running process.
# The cancel test asserts this PID is no longer alive (kill -0 fails)
# after `review cancel` releases the run's reviewer processes.
#
# /bin/sleep by absolute path: these fixtures run under env_clear with
# an empty environment (no PATH).
printf '%s\n' "$$" > "$1"
exec /bin/sleep 300
