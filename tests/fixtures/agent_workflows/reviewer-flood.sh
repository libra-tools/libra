#!/bin/sh
# AG-7 fake reviewer — floods stdout well past the 64 KiB per-sink cap.
#
# The line count is the first positional arg when it is a positive integer,
# else a default of 16384 (~1 MiB). Two uses:
#   - sink test (no numeric arg → default ~1 MiB): the sink must truncate this
#     and append the truncation marker without starving a quiet sibling;
#   - cancel test (`… 2000000 <prompt>` → ~130 MiB): a sustained multi-second
#     flood so a cancel at ~500ms lands mid-flood with the pipe full.
# The launcher appends the prompt as the final argument; stdin is EOF.
count=16384
case "$1" in
	'' | *[!0-9]*) ;; # not a positive integer → keep the default
	*) count="$1" ;;  # all digits → honor the requested line count
esac
line='flood-0123456789abcdefghijklmnopqrstuvwxyz0123456789abcdef0123456789'
i=0
while [ "$i" -lt "$count" ]; do
	printf '%s\n' "$line"
	i=$((i + 1))
done
