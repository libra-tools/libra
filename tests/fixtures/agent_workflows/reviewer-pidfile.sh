#!/bin/sh
# AG-7 fake reviewer — records its PID then blocks, to prove cancel reaps the
# reviewer process.
#
# Invoked as `reviewer-pidfile.sh <pidfile> <prompt>`: writes its own PID to
# <pidfile> then `exec sleep` (so the recorded PID IS the sleeping process the
# runner must SIGKILL on cancel). stdin is EOF.
echo $$ > "$1"
exec sleep 300
