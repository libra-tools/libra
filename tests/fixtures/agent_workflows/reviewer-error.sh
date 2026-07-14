#!/bin/sh
# AG-23 fake investigator/reviewer — a hard failure.
#
# Writes a diagnostic to stderr and exits non-zero, so the run pauses as
# `agent_failure` with a retry `detail` recorded on the pending turn. stdin
# is EOF.
echo "reviewer exploded: unable to open workspace snapshot" >&2
exit 3
