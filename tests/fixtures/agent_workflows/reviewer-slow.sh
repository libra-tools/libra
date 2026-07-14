#!/bin/sh
# AG-7 / AG-23 fake investigator/reviewer — a slow turn.
#
# Sleeps for the first positional arg (default 30s), THEN emits a late finding.
# Two uses:
#   - review (`… 1 <prompt>`): sleeps ~1s, then prints `slow-finding-after-sleep`
#     so the sink proves it captures output produced after a delay (exit 0, Ok);
#   - investigate (`… 30 <prompt>`): sleeps 30s so a cancel preempts it before
#     the late line is ever reached.
# The launcher appends the prompt as the final argument; stdin is EOF.
sleep "${1:-30}"
echo "slow-finding-after-sleep"
