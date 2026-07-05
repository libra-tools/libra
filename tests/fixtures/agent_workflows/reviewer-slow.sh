#!/bin/sh
# Fake reviewer: sleeps for $1 seconds (default 1), then prints one
# finding. Used both for "late output is still captured" (short sleep)
# and as a long-running victim for cancel/timeout scenarios (long sleep,
# killed before it ever prints).
#
# /bin/sleep is called by absolute path: the reviewer spawn contract is
# env_clear + explicit allowlist, and these fixtures run with an empty
# environment (no PATH).
sleep_secs="${1:-1}"
/bin/sleep "$sleep_secs"
printf '%s\n' 'slow-finding-after-sleep'
exit 0
